//! Configuración local de la app y token del dispositivo.
//!
//! La configuración (servidor, carpeta, preferencias) va en un JSON en la
//! carpeta de datos del usuario. El token `agbk_...` nunca va ahí: se guarda
//! en el almacén de credenciales del sistema (Administrador de credenciales
//! de Windows, Llavero de macOS, Secret Service en Linux).

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const PRODUCTION_API: &str = "https://api.asoft.es/api";
pub const DEVELOPMENT_API: &str = "https://agest-backend.test/api";
const KEYRING_SERVICE: &str = "agest-backup";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub api_base: String,
    pub backup_dir: Option<PathBuf>,
    pub start_with_system: bool,
    pub minimize_to_tray: bool,
    /// Descargar e instalar solas las versiones nuevas.
    pub auto_update: bool,
    pub device_name: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_base: std::env::var("AGEST_API_URL").unwrap_or_else(|_| PRODUCTION_API.to_string()),
            backup_dir: None,
            start_with_system: false,
            minimize_to_tray: true,
            auto_update: true,
            device_name: None,
        }
    }
}

pub fn data_dir() -> PathBuf {
    directories::ProjectDirs::from("es", "ASoft", "AGest Backup")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("agest-backup"))
}

fn config_path() -> PathBuf {
    data_dir().join("config.json")
}

impl Config {
    pub fn load() -> Self {
        std::fs::read_to_string(config_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .context("no se ha podido crear la carpeta de configuración")?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    pub fn is_development(&self) -> bool {
        self.api_base.trim_end_matches('/') != PRODUCTION_API
    }
}

/// Token del dispositivo en el almacén seguro del sistema, por servidor.
pub struct TokenStore;

impl TokenStore {
    fn entry(api_base: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(KEYRING_SERVICE, api_base)
            .context("almacén de credenciales no disponible")
    }

    pub fn load(api_base: &str) -> Option<String> {
        Self::entry(api_base)
            .ok()?
            .get_password()
            .ok()
            .filter(|t| t.starts_with("agbk_"))
    }

    pub fn save(api_base: &str, token: &str) -> Result<()> {
        Self::entry(api_base)?
            .set_password(token)
            .context("no se ha podido guardar el token")
    }

    pub fn delete(api_base: &str) {
        if let Ok(entry) = Self::entry(api_base) {
            let _ = entry.delete_credential();
        }
    }
}

/// Nombre del equipo y sistema operativo, para que el panel lo reconozca.
pub fn machine_info() -> (String, String) {
    let host = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "Equipo".to_string());
    let info = os_info::get();
    let os = format!("{} {}", info.os_type(), info.version());
    (host, os.chars().take(100).collect())
}
