//! Trabajo en segundo plano: latido con el servidor, programación y copias.
//!
//! Dos hilos: el de control (latido cada minuto, revisa cada 15 s qué toca y
//! atiende las órdenes de la interfaz) y el de copias (una a la vez). La
//! interfaz solo lee `Shared` y envía `Command`.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Local};
use sha2::{Digest, Sha256};

use crate::api::{ApiClient, DeviceState, Heartbeat, Run, Schedule, is_auth_error};
use crate::config::{Config, machine_info};
use crate::schedule;
use crate::store::{self, Entry, Index};

const HEARTBEAT_EVERY: Duration = Duration::from_secs(60);
const CHECK_EVERY: Duration = Duration::from_secs(15);
const BUILD_TIMEOUT: Duration = Duration::from_secs(45 * 60);

#[derive(Debug, Clone, PartialEq)]
pub enum Stage {
    Waiting,
    Building,
    Downloading,
    Verifying,
}

#[derive(Debug, Clone)]
pub struct Progress {
    pub shop_id: i64,
    pub shop_name: String,
    pub stage: Stage,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub started: Instant,
}

#[derive(Debug, Clone)]
pub struct Notice {
    pub text: String,
    pub ok: bool,
    pub at: Instant,
}

#[derive(Default)]
pub struct Shared {
    pub state: Option<DeviceState>,
    pub connected: bool,
    pub last_sync: Option<DateTime<Local>>,
    pub last_error: Option<String>,
    /// El servidor ya no acepta el token: hay que volver a iniciar sesión.
    pub auth_lost: Option<String>,
    pub current: Option<Progress>,
    pub queued: Vec<i64>,
    pub runs: Vec<Run>,
    pub index: Index,
    pub disk: Option<(u64, u64)>,
    pub notices: Vec<Notice>,
    pub last_attempt: HashMap<i64, DateTime<Local>>,
    /// Última copia fallida (tienda y hora), hasta que una copia de esa tienda salga bien.
    pub last_failure: Option<(i64, String, DateTime<Local>)>,
}

impl Shared {
    pub fn notify(&mut self, text: impl Into<String>, ok: bool) {
        self.notices.push(Notice {
            text: text.into(),
            ok,
            at: Instant::now(),
        });
    }

    pub fn shop_name(&self, shop_id: i64) -> String {
        self.state
            .as_ref()
            .and_then(|s| s.shops.iter().find(|sh| sh.id == shop_id))
            .map(|s| s.name.clone())
            .unwrap_or_else(|| format!("Tienda #{shop_id}"))
    }

    pub fn schedule_for(&self, shop_id: i64) -> Schedule {
        self.state
            .as_ref()
            .and_then(|s| s.schedules.iter().find(|sc| sc.shop_id == shop_id))
            .cloned()
            .unwrap_or_else(|| Schedule {
                shop_id,
                ..Default::default()
            })
    }
}

pub type SharedRef = Arc<Mutex<Shared>>;

pub enum Command {
    Refresh,
    RunNow(i64),
    SaveSchedule(Schedule),
    FolderChanged,
    Stop,
}

#[derive(Debug, Clone)]
pub struct Job {
    pub shop_id: i64,
    pub run_id: Option<i64>,
    pub trigger: &'static str,
}

pub struct Worker {
    tx: Sender<Command>,
}

impl Worker {
    pub fn start(
        api: ApiClient,
        config: Arc<Mutex<Config>>,
        shared: SharedRef,
        ctx: egui::Context,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<Command>();
        let (job_tx, job_rx) = mpsc::channel::<Job>();

        if let Some(dir) = config.lock().unwrap().backup_dir.clone() {
            let mut s = shared.lock().unwrap();
            s.index = Index::load(&dir);
            s.disk = store::disk_space(&dir);
        }

        {
            let (api, config, shared, ctx, tx) = (
                api.clone(),
                config.clone(),
                shared.clone(),
                ctx.clone(),
                tx.clone(),
            );
            thread::Builder::new()
                .name("agest-backup-jobs".into())
                .spawn(move || job_loop(api, config, shared, ctx, job_rx, tx))
                .expect("hilo de copias");
        }
        thread::Builder::new()
            .name("agest-backup-control".into())
            .spawn(move || control_loop(api, config, shared, ctx, rx, job_tx))
            .expect("hilo de control");

        Self { tx }
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Stop);
    }
}

