//! Modo línea de comandos, sin ventana: para servidores, el Programador de
//! tareas de Windows o scripts. Usa la misma sesión, carpeta y lógica de
//! copia que la aplicación.
//!
//! ```text
//! agest-backup login            autoriza este equipo (abre el navegador)
//! agest-backup folder <ruta>    elige la carpeta de copias
//! agest-backup status           tiendas, programación y espacio en disco
//! agest-backup backup [--shop ID | --due]
//!                               copia una tienda, las que tocan (--due) o todas
//! agest-backup update           instala la última versión, si hay una nueva
//! agest-backup logout           cierra la sesión y desautoriza el equipo
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result, anyhow, bail};
use chrono::Local;

use crate::api::{ApiClient, DeviceState};
use crate::config::{Config, TokenStore, machine_info};
use crate::schedule;
use crate::store::{self, Index};
use crate::worker::{self, Job, Stage, human_bytes};

pub const COMMANDS: &[&str] = &[
    "login", "folder", "status", "backup", "update", "logout", "help", "--help",
];

/// En la versión «release» la app no tiene consola propia: se engancha a la
/// de quien la lanzó para que se vea la salida.
fn attach_console() {
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(
            windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
        );
    }
}

pub fn run(args: &[String]) -> i32 {
    attach_console();
    match dispatch(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("Error: {e:#}");
            1
        }
    }
}

