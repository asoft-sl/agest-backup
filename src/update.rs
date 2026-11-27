//! Actualización automática desde las versiones publicadas en GitHub.
//!
//! Un hilo mira cada pocas horas la última versión de
//! `github.com/asoft-sl/agest-backup/releases`. Si es más nueva que la que se
//! está ejecutando, descarga el ejecutable junto al actual (`….exe.new`) y
//! comprueba su SHA-256 con el que publica GitHub (o el `.sha256` adjunto).
//!
//! Para instalarla se renombra el ejecutable en uso a `….exe.old` (Windows
//! deja renombrar un ejecutable abierto, no sobrescribirlo), se pone el nuevo
//! en su sitio y se arranca. Se hace sola cuando la app está en la bandeja y
//! no hay ninguna copia en curso; con la ventana abierta se ofrece un botón.
//! Al salir con una versión ya descargada también se instala, para el
//! siguiente arranque. El `.old` se borra al arrancar la versión nueva.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Local};
use serde::Deserialize;

use crate::config::Config;

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const LATEST_URL: &str = "https://api.github.com/repos/asoft-sl/agest-backup/releases/latest";
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(60);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);
const RETRY_AFTER: Duration = Duration::from_secs(30 * 60);

/// Nombre del ejecutable publicado para esta plataforma (solo hay de Windows).
const ASSET: Option<&str> = if cfg!(windows) {
    Some("agest-backup.exe")
} else {
    None
};

#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Idle,
    Checking,
    UpToDate,
    Downloading {
        version: String,
        done: u64,
        total: u64,
    },
    /// Descargada y verificada: se instala al reiniciar.
    Ready {
        version: String,
        url: String,
    },
    Failed(String),
    Unsupported,
}

#[derive(Debug, Clone)]
pub struct Status {
    pub phase: Phase,
    pub last_check: Option<DateTime<Local>>,
}

static STATUS: Mutex<Status> = Mutex::new(Status {
    phase: Phase::Idle,
    last_check: None,
});
static WAKE: OnceLock<Mutex<Sender<()>>> = OnceLock::new();
static EXE: OnceLock<PathBuf> = OnceLock::new();
/// La ventana está oculta (la app vive en la bandeja).
pub static WINDOW_HIDDEN: AtomicBool = AtomicBool::new(false);
/// Este arranque viene de instalar una actualización.
pub static JUST_UPDATED: AtomicBool = AtomicBool::new(false);

pub fn status() -> Status {
    STATUS.lock().unwrap().clone()
}

fn set_phase(phase: Phase) {
    STATUS.lock().unwrap().phase = phase;
}

/// Ruta del ejecutable, guardada al arrancar: en Windows, tras renombrarlo,
/// `current_exe()` devolvería la ruta del `.old`.
pub fn exe_path() -> Option<PathBuf> {
    EXE.get().cloned()
}

fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    exe.with_file_name(name)
}

fn staged_path(exe: &Path) -> PathBuf {
    sibling(exe, ".new")
}

fn old_path(exe: &Path) -> PathBuf {
    sibling(exe, ".old")
}

/// Al arrancar: recuerda la ruta del ejecutable y borra los restos de una
/// actualización anterior (el proceso viejo puede tardar unos segundos en salir).
pub fn init(args: &[String]) {
    JUST_UPDATED.store(args.iter().any(|a| a == "--updated"), Ordering::Relaxed);
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let _ = EXE.set(exe.clone());
    let _ = std::fs::remove_file(staged_path(&exe));
    let old = old_path(&exe);
    if old.exists() {
        thread::spawn(move || {
            for _ in 0..30 {
                if std::fs::remove_file(&old).is_ok() || !old.exists() {
                    return;
                }
                thread::sleep(Duration::from_secs(2));
            }
        });
    }
}

// --- Versiones --------------------------------------------------------------