fn lost_auth(shared: &SharedRef, ctx: &egui::Context, err: &anyhow::Error) -> bool {
    if is_auth_error(err) {
        shared.lock().unwrap().auth_lost = Some(err.to_string());
        ctx.request_repaint();
        return true;
    }
    false
}

/// Latido con el espacio en disco y las copias de la carpeta indicada.
pub fn build_heartbeat(dir: Option<&Path>) -> (Heartbeat, Option<(u64, u64)>, Index) {
    let (host, os) = machine_info();
    let (disk, index) = match dir {
        Some(d) => (store::disk_space(d), Index::load(d)),
        None => (None, Index::default()),
    };
    let hb = Heartbeat {
        backup_path: dir.map(|d| d.display().to_string()),
        disk_total_bytes: disk.map(|d| d.0),
        disk_free_bytes: disk.map(|d| d.1),
        backups_bytes: index.total_bytes(),
        backups_count: index.entries.len() as u64,
        app_version: env!("CARGO_PKG_VERSION").into(),
        os,
        hostname: host,
    };
    (hb, disk, index)
}

fn heartbeat(api: &ApiClient, config: &Arc<Mutex<Config>>, shared: &SharedRef) -> Result<()> {
    let dir = config.lock().unwrap().backup_dir.clone();
    let (hb, disk, index) = build_heartbeat(dir.as_deref());
    let state = api.heartbeat(&hb)?;
    let runs = api.runs(100).unwrap_or_default();
    let mut s = shared.lock().unwrap();
    s.state = Some(state);
    s.runs = runs;
    s.index = index;
    s.disk = disk;
    s.connected = true;
    s.last_error = None;
    s.last_sync = Some(Local::now());
    Ok(())
}

fn enqueue(shared: &SharedRef, job_tx: &Sender<Job>, job: Job) -> bool {
    let mut s = shared.lock().unwrap();
    let busy = s.current.as_ref().is_some_and(|c| c.shop_id == job.shop_id);
    if busy || s.queued.contains(&job.shop_id) {
        return false;
    }
    s.queued.push(job.shop_id);
    drop(s);
    job_tx.send(job).is_ok()
}

/// Copias programadas que tocan y copias pedidas desde el panel.
fn check_due(config: &Arc<Mutex<Config>>, shared: &SharedRef, job_tx: &Sender<Job>) {
    if config.lock().unwrap().backup_dir.is_none() {
        return;
    }
    let (pending, schedules, last_success, last_attempt) = {
        let s = shared.lock().unwrap();
        let Some(state) = &s.state else { return };
        let mut last_success = s.index.last_success();
        // también cuentan las completadas que el servidor conoce (p. ej. tras cambiar de carpeta)
        for run in s
            .runs
            .iter()
            .filter(|r| r.status == "completed" && r.pruned_at.is_none())
        {
            if let Some(t) = run
                .finished_at
                .as_deref()
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            {
                let t = t.with_timezone(&Local);
                last_success
                    .entry(run.shop_id)
                    .and_modify(|v| *v = (*v).max(t))
                    .or_insert(t);
            }
        }
        (
            state.pending_runs.clone(),
            state.schedules.clone(),
            last_success,
            s.last_attempt.clone(),
        )
    };

    for run in pending {
        enqueue(
            shared,
            job_tx,
            Job {
                shop_id: run.shop_id,
                run_id: Some(run.id),
                trigger: "panel",
            },
        );
    }

    let now = Local::now();
    for sc in schedules {
        if schedule::is_due(
            &sc,
            last_success.get(&sc.shop_id).copied(),
            last_attempt.get(&sc.shop_id).copied(),
            now,
        ) {
            enqueue(
                shared,
                job_tx,
                Job {
                    shop_id: sc.shop_id,
                    run_id: None,
                    trigger: "scheduled",
                },
            );
        }
    }
}

