//! Inicio de sesión: OAuth 2.0 «authorization code» + PKCE con redirect
//! loopback (RFC 8252). La app abre el navegador en la pantalla de
//! autorización del panel y espera la vuelta en `http://127.0.0.1:<puerto>`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use crate::api::{ApiClient, TokenResponse};

const WAIT: Duration = Duration::from_secs(10 * 60);

pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("generador aleatorio del sistema");
    URL_SAFE_NO_PAD.encode(buf)
}

impl Pkce {
    pub fn new() -> Self {
        // 64 bytes → 86 caracteres (RFC 7636: entre 43 y 128)
        let verifier = random_token(64);
        let challenge = Self::challenge_for(&verifier);
        Self {
            verifier,
            challenge,
        }
    }

    pub fn challenge_for(verifier: &str) -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
    }
}

/// Saca un parámetro de una query `a=1&b=2`.
pub fn query_param(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (k == name).then(|| {
            urlencoding::decode(&v.replace('+', " "))
                .map(|s| s.into_owned())
                .unwrap_or_default()
        })
    })
}

const PAGE: &str = r#"<!doctype html><html lang="es"><head><meta charset="utf-8"><title>AGest Backup</title>
<style>body{font-family:system-ui,-apple-system,"Segoe UI",sans-serif;background:#f5f5f5;color:#171717;display:flex;align-items:center;justify-content:center;height:100vh;margin:0}
.card{background:#fff;border:1px solid #e5e5e5;border-radius:12px;padding:32px 40px;max-width:420px;text-align:center;box-shadow:0 1px 3px rgba(0,0,0,.06)}
h1{font-size:20px;margin:0 0 8px}p{color:#666;font-size:14px;line-height:1.5;margin:0}</style></head>
<body><div class="card"><h1>{{TITLE}}</h1><p>{{TEXT}}</p></div></body></html>"#;

fn page(title: &str, text: &str) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let html = PAGE.replace("{{TITLE}}", title).replace("{{TEXT}}", text);
    tiny_http::Response::from_string(html).with_header(
        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
            .expect("cabecera"),
    )
}

/// Flujo completo, bloqueante: devuelve el token del dispositivo.
pub fn login(
    api: &ApiClient,
    device_name: &str,
    hostname: &str,
    os: &str,
    cancel: Arc<AtomicBool>,
    on_url: impl FnOnce(&str),
) -> Result<TokenResponse> {
    let authorize = api.authorize_url()?;
    let server = tiny_http::Server::http("127.0.0.1:0")
        .map_err(|e| anyhow!("no se ha podido abrir el puerto local: {e}"))?;
    let port = server
        .server_addr()
        .to_ip()
        .map(|a| a.port())
        .ok_or_else(|| anyhow!("puerto local desconocido"))?;
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    let pkce = Pkce::new();
    let state = random_token(24);

    let url = format!(
        "{authorize}?client_id=agest-backup&redirect_uri={}&state={}&code_challenge={}&code_challenge_method=S256&device_name={}&hostname={}&os={}",
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(&state),
        urlencoding::encode(&pkce.challenge),
        urlencoding::encode(device_name),
        urlencoding::encode(hostname),
        urlencoding::encode(os),
    );
    on_url(&url);
    // AGEST_BACKUP_NO_BROWSER: no abre el navegador (pruebas, o equipos sin navegador
    // por defecto); el enlace se puede copiar desde la pantalla de espera.
    if std::env::var_os("AGEST_BACKUP_NO_BROWSER").is_some() {
        eprintln!("AUTHORIZE_URL={url}");
    } else if let Err(e) = open::that_detached(&url) {
        eprintln!("no se ha podido abrir el navegador: {e}");
    }

    let started = Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) {
            bail!("Inicio de sesión cancelado.");
        }
        if started.elapsed() > WAIT {
            bail!("Se ha agotado el tiempo de espera. Vuelve a intentarlo.");
        }
        let Some(request) = server.recv_timeout(Duration::from_millis(300))? else {
            continue;
        };
        let url = request.url().to_string();
        let (path, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
        if path != "/callback" {
            let _ = request.respond(tiny_http::Response::empty(404));
            continue;
        }
        if query_param(query, "state").as_deref() != Some(state.as_str()) {
            let _ = request.respond(page(
                "Enlace no válido",
                "Esta respuesta no corresponde al inicio de sesión en curso.",
            ));
            continue;
        }
        if query_param(query, "error").is_some() {
            let _ = request.respond(page(
                "Autorización cancelada",
                "Puedes cerrar esta pestaña y volver a AGest Backup.",
            ));
            bail!("Has cancelado la autorización en el navegador.");
        }
        let Some(code) = query_param(query, "code") else {
            let _ = request.respond(page(
                "Falta el código",
                "Vuelve a AGest Backup e inténtalo de nuevo.",
            ));
            bail!("El navegador no ha devuelto el código de autorización.");
        };
        return match api.exchange_code(&code, &pkce.verifier, &redirect_uri) {
            Ok(token) => {
                let _ = request.respond(page(
                    "Equipo autorizado",
                    "Ya puedes cerrar esta pestaña y volver a AGest Backup.",
                ));
                Ok(token)
            }
            Err(e) => {
                let _ = request.respond(page("No se ha podido completar", &e.to_string()));
                Err(e)
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_s256_del_rfc_7636() {
        // Ejemplo del apéndice B del RFC 7636
        assert_eq!(
            Pkce::challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let p = Pkce::new();
        assert!((43..=128).contains(&p.verifier.len()));
        assert_eq!(p.challenge, Pkce::challenge_for(&p.verifier));
    }

    #[test]
    fn lee_parametros_de_la_query() {
        let q = "code=abc%2Fdef&state=xy+z&error";
        assert_eq!(query_param(q, "code").as_deref(), Some("abc/def"));
        assert_eq!(query_param(q, "state").as_deref(), Some("xy z"));
        assert_eq!(query_param(q, "error").as_deref(), Some(""));
        assert_eq!(query_param(q, "otro"), None);
    }
}
