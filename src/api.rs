//! Cliente de la API de AGest para el dispositivo de copias.
//!
//! Contrato: `/api/backup/oauth/*` (público) y `/api/backup-device/*`
//! (con `Authorization: Bearer agbk_...`).

use std::io::Read;
use std::time::Duration;

use anyhow::Result;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Device {
    pub id: i64,
    pub name: String,
    pub hostname: Option<String>,
    pub os: Option<String>,
    pub app_version: Option<String>,
    pub backup_path: Option<String>,
    pub disk_total_bytes: Option<u64>,
    pub disk_free_bytes: Option<u64>,
    pub backups_bytes: u64,
    pub backups_count: u64,
    pub online: bool,
    pub status: String,
    pub last_seen_at: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Shop {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UserInfo {
    pub id: Option<i64>,
    pub name: Option<String>,
    pub email: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    pub shop_id: i64,
    #[serde(skip_serializing)]
    pub shop_name: Option<String>,
    pub enabled: bool,
    pub frequency: String,
    pub time: String,
    pub weekday: Option<u8>,
    pub month_day: Option<u8>,
    pub retention_count: u32,
    pub include_files: bool,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            shop_id: 0,
            shop_name: None,
            enabled: false,
            frequency: "daily".into(),
            time: "03:00".into(),
            weekday: None,
            month_day: None,
            retention_count: 14,
            include_files: true,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Run {
    pub id: i64,
    pub backup_device_id: i64,
    pub shop_id: i64,
    pub shop_name: Option<String>,
    pub trigger: String,
    pub status: String,
    pub include_files: bool,
    pub file_name: Option<String>,
    pub size_bytes: Option<u64>,
    pub sha256: Option<String>,
    pub rows_count: Option<u64>,
    pub files_count: Option<u64>,
    pub error: Option<String>,
    pub requested_at: Option<String>,
    pub finished_at: Option<String>,
    pub pruned_at: Option<String>,
    pub duration_seconds: Option<i64>,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DeviceState {
    pub device: Device,
    pub user: UserInfo,
    pub shops: Vec<Shop>,
    pub schedules: Vec<Schedule>,
    pub pending_runs: Vec<Run>,
    pub server_time: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub device: Device,
}

#[derive(Debug, Deserialize)]
struct OauthConfig {
    authorize_url: String,
}

#[derive(Debug, Deserialize)]
struct DataEnvelope<T> {
    data: T,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Heartbeat {
    pub backup_path: Option<String>,
    pub disk_total_bytes: Option<u64>,
    pub disk_free_bytes: Option<u64>,
    pub backups_bytes: u64,
    pub backups_count: u64,
    pub app_version: String,
    pub os: String,
    pub hostname: String,
}

/// Error de la API con el mensaje del servidor (ya traducido por él).
#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    /// `error_code` del servidor (p. ej. `backup_device_revoked`).
    #[allow(dead_code)]
    pub code: Option<String>,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

impl ApiError {
    /// El token ya no vale (revocado, inválido): hay que volver a iniciar sesión.
    pub fn is_auth(&self) -> bool {
        self.status == 401
    }
}

/// ¿Es este error un «vuelve a iniciar sesión»?
pub fn is_auth_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<ApiError>()
        .is_some_and(ApiError::is_auth)
}

#[derive(Clone)]
pub struct ApiClient {
    base: String,
    token: Option<String>,
    http: reqwest::blocking::Client,
}

pub struct Download {
    pub reader: Box<dyn Read + Send>,
    pub sha256: Option<String>,
    pub size: Option<u64>,
}

impl ApiClient {
    pub fn new(base: &str, token: Option<String>) -> Self {
        let http = reqwest::blocking::Client::builder()
            .user_agent(concat!("AGestBackup/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            .build()
            .expect("cliente HTTP");
        Self {
            base: base.trim_end_matches('/').to_string(),
            token,
            http,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base, path.trim_start_matches('/'))
    }

    fn auth(&self, req: reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder {
        let req = req
            .header("Accept", "application/json")
            .header("Accept-Language", "es");
        match &self.token {
            Some(t) => req.bearer_auth(t),
            None => req,
        }
    }

    fn check(resp: reqwest::blocking::Response) -> Result<reqwest::blocking::Response> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body: serde_json::Value = resp.json().unwrap_or(serde_json::Value::Null);
        let message = body
            .get("errors")
            .and_then(|e| e.as_object())
            .and_then(|o| o.values().next())
            .and_then(|v| v.get(0))
            .and_then(|v| v.as_str())
            .or_else(|| body.get("error").and_then(|v| v.as_str()))
            .or_else(|| body.get("message").and_then(|v| v.as_str()))
            .map(str::to_string)
            .unwrap_or_else(|| format!("El servidor respondió {}", status.as_u16()));
        Err(ApiError {
            status: status.as_u16(),
            code: body
                .get("error_code")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            message,
        }
        .into())
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self
            .auth(self.http.get(self.url(path)))
            .send()
            .map_err(net_error)?;
        Ok(Self::check(resp)?.json()?)
    }

    fn send<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: serde_json::Value,
    ) -> Result<T> {
        let resp = self
            .auth(self.http.request(method, self.url(path)))
            .json(&body)
            .send()
            .map_err(net_error)?;
        Ok(Self::check(resp)?.json()?)
    }

    // --- OAuth -----------------------------------------------------------

    pub fn authorize_url(&self) -> Result<String> {
        Ok(self
            .get::<OauthConfig>("backup/oauth/config")?
            .authorize_url)
    }

    pub fn exchange_code(
        &self,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<TokenResponse> {
        self.send(
            reqwest::Method::POST,
            "backup/oauth/token",
            json!({
                "grant_type": "authorization_code",
                "client_id": "agest-backup",
                "code": code,
                "code_verifier": verifier,
                "redirect_uri": redirect_uri,
            }),
        )
    }

    // --- Dispositivo -----------------------------------------------------

    pub fn heartbeat(&self, hb: &Heartbeat) -> Result<DeviceState> {
        self.send(
            reqwest::Method::PUT,
            "backup-device/heartbeat",
            serde_json::to_value(hb)?,
        )
    }

    pub fn save_schedule(&self, schedule: &Schedule) -> Result<Schedule> {
        let env: DataEnvelope<Schedule> = self.send(
            reqwest::Method::PUT,
            &format!("backup-device/schedules/{}", schedule.shop_id),
            serde_json::to_value(schedule)?,
        )?;
        Ok(env.data)
    }

    pub fn runs(&self, limit: u32) -> Result<Vec<Run>> {
        Ok(self
            .get::<DataEnvelope<Vec<Run>>>(&format!("backup-device/runs?limit={limit}"))?
            .data)
    }

    pub fn create_run(&self, shop_id: i64, trigger: &str) -> Result<Run> {
        let env: DataEnvelope<Run> = self.send(
            reqwest::Method::POST,
            "backup-device/runs",
            json!({ "shop_id": shop_id, "trigger": trigger }),
        )?;
        Ok(env.data)
    }

    pub fn start_run(&self, id: i64) -> Result<Run> {
        let env: DataEnvelope<Run> = self.send(
            reqwest::Method::POST,
            &format!("backup-device/runs/{id}/start"),
            json!({}),
        )?;
        Ok(env.data)
    }

    pub fn get_run(&self, id: i64) -> Result<Run> {
        Ok(self
            .get::<DataEnvelope<Run>>(&format!("backup-device/runs/{id}"))?
            .data)
    }

    pub fn download(&self, id: i64) -> Result<Download> {
        let resp = self
            .auth(
                self.http
                    .get(self.url(&format!("backup-device/runs/{id}/download"))),
            )
            .timeout(Duration::from_secs(6 * 3600))
            .send()
            .map_err(net_error)?;
        let resp = Self::check(resp)?;
        let header = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let sha256 = header("x-backup-sha256");
        let size = header("x-backup-size")
            .and_then(|s| s.parse().ok())
            .or(resp.content_length());
        Ok(Download {
            reader: Box::new(resp),
            sha256,
            size,
        })
    }

    pub fn complete_run(&self, id: i64, sha256: &str, file_name: &str) -> Result<Run> {
        let env: DataEnvelope<Run> = self.send(
            reqwest::Method::POST,
            &format!("backup-device/runs/{id}/complete"),
            json!({ "sha256": sha256, "file_name": file_name }),
        )?;
        Ok(env.data)
    }

    pub fn fail_run(&self, id: i64, error: &str) -> Result<()> {
        let message: String = error.chars().take(1900).collect();
        let _: serde_json::Value = self.send(
            reqwest::Method::POST,
            &format!("backup-device/runs/{id}/fail"),
            json!({ "error": message }),
        )?;
        Ok(())
    }

    pub fn mark_pruned(&self, id: i64) -> Result<()> {
        let _: serde_json::Value = self.send(
            reqwest::Method::POST,
            &format!("backup-device/runs/{id}/pruned"),
            json!({}),
        )?;
        Ok(())
    }

    pub fn logout(&self) -> Result<()> {
        let resp = self
            .auth(self.http.delete(self.url("backup-device/session")))
            .send()
            .map_err(net_error)?;
        Self::check(resp)?;
        Ok(())
    }
}

fn net_error(err: reqwest::Error) -> anyhow::Error {
    if err.is_connect() || err.is_timeout() {
        anyhow::anyhow!(
            "No se puede conectar con el servidor de AGest. Comprueba la conexión a Internet."
        )
    } else {
        err.into()
    }
}