fn control_loop(
    api: ApiClient,
    config: Arc<Mutex<Config>>,
    shared: SharedRef,
    ctx: egui::Context,
    rx: Receiver<Command>,
    job_tx: Sender<Job>,
) {
    let mut last_beat: Option<Instant> = None;
    let mut last_check = Instant::now() - CHECK_EVERY;

    loop {
        if last_beat.is_none_or(|t| t.elapsed() >= HEARTBEAT_EVERY) {
            last_beat = Some(Instant::now());
            if let Err(e) = heartbeat(&api, &config, &shared) {
                if lost_auth(&shared, &ctx, &e) {
                    return;
                }
                let mut s = shared.lock().unwrap();
                s.connected = false;
                s.last_error = Some(e.to_string());
            }
            ctx.request_repaint();
        }

        if last_check.elapsed() >= CHECK_EVERY {
            last_check = Instant::now();
            check_due(&config, &shared, &job_tx);
            ctx.request_repaint();
        }

        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(Command::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            Ok(Command::Refresh) => last_beat = None,
            Ok(Command::FolderChanged) => {
                if let Some(dir) = config.lock().unwrap().backup_dir.clone() {
                    let mut s = shared.lock().unwrap();
                    s.index = Index::load(&dir);
                    s.disk = store::disk_space(&dir);
                }
                last_beat = None;
            }
            Ok(Command::RunNow(shop_id)) => {
                if config.lock().unwrap().backup_dir.is_none() {
                    shared
                        .lock()
                        .unwrap()
                        .notify("Elige primero la carpeta de copias.", false);
                } else if !enqueue(
                    &shared,
                    &job_tx,
                    Job {
                        shop_id,
                        run_id: None,
                        trigger: "manual",
                    },
                ) {
                    shared
                        .lock()
                        .unwrap()
                        .notify("Esa tienda ya tiene una copia en curso.", false);
                }
                ctx.request_repaint();
            }
            Ok(Command::SaveSchedule(sc)) => {
                match api.save_schedule(&sc) {
                    Ok(_) => {
                        shared
                            .lock()
                            .unwrap()
                            .notify("Programación guardada.", true);
                        last_beat = None;
                    }
                    Err(e) => {
                        if lost_auth(&shared, &ctx, &e) {
                            return;
                        }
                        shared
                            .lock()
                            .unwrap()
                            .notify(format!("No se ha podido guardar: {e}"), false);
                    }
                }
                ctx.request_repaint();
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

fn job_loop(
    api: ApiClient,
    config: Arc<Mutex<Config>>,
    shared: SharedRef,
    ctx: egui::Context,
    rx: Receiver<Job>,
    control: Sender<Command>,
) {
    while let Ok(job) = rx.recv() {
        let Some(dir) = config.lock().unwrap().backup_dir.clone() else {
            shared
                .lock()
                .unwrap()
                .queued
                .retain(|id| *id != job.shop_id);
            continue;
        };
        let shop_name = shared.lock().unwrap().shop_name(job.shop_id);
        {
            let mut s = shared.lock().unwrap();
            s.queued.retain(|id| *id != job.shop_id);
            s.last_attempt.insert(job.shop_id, Local::now());
            s.current = Some(Progress {
                shop_id: job.shop_id,
                shop_name: shop_name.clone(),
                stage: Stage::Waiting,
                downloaded: 0,
                total: None,
                started: Instant::now(),
            });
        }
        ctx.request_repaint();

        let result = {
            let (shared, ctx) = (shared.clone(), ctx.clone());
            run_backup(
                &api,
                &dir,
                &job,
                &shop_name,
                &mut |stage, downloaded, total| {
                    if let Some(p) = shared.lock().unwrap().current.as_mut() {
                        p.stage = stage;
                        p.downloaded = downloaded;
                        if total.is_some() {
                            p.total = total;
                        }
                    }
                    ctx.request_repaint();
                },
            )
        };
        let mut s = shared.lock().unwrap();
        s.current = None;
        match result {
            Ok(entry) => {
                if s.last_failure.as_ref().is_some_and(|f| f.0 == job.shop_id) {
                    s.last_failure = None;
                }
                s.index.add(entry.clone());
                let keep = s.schedule_for(job.shop_id).retention_count;
                let pruned = prune(&api, &dir, &mut s.index, job.shop_id, keep);
                if let Err(e) = s.index.save(&dir) {
                    s.notify(
                        format!("No se ha podido guardar el índice local: {e}"),
                        false,
                    );
                }
                s.disk = store::disk_space(&dir);
                let extra = if pruned > 0 {
                    format!(" Se han borrado {pruned} copias antiguas.")
                } else {
                    String::new()
                };
                s.notify(
                    format!(
                        "Copia de «{shop_name}» completada ({}).{extra}",
                        human_bytes(entry.size)
                    ),
                    true,
                );
            }
            Err(e) => {
                if is_auth_error(&e) {
                    s.auth_lost = Some(e.to_string());
                } else {
                    s.last_failure = Some((job.shop_id, shop_name.clone(), Local::now()));
                    s.notify(format!("La copia de «{shop_name}» ha fallado: {e}"), false);
                }
            }
        }
        drop(s);
        let _ = control.send(Command::Refresh);
        ctx.request_repaint();
    }
}

/// Borra las copias que sobran según la retención y avisa al servidor.
pub fn prune(api: &ApiClient, dir: &Path, index: &mut Index, shop_id: i64, keep: u32) -> usize {
    let mut removed = 0;
    for entry in index.to_prune(shop_id, keep) {
        let path = dir.join(&entry.file);
        if !path.exists() || std::fs::remove_file(&path).is_ok() {
            index.entries.retain(|e| e.run_id != entry.run_id);
            if entry.run_id > 0 {
                let _ = api.mark_pruned(entry.run_id);
            }
            removed += 1;
        }
    }
    removed
}

/// Progreso de una copia: etapa, bytes descargados y total (si se sabe).
pub type OnProgress<'a> = &'a mut dyn FnMut(Stage, u64, Option<u64>);

/// Una copia completa: pedirla, esperar al ZIP, descargarlo, verificarlo y confirmarla.
pub fn run_backup(
    api: &ApiClient,
    dir: &Path,
    job: &Job,
    shop_name: &str,
    on_progress: OnProgress,
) -> Result<Entry> {
    let run = match job.run_id {
        Some(id) => api.start_run(id)?,
        None => api.create_run(job.shop_id, job.trigger)?,
    };
    let run_id = run.id;
    let mut part: Option<PathBuf> = None;

    let result = (|| -> Result<Entry> {
        on_progress(Stage::Building, 0, None);
        let run = wait_ready(api, run)?;
        let expected = run
            .sha256
            .clone()
            .ok_or_else(|| anyhow!("el servidor no ha indicado el SHA-256 de la copia"))?;

        let folder = dir.join(store::shop_folder(job.shop_id, shop_name));
        std::fs::create_dir_all(&folder)
            .map_err(|e| anyhow!("no se puede escribir en la carpeta de copias: {e}"))?;
        let file_name = run.file_name.clone().unwrap_or_else(|| {
            format!(
                "agest-{}-{}.zip",
                job.shop_id,
                Local::now().format("%Y%m%d-%H%M%S")
            )
        });
        let file_name = sanitize_file_name(&file_name);
        let target = unique_path(&folder.join(&file_name));
        let tmp = target.with_extension("zip.part");
        part = Some(tmp.clone());

        if let (Some(size), Some((_, free))) = (run.size_bytes, store::disk_space(dir))
            && size > free
        {
            bail!(
                "No hay espacio suficiente en el disco ({} necesarios, {} libres).",
                human_bytes(size),
                human_bytes(free)
            );
        }

        let download = api.download(run_id)?;
        let total = download.size.or(run.size_bytes);
        on_progress(Stage::Downloading, 0, total);
        let (sha, size) = save_stream(download.reader, &tmp, |n| {
            on_progress(Stage::Downloading, n, total)
        })?;

        on_progress(Stage::Verifying, size, total);
        if !sha.eq_ignore_ascii_case(&expected)
            || download
                .sha256
                .as_deref()
                .is_some_and(|h| !h.eq_ignore_ascii_case(&sha))
        {
            bail!("La copia descargada está dañada (SHA-256 distinto). Se reintentará.");
        }
        std::fs::rename(&tmp, &target)?;
        part = None;

        let final_name = target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&file_name)
            .to_string();
        api.complete_run(run_id, &sha, &final_name)?;

        let relative = target
            .strip_prefix(dir)
            .unwrap_or(&target)
            .to_string_lossy()
            .replace('\\', "/");
        Ok(Entry {
            run_id,
            shop_id: job.shop_id,
            shop_name: shop_name.to_string(),
            file: relative,
            size,
            sha256: sha,
            completed_at: Local::now(),
        })
    })();

    if let Err(e) = &result {
        if let Some(tmp) = part {
            let _ = std::fs::remove_file(tmp);
        }
        if !is_auth_error(e) {
            let _ = api.fail_run(run_id, &e.to_string());
        }
    }
    result
}

fn wait_ready(api: &ApiClient, mut run: Run) -> Result<Run> {
    let started = Instant::now();
    loop {
        match run.status.as_str() {
            "ready" | "downloading" => return Ok(run),
            "failed" => bail!(
                run.error
                    .clone()
                    .unwrap_or_else(|| "el servidor no ha podido generar la copia".into())
            ),
            "expired" => bail!("la copia caducó en el servidor antes de descargarse"),
            "completed" => bail!("esta copia ya estaba completada"),
            _ => {}
        }
        if started.elapsed() > BUILD_TIMEOUT {
            bail!("el servidor tarda demasiado en generar la copia");
        }
        thread::sleep(Duration::from_millis(1500));
        run = api.get_run(run.id)?;
    }
}

/// Copia el flujo a disco calculando el SHA-256 sobre la marcha.
pub fn save_stream(
    mut reader: impl Read,
    path: &Path,
    mut progress: impl FnMut(u64),
) -> Result<(String, u64)> {
    let file = File::create(path)?;
    let mut writer = BufWriter::with_capacity(1 << 20, file);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut total = 0u64;
    let mut last = Instant::now();
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        writer.write_all(&buf[..n])?;
        total += n as u64;
        if last.elapsed() > Duration::from_millis(150) {
            progress(total);
            last = Instant::now();
        }
    }
    writer.flush()?;
    writer
        .into_inner()
        .map_err(|e| anyhow!(e.to_string()))?
        .sync_all()?;
    progress(total);
    let sha = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok((sha, total))
}

fn sanitize_file_name(name: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("copia.zip");
    let clean: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    if clean.ends_with(".zip") {
        clean
    } else {
        format!("{clean}.zip")
    }
}

fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("copia");
    (2..)
        .map(|i| path.with_file_name(format!("{stem}-{i}.zip")))
        .find(|p| !p.exists())
        .expect("nombre libre")
}

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if value < 10.0 {
        format!("{:.1} {}", value, UNITS[unit]).replace('.', ",")
    } else {
        format!("{:.0} {}", value, UNITS[unit])
    }
}

/// Tiendas en la cola (para la interfaz).
pub fn queued_set(shared: &Shared) -> HashSet<i64> {
    shared.queued.iter().copied().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descarga_calcula_sha256_y_tamano() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.zip.part");
        let data = vec![7u8; 600_000];
        let mut seen = 0;
        let (sha, size) = save_stream(&data[..], &path, |n| seen = n).unwrap();
        assert_eq!(size, 600_000);
        assert_eq!(seen, 600_000);
        let expected: String = Sha256::digest(&data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(sha, expected);
        assert_eq!(std::fs::read(&path).unwrap(), data);
    }

    #[test]
    fn nombres_de_fichero_seguros_y_unicos() {
        assert_eq!(sanitize_file_name("../../etc/agest 1.zip"), "agest_1.zip");
        assert_eq!(sanitize_file_name("copia"), "copia.zip");
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.zip");
        std::fs::write(&p, b"1").unwrap();
        assert_eq!(unique_path(&p), dir.path().join("a-2.zip"));
    }

    #[test]
    fn bytes_legibles() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1,5 KB");
        assert_eq!(human_bytes(50 * 1024 * 1024), "50 MB");
    }
}