fn dispatch(args: &[String]) -> Result<()> {
    let command = args.first().map(String::as_str).unwrap_or("help");
    let rest = &args[1.min(args.len())..];
    match command {
        "login" => login(),
        "folder" => folder(rest),
        "status" => status(),
        "backup" => backup(rest),
        "update" => self_update(),
        "logout" => logout(),
        _ => {
            println!(
                "{}",
                include_str!("cli.rs")
                    .lines()
                    .skip_while(|l| !l.contains("```text"))
                    .skip(1)
                    .take_while(|l| !l.contains("```"))
                    .map(|l| l.trim_start_matches("//! "))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            Ok(())
        }
    }
}

fn session() -> Result<(Config, ApiClient)> {
    let config = Config::load();
    let token = TokenStore::load(&config.api_base)
        .ok_or_else(|| anyhow!("este equipo no tiene sesión; ejecuta `agest-backup login`"))?;
    let api = ApiClient::new(&config.api_base, Some(token));
    Ok((config, api))
}

fn login() -> Result<()> {
    let mut config = Config::load();
    let api = ApiClient::new(&config.api_base, None);
    let (host, os) = machine_info();
    let name = config.device_name.clone().unwrap_or_else(|| host.clone());
    println!("Servidor: {}", config.api_base);
    let token = crate::oauth::login(
        &api,
        &name,
        &host,
        &os,
        Arc::new(AtomicBool::new(false)),
        |url| {
            println!(
                "Autoriza este equipo en el navegador. Si no se abre solo, abre este enlace:\n{url}"
            );
        },
    )?;
    TokenStore::save(&config.api_base, &token.access_token)?;
    config.device_name = Some(token.device.name.clone());
    config.save()?;
    println!("Equipo «{}» autorizado.", token.device.name);
    Ok(())
}

fn folder(rest: &[String]) -> Result<()> {
    let path = PathBuf::from(
        rest.first()
            .ok_or_else(|| anyhow!("indica la carpeta: agest-backup folder <ruta>"))?,
    );
    std::fs::create_dir_all(&path)
        .with_context(|| format!("no se puede crear {}", path.display()))?;
    let path = path.canonicalize()?;
    // sin el prefijo `\\?\` de Windows: la ruta se muestra en el panel
    let path = path
        .to_str()
        .and_then(|p| p.strip_prefix(r"\\?\"))
        .filter(|p| !p.starts_with("UNC"))
        .map(PathBuf::from)
        .unwrap_or(path);
    let mut config = Config::load();
    config.backup_dir = Some(path.clone());
    config.save()?;
    println!("Carpeta de copias: {}", path.display());
    Ok(())
}

fn heartbeat(config: &Config, api: &ApiClient) -> Result<DeviceState> {
    let (hb, _, _) = worker::build_heartbeat(config.backup_dir.as_deref());
    api.heartbeat(&hb)
}

fn status() -> Result<()> {
    let (config, api) = session()?;
    let state = heartbeat(&config, &api)?;
    println!(
        "Equipo: {} · {}",
        state.device.name,
        state.user.email.unwrap_or_default()
    );
    match &config.backup_dir {
        Some(dir) => {
            let index = Index::load(dir);
            println!(
                "Carpeta: {} ({} copias, {})",
                dir.display(),
                index.entries.len(),
                human_bytes(index.total_bytes())
            );
            if let Some((total, free)) = store::disk_space(dir) {
                println!(
                    "Disco: {} libres de {}",
                    human_bytes(free),
                    human_bytes(total)
                );
            }
        }
        None => println!("Carpeta: sin elegir (agest-backup folder <ruta>)"),
    }
    let now = Local::now();
    for sc in &state.schedules {
        let next = schedule::next_slot(sc, now)
            .map(|t| format!(" · próxima {}", t.format("%d/%m/%Y %H:%M")))
            .unwrap_or_default();
        println!(
            "  #{} {}: {}{}",
            sc.shop_id,
            sc.shop_name.clone().unwrap_or_default(),
            schedule::describe(sc),
            next
        );
    }
    if !state.pending_runs.is_empty() {
        println!(
            "Copias pedidas desde el panel: {}",
            state.pending_runs.len()
        );
    }
    Ok(())
}

fn backup(rest: &[String]) -> Result<()> {
    let (config, api) = session()?;
    let dir = config
        .backup_dir
        .clone()
        .ok_or_else(|| anyhow!("elige antes la carpeta: agest-backup folder <ruta>"))?;
    let state = heartbeat(&config, &api)?;
    let mut index = Index::load(&dir);

    let only = rest
        .iter()
        .position(|a| a == "--shop")
        .and_then(|i| rest.get(i + 1))
        .map(|v| v.parse::<i64>())
        .transpose()
        .context("--shop necesita un número")?;
    let due_only = rest.iter().any(|a| a == "--due");

    let mut jobs: Vec<Job> = Vec::new();
    for run in &state.pending_runs {
        if only.is_none_or(|id| id == run.shop_id) {
            jobs.push(Job {
                shop_id: run.shop_id,
                run_id: Some(run.id),
                trigger: "panel",
            });
        }
    }
    let last = index.last_success();
    for shop in &state.shops {
        if jobs.iter().any(|j| j.shop_id == shop.id) || only.is_some_and(|id| id != shop.id) {
            continue;
        }
        if due_only {
            let sc = state
                .schedules
                .iter()
                .find(|s| s.shop_id == shop.id)
                .cloned()
                .unwrap_or_default();
            if !schedule::is_due(&sc, last.get(&shop.id).copied(), None, Local::now()) {
                continue;
            }
            jobs.push(Job {
                shop_id: shop.id,
                run_id: None,
                trigger: "scheduled",
            });
        } else {
            jobs.push(Job {
                shop_id: shop.id,
                run_id: None,
                trigger: "manual",
            });
        }
    }
    if let Some(id) = only
        && !state.shops.iter().any(|s| s.id == id)
    {
        bail!("la tienda #{id} no es de esta cuenta");
    }
    if jobs.is_empty() {
        println!("No hay copias que hacer.");
        return Ok(());
    }

    let mut failed = 0;
    for job in jobs {
        let name = state
            .shops
            .iter()
            .find(|s| s.id == job.shop_id)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| format!("Tienda #{}", job.shop_id));
        println!("→ {name}");
        let mut last_stage = None;
        let result =
            worker::run_backup(&api, &dir, &job, &name, &mut |stage, downloaded, total| {
                if last_stage.as_ref() != Some(&stage) {
                    let label = match stage {
                        Stage::Waiting => "preparando",
                        Stage::Building => "el servidor genera la copia",
                        Stage::Downloading => "descargando",
                        Stage::Verifying => "comprobando SHA-256",
                    };
                    println!("   {label}…");
                    last_stage = Some(stage);
                }
                let _ = (downloaded, total);
            });
        match result {
            Ok(entry) => {
                println!(
                    "   ✔ {} ({}) sha256 {}",
                    entry.file,
                    human_bytes(entry.size),
                    &entry.sha256[..16]
                );
                index.add(entry);
                let keep = state
                    .schedules
                    .iter()
                    .find(|s| s.shop_id == job.shop_id)
                    .map(|s| s.retention_count)
                    .unwrap_or(14);
                let pruned = worker::prune(&api, &dir, &mut index, job.shop_id, keep);
                if pruned > 0 {
                    println!("   {pruned} copias antiguas borradas por la retención");
                }
                index.save(&dir)?;
            }
            Err(e) => {
                failed += 1;
                println!("   ✖ {e}");
            }
        }
    }
    let _ = heartbeat(&config, &api);
    if failed > 0 {
        bail!("{failed} copias han fallado");
    }
    Ok(())
}

fn self_update() -> Result<()> {
    crate::update::init(&[]);
    println!("Versión instalada: {}", crate::update::CURRENT);
    let Some(release) = crate::update::check_cli()? else {
        println!("Ya tienes la última versión.");
        return Ok(());
    };
    println!("Descargando la versión {}…", release.version);
    crate::update::download_and_install(&release)?;
    println!(
        "Instalada la versión {}. Si la app está abierta, sal desde la bandeja y vuelve a abrirla.",
        release.version
    );
    Ok(())
}

fn logout() -> Result<()> {
    let (config, api) = session()?;
    let _ = api.logout();
    TokenStore::delete(&config.api_base);
    println!("Sesión cerrada. Las copias del disco se conservan.");
    Ok(())
}