/// `v1.2.3` o `1.2.3-beta` → (1, 2, 3).
pub fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let core = v.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>());
    let major = parts.next()?.ok()?;
    let minor = parts.next().unwrap_or(Ok(0)).ok()?;
    let patch = parts.next().unwrap_or(Ok(0)).ok()?;
    Some((major, minor, patch))
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Debug, Deserialize)]
struct GhAsset {
    name: String,
    size: u64,
    browser_download_url: String,
    /// `sha256:<hex>` (GitHub lo calcula al subir el fichero).
    #[serde(default)]
    digest: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Release {
    pub version: String,
    pub url: String,
    pub download_url: String,
    pub size: u64,
    pub sha256: String,
}

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .user_agent(concat!("AGestBackup/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30 * 60))
        .build()
        .expect("cliente HTTP")
}

fn is_hex_sha256(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Elige el ejecutable de esta plataforma y su SHA-256 en una versión publicada.
fn pick(release: GhRelease, asset_name: &str, http: &reqwest::blocking::Client) -> Result<Release> {
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == asset_name)
        .ok_or_else(|| anyhow!("la versión {} no trae {asset_name}", release.tag_name))?;
    let from_digest = asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"))
        .filter(|h| is_hex_sha256(h))
        .map(str::to_lowercase);
    let sha256 = match from_digest {
        Some(h) => h,
        None => {
            let sums = release
                .assets
                .iter()
                .find(|a| a.name == format!("{asset_name}.sha256"))
                .ok_or_else(|| anyhow!("la versión {} no publica su SHA-256", release.tag_name))?;
            let text = http
                .get(&sums.browser_download_url)
                .send()?
                .error_for_status()?
                .text()?;
            text.split_whitespace()
                .find(|w| is_hex_sha256(w))
                .map(str::to_lowercase)
                .ok_or_else(|| anyhow!("el fichero .sha256 no es válido"))?
        }
    };
    Ok(Release {
        version: release.tag_name.trim_start_matches(['v', 'V']).to_string(),
        url: release.html_url.clone(),
        download_url: asset.browser_download_url.clone(),
        size: asset.size,
        sha256,
    })
}

/// Última versión publicada, si es más nueva que esta.
pub fn check(http: &reqwest::blocking::Client) -> Result<Option<Release>> {
    let Some(asset) = ASSET else {
        bail!("las actualizaciones automáticas solo están disponibles en Windows");
    };
    let url = std::env::var("AGEST_BACKUP_UPDATE_URL").unwrap_or_else(|_| LATEST_URL.into());
    let resp = http
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .map_err(|_| anyhow!("no se puede conectar con GitHub para buscar actualizaciones"))?;
    if !resp.status().is_success() {
        bail!(
            "GitHub respondió {} al buscar actualizaciones",
            resp.status().as_u16()
        );
    }
    let release: GhRelease = resp.json()?;
    if release.draft || release.prerelease || !is_newer(&release.tag_name, CURRENT) {
        return Ok(None);
    }
    pick(release, asset, http).map(Some)
}

/// Descarga el ejecutable nuevo junto al actual y comprueba tamaño y SHA-256.
pub fn download(
    http: &reqwest::blocking::Client,
    release: &Release,
    mut progress: impl FnMut(u64),
) -> Result<PathBuf> {
    let exe = exe_path()
        .or_else(|| std::env::current_exe().ok())
        .ok_or_else(|| anyhow!("no se sabe dónde está el ejecutable"))?;
    let staged = staged_path(&exe);
    let part = sibling(&exe, ".part");
    let result = (|| -> Result<()> {
        let resp = http
            .get(&release.download_url)
            .send()
            .map_err(|_| anyhow!("no se puede descargar la actualización"))?
            .error_for_status()?;
        let (sha, size) =
            crate::worker::save_stream(resp, &part, &mut progress).with_context(|| {
                format!(
                    "no se puede escribir en {}",
                    exe.parent().unwrap_or(&exe).display()
                )
            })?;
        if size != release.size || !sha.eq_ignore_ascii_case(&release.sha256) {
            bail!("la actualización descargada está dañada (SHA-256 distinto)");
        }
        if cfg!(windows) {
            let mut head = [0u8; 2];
            use std::io::Read;
            std::fs::File::open(&part)?.read_exact(&mut head)?;
            if &head != b"MZ" {
                bail!("la actualización descargada no es un ejecutable de Windows");
            }
        }
        std::fs::rename(&part, &staged)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result.map(|()| staged)
}

/// Pone el ejecutable descargado en el sitio del actual.
fn swap(exe: &Path) -> Result<()> {
    let staged = staged_path(exe);
    if !staged.exists() {
        bail!("no hay ninguna actualización descargada");
    }
    let old = old_path(exe);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old).context("no se puede sustituir el ejecutable")?;
    if let Err(e) = std::fs::rename(&staged, exe) {
        let _ = std::fs::rename(&old, exe);
        return Err(e).context("no se puede instalar la actualización");
    }
    Ok(())
}

/// Instala la versión descargada (si la hay) sin arrancarla: al salir.
pub fn install_on_exit() {
    if !matches!(status().phase, Phase::Ready { .. }) {
        return;
    }
    if let Some(exe) = exe_path() {
        let _ = swap(&exe);
    }
}

/// Instala la versión descargada y la arranca. Si va bien, quien llama debe
/// cerrar este proceso enseguida.
pub fn install_and_relaunch(minimized: bool) -> Result<()> {
    let exe = exe_path().ok_or_else(|| anyhow!("no se sabe dónde está el ejecutable"))?;
    let result = swap(&exe).and_then(|()| {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("--updated");
        if minimized {
            cmd.arg("--minimized");
        }
        if let Err(e) = cmd.spawn() {
            // vuelve a dejar la versión actual para no quedarnos sin app
            let _ = std::fs::rename(&exe, staged_path(&exe));
            let _ = std::fs::rename(old_path(&exe), &exe);
            return Err(e).context("no se ha podido arrancar la versión nueva");
        }
        Ok(())
    });
    if let Err(e) = &result {
        set_phase(Phase::Failed(format!("{e:#}")));
    }
    result
}

/// ¿Toca instalar ya la actualización sola? Solo con la app en la bandeja,
/// las actualizaciones automáticas activadas y sin copias en marcha.
pub fn should_auto_install(auto_update: bool, busy: bool) -> bool {
    auto_update
        && !busy
        && WINDOW_HIDDEN.load(Ordering::Relaxed)
        && matches!(status().phase, Phase::Ready { .. })
}

/// Para la línea de comandos: busca una versión nueva.
pub fn check_cli() -> Result<Option<Release>> {
    check(&http())
}

/// Para la línea de comandos: descarga, verifica e instala (sin arrancarla).
pub fn download_and_install(release: &Release) -> Result<()> {
    download(&http(), release, |_| {})?;
    let exe = exe_path().ok_or_else(|| anyhow!("no se sabe dónde está el ejecutable"))?;
    swap(&exe)
}

/// Pide al hilo de actualizaciones que mire ahora.
pub fn check_now() {
    if let Some(tx) = WAKE.get() {
        let _ = tx.lock().unwrap().send(());
    }
}

/// Hilo que busca y descarga actualizaciones. `auto_update` desactivado
/// solo deja buscar a mano (`check_now`).
pub fn start(config: Arc<Mutex<Config>>, ctx: egui::Context) {
    if ASSET.is_none()
        || cfg!(debug_assertions) && std::env::var_os("AGEST_BACKUP_UPDATE_URL").is_none()
    {
        // En desarrollo (cargo run) no se reemplaza el binario de target/.
        set_phase(Phase::Unsupported);
        return;
    }
    let (tx, rx) = mpsc::channel::<()>();
    if WAKE.set(Mutex::new(tx)).is_err() {
        return;
    }
    thread::Builder::new()
        .name("agest-backup-update".into())
        .spawn(move || {
            let http = http();
            let mut wait = FIRST_CHECK_AFTER;
            loop {
                let forced = match rx.recv_timeout(wait) {
                    Ok(()) => true,
                    Err(RecvTimeoutError::Timeout) => false,
                    Err(RecvTimeoutError::Disconnected) => return,
                };
                if !forced && !config.lock().unwrap().auto_update {
                    wait = CHECK_EVERY;
                    continue;
                }
                wait = if run_once(&http, &ctx) {
                    CHECK_EVERY
                } else {
                    RETRY_AFTER
                };
                ctx.request_repaint();
            }
        })
        .expect("hilo de actualizaciones");
}

/// Busca y, si hay, descarga. Devuelve `false` si algo falló.
fn run_once(http: &reqwest::blocking::Client, ctx: &egui::Context) -> bool {
    let previous = status().phase;
    set_phase(Phase::Checking);
    ctx.request_repaint();
    let result = check(http).and_then(|found| {
        let Some(release) = found else {
            return Ok(Phase::UpToDate);
        };
        if let Phase::Ready { version, .. } = &previous
            && *version == release.version
        {
            return Ok(previous.clone());
        }
        let total = release.size;
        set_phase(Phase::Downloading {
            version: release.version.clone(),
            done: 0,
            total,
        });
        download(http, &release, |done| {
            if let Phase::Downloading { done: d, .. } = &mut STATUS.lock().unwrap().phase {
                *d = done;
            }
            ctx.request_repaint();
        })?;
        Ok(Phase::Ready {
            version: release.version,
            url: release.url,
        })
    });
    let mut st = STATUS.lock().unwrap();
    st.last_check = Some(Local::now());
    match result {
        Ok(phase) => {
            st.phase = phase;
            true
        }
        Err(e) => {
            // Si ya había una descargada, se conserva.
            st.phase = match previous {
                Phase::Ready { .. } => previous,
                _ => Phase::Failed(format!("{e:#}")),
            };
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compara_versiones() {
        assert_eq!(parse_version("v0.1.2"), Some((0, 1, 2)));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("2.0.0-beta.1"), Some((2, 0, 0)));
        assert_eq!(parse_version("latest"), None);
        assert!(is_newer("v0.1.2", "0.1.1"));
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(!is_newer("v0.1.1", "0.1.1"));
        assert!(!is_newer("v0.1.0", "0.1.1"));
        assert!(!is_newer("nightly", "0.1.1"));
    }

    fn release(assets: Vec<GhAsset>) -> GhRelease {
        GhRelease {
            tag_name: "v9.9.9".into(),
            html_url: "https://github.com/x".into(),
            draft: false,
            prerelease: false,
            assets,
        }
    }

    #[test]
    fn usa_el_digest_de_github() {
        let sha = "6670f36e27b363293e83fda5a977a65fe958ed20ff474fe0b98b7094e05efb75";
        let r = pick(
            release(vec![GhAsset {
                name: "agest-backup.exe".into(),
                size: 10,
                browser_download_url: "https://example.invalid/a.exe".into(),
                digest: Some(format!("sha256:{}", sha.to_uppercase())),
            }]),
            "agest-backup.exe",
            &http(),
        )
        .unwrap();
        assert_eq!(r.version, "9.9.9");
        assert_eq!(r.sha256, sha);
        assert_eq!(r.size, 10);
    }

    #[test]
    fn sin_ejecutable_o_sin_sha_no_actualiza() {
        let none = pick(release(vec![]), "agest-backup.exe", &http());
        assert!(none.is_err());
        let no_sha = pick(
            release(vec![GhAsset {
                name: "agest-backup.exe".into(),
                size: 10,
                browser_download_url: "https://example.invalid/a.exe".into(),
                digest: None,
            }]),
            "agest-backup.exe",
            &http(),
        );
        assert!(no_sha.is_err());
    }

    #[test]
    fn sustituye_el_ejecutable_y_deja_el_viejo() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("agest-backup.exe");
        std::fs::write(&exe, b"viejo").unwrap();
        assert!(swap(&exe).is_err(), "sin descarga no hace nada");
        assert_eq!(std::fs::read(&exe).unwrap(), b"viejo");
        std::fs::write(staged_path(&exe), b"nuevo").unwrap();
        swap(&exe).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"nuevo");
        assert_eq!(std::fs::read(old_path(&exe)).unwrap(), b"viejo");
        assert!(!staged_path(&exe).exists());
    }
}
