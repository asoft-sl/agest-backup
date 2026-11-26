//! Interfaz: inicio de sesión, elegir carpeta y panel (Inicio, Programación,
//! Historial, Ajustes), con el mismo aire que el panel web.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Local};
use egui::{
    Align, Align2, Color32, CornerRadius, Frame, Layout, Margin, RichText, ScrollArea, Sense,
    Stroke, Ui, vec2,
};

use crate::api::{ApiClient, Run, Schedule, TokenResponse};
use crate::config::{Config, DEVELOPMENT_API, PRODUCTION_API, TokenStore, data_dir, machine_info};
use crate::schedule::{describe, next_slot, parse_time, weekday_name};
use crate::theme::{
    self, Kind, Tone, badge, button, button_sized, card, muted, palette, small_muted, strong,
    toggle,
};
use crate::tray::Tray;
use crate::worker::{
    Command, Notice, Progress, Shared, SharedRef, Stage, Worker, human_bytes, queued_set,
};

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Home,
    Schedules,
    History,
    Settings,
}

struct LoginFlow {
    rx: Receiver<anyhow::Result<TokenResponse>>,
    cancel: Arc<AtomicBool>,
    base: String,
    /// Enlace de autorización, por si el navegador no se abre solo.
    url: Arc<Mutex<Option<String>>>,
}

pub struct App {
    config: Arc<Mutex<Config>>,
    shared: SharedRef,
    api: Option<ApiClient>,
    worker: Option<Worker>,
    tab: Tab,
    login: Option<LoginFlow>,
    login_error: Option<String>,
    server_input: String,
    show_server: bool,
    drafts: HashMap<i64, Schedule>,
    toasts: Vec<Notice>,
    tray: Option<Tray>,
    confirm_logout: bool,
    settings_error: Option<String>,
    hid_once: bool,
}

/// Lo que la interfaz necesita del estado compartido, copiado una vez por fotograma.
struct View {
    shops: Vec<(i64, String)>,
    schedules: HashMap<i64, Schedule>,
    user: (String, String),
    device_name: String,
    connected: bool,
    last_sync: Option<DateTime<Local>>,
    last_error: Option<String>,
    current: Option<Progress>,
    queued: HashSet<i64>,
    runs: Vec<Run>,
    local_runs: HashSet<i64>,
    last_local: HashMap<i64, DateTime<Local>>,
    local_bytes: u64,
    local_count: usize,
    disk: Option<(u64, u64)>,
}

impl View {
    fn from(s: &Shared) -> Self {
        let state = s.state.clone().unwrap_or_default();
        Self {
            shops: state
                .shops
                .iter()
                .map(|sh| (sh.id, sh.name.clone()))
                .collect(),
            schedules: state
                .schedules
                .iter()
                .map(|sc| (sc.shop_id, sc.clone()))
                .collect(),
            user: (
                state.user.name.clone().unwrap_or_default(),
                state.user.email.clone().unwrap_or_default(),
            ),
            device_name: state.device.name.clone(),
            connected: s.connected,
            last_sync: s.last_sync,
            last_error: s.last_error.clone(),
            current: s.current.clone(),
            queued: queued_set(s),
            runs: s.runs.clone(),
            local_runs: s.index.entries.iter().map(|e| e.run_id).collect(),
            last_local: s.index.last_success(),
            local_bytes: s.index.total_bytes(),
            local_count: s.index.entries.len(),
            disk: s.disk,
        }
    }

    fn schedule(&self, shop_id: i64) -> Schedule {
        self.schedules
            .get(&shop_id)
            .cloned()
            .unwrap_or_else(|| Schedule {
                shop_id,
                ..Default::default()
            })
    }

    fn last_run(&self, shop_id: i64) -> Option<&Run> {
        self.runs.iter().find(|r| r.shop_id == shop_id)
    }
}

fn parse_rfc3339(value: Option<&str>) -> Option<DateTime<Local>> {
    value
        .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
        .map(|t| t.with_timezone(&Local))
}

pub fn relative(t: DateTime<Local>) -> String {
    let secs = (Local::now() - t).num_seconds();
    let (future, secs) = if secs < 0 {
        (true, -secs)
    } else {
        (false, secs)
    };
    let text = match secs {
        0..=59 => {
            return if future {
                "en unos segundos".into()
            } else {
                "hace un momento".into()
            };
        }
        60..=3599 => format!("{} min", secs / 60),
        3600..=86_399 => format!("{} h", secs / 3600),
        _ => {
            let days = secs / 86_400;
            if days == 1 {
                "1 día".into()
            } else {
                format!("{days} días")
            }
        }
    };
    if future {
        format!("en {text}")
    } else {
        format!("hace {text}")
    }
}

fn format_dt(t: DateTime<Local>) -> String {
    t.format("%d/%m/%Y %H:%M").to_string()
}

fn status_tone(status: &str) -> (Tone, &'static str) {
    match status {
        "completed" => (Tone::Success, "Completada"),
        "failed" => (Tone::Danger, "Fallida"),
        "expired" => (Tone::Warning, "Caducada"),
        "requested" => (Tone::Info, "Pendiente"),
        "building" => (Tone::Info, "Generando"),
        "ready" => (Tone::Info, "Lista"),
        "downloading" => (Tone::Info, "Descargando"),
        _ => (Tone::Neutral, "—"),
    }
}

fn trigger_label(trigger: &str) -> &'static str {
    match trigger {
        "scheduled" => "Programada",
        "manual" => "Desde el equipo",
        "panel" => "Desde el panel",
        _ => "—",
    }
}

/// Programación sin el nombre de la tienda, para comparar borrador y servidor.
fn same_schedule(a: &Schedule, b: &Schedule) -> bool {
    Schedule {
        shop_name: None,
        ..a.clone()
    } == Schedule {
        shop_name: None,
        ..b.clone()
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, start_hidden: bool) -> Self {
        theme::apply(&cc.egui_ctx);
        let config = Config::load();
        let tray = crate::tray::create(&cc.egui_ctx);
        if start_hidden && tray.is_none() {
            cc.egui_ctx
                .send_viewport_cmd(egui::ViewportCommand::Visible(true));
        }
        let server_input = config.api_base.clone();
        let token = TokenStore::load(&config.api_base);

        let mut app = Self {
            config: Arc::new(Mutex::new(config)),
            shared: Arc::new(Mutex::new(Shared::default())),
            api: None,
            worker: None,
            tab: Tab::Home,
            login: None,
            login_error: None,
            server_input,
            show_server: false,
            drafts: HashMap::new(),
            toasts: Vec::new(),
            tray,
            confirm_logout: false,
            settings_error: None,
            hid_once: false,
        };
        if let Some(token) = token {
            app.start_session(token, &cc.egui_ctx);
        }
        app
    }

    fn base(&self) -> String {
        self.config.lock().unwrap().api_base.clone()
    }

    fn start_session(&mut self, token: String, ctx: &egui::Context) {
        let api = ApiClient::new(&self.base(), Some(token));
        *self.shared.lock().unwrap() = Shared::default();
        self.worker = Some(Worker::start(
            api.clone(),
            self.config.clone(),
            self.shared.clone(),
            ctx.clone(),
        ));
        self.api = Some(api);
        self.drafts.clear();
        self.tab = Tab::Home;
    }

    fn end_session(&mut self, message: Option<String>) {
        self.worker = None;
        self.api = None;
        TokenStore::delete(&self.base());
        *self.shared.lock().unwrap() = Shared::default();
        self.login_error = message;
        self.confirm_logout = false;
    }

    fn send(&self, cmd: Command) {
        if let Some(w) = &self.worker {
            w.send(cmd);
        }
    }

    fn begin_login(&mut self, ctx: &egui::Context) {
        let base = {
            let mut cfg = self.config.lock().unwrap();
            let input = self.server_input.trim().trim_end_matches('/').to_string();
            if !input.is_empty() && input != cfg.api_base {
                cfg.api_base = input;
                let _ = cfg.save();
            }
            cfg.api_base.clone()
        };
        let (host, os) = machine_info();
        let name = self
            .config
            .lock()
            .unwrap()
            .device_name
            .clone()
            .unwrap_or_else(|| host.clone());
        let cancel = Arc::new(AtomicBool::new(false));
        let url = Arc::new(Mutex::new(None));
        let (tx, rx) = mpsc::channel();
        {
            let (cancel, ctx, base, url) = (cancel.clone(), ctx.clone(), base.clone(), url.clone());
            std::thread::spawn(move || {
                let api = ApiClient::new(&base, None);
                let repaint = ctx.clone();
                let result = crate::oauth::login(&api, &name, &host, &os, cancel, move |link| {
                    *url.lock().unwrap() = Some(link.to_string());
                    repaint.request_repaint();
                });
                let _ = tx.send(result);
                ctx.request_repaint();
            });
        }
        self.login_error = None;
        self.login = Some(LoginFlow {
            rx,
            cancel,
            base,
            url,
        });
    }

    fn poll_login(&mut self, ctx: &egui::Context) {
        let Some(flow) = &self.login else { return };
        let Ok(result) = flow.rx.try_recv() else {
            return;
        };
        let base = flow.base.clone();
        self.login = None;
        match result {
            Ok(token) => {
                let saved = TokenStore::save(&base, &token.access_token);
                {
                    let mut cfg = self.config.lock().unwrap();
                    cfg.device_name = Some(token.device.name.clone());
                    let _ = cfg.save();
                }
                self.start_session(token.access_token, ctx);
                if let Err(e) = saved {
                    self.shared
                        .lock()
                        .unwrap()
                        .notify(format!("No se ha podido guardar la sesión en el equipo ({e}); tendrás que iniciarla de nuevo al reiniciar."), false);
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            Err(e) => self.login_error = Some(e.to_string()),
        }
    }

    fn choose_folder(&mut self) -> bool {
        let Some(dir) = rfd::FileDialog::new()
            .set_title("Carpeta para las copias de AGest")
            .pick_folder()
        else {
            return false;
        };
        let probe = dir.join(".agest-write-test");
        if std::fs::write(&probe, b"ok").is_err() {
            self.settings_error = Some("No se puede escribir en esa carpeta. Elige otra.".into());
            return false;
        }
        let _ = std::fs::remove_file(probe);
        self.settings_error = None;
        {
            let mut cfg = self.config.lock().unwrap();
            cfg.backup_dir = Some(dir);
            let _ = cfg.save();
        }
        self.send(Command::FolderChanged);
        true
    }

    fn set_autostart(&mut self, enabled: bool) {
        let result = (|| -> anyhow::Result<()> {
            let exe = std::env::current_exe()?;
            let launcher = auto_launch::AutoLaunchBuilder::new()
                .set_app_name("AGest Backup")
                .set_app_path(&exe.to_string_lossy())
                .set_args(&["--minimized"])
                .build()?;
            if enabled {
                launcher.enable()?
            } else {
                launcher.disable()?
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                let mut cfg = self.config.lock().unwrap();
                cfg.start_with_system = enabled;
                let _ = cfg.save();
                self.settings_error = None;
            }
            Err(e) => {
                self.settings_error =
                    Some(format!("No se ha podido cambiar el inicio automático: {e}"))
            }
        }
    }

    fn logout(&mut self) {
        if let Some(api) = self.api.clone() {
            std::thread::spawn(move || {
                let _ = api.logout();
            });
        }
        self.end_session(None);
    }

    // --- Pantallas -----------------------------------------------------------

    fn login_screen(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            let width = 420.0_f32.min(ui.available_width() - 32.0);
            ui.add_space(((ui.available_height() - 420.0) / 2.0).max(24.0));
            ui.vertical_centered(|ui| {
                ui.set_max_width(width);
                card(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        logo(ui, 52.0);
                        ui.add_space(8.0);
                        ui.label(theme::heading("AGest Backup"));
                        ui.label(muted(ui, "Copias de seguridad de tus tiendas en este equipo"));
                    });
                    ui.add_space(20.0);
                    if let Some(flow) = &self.login {
                        ui.vertical_centered(|ui| {
                            ui.spinner();
                            ui.label(strong("Esperando la autorización en el navegador…"));
                            ui.label(small_muted(ui, "Inicia sesión en AGest si te lo pide y pulsa «Autorizar»."));
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                let link = flow.url.lock().unwrap().clone();
                                if let Some(link) = link {
                                    if button(ui, Kind::Ghost, "Copiar enlace").on_hover_text("Por si el navegador no se ha abierto solo").clicked() {
                                        ui.ctx().copy_text(link.clone());
                                        self.shared.lock().unwrap().notify("Enlace copiado. Pégalo en el navegador.", true);
                                    }
                                    if button(ui, Kind::Ghost, "Abrir de nuevo").clicked() {
                                        let _ = open::that_detached(&link);
                                    }
                                }
                                if button(ui, Kind::Outline, "Cancelar").clicked() {
                                    flow.cancel.store(true, Ordering::Relaxed);
                                }
                            });
                        });
                    } else {
                        if button_sized(ui, Kind::Primary, "Iniciar sesión con AGest", vec2(ui.available_width(), 40.0)).clicked() {
                            self.begin_login(ctx);
                        }
                        ui.add_space(4.0);
                        ui.label(small_muted(ui, "Se abrirá el navegador para que autorices este equipo con tu cuenta de AGest."));
                    }
                    if let Some(err) = &self.login_error {
                        ui.add_space(8.0);
                        alert(ui, Tone::Danger, err);
                    }
                    ui.add_space(12.0);
                    ui.separator();
                    ui.horizontal(|ui| {
                        let label = if self.show_server { "Ocultar servidor" } else { "Servidor" };
                        if ui.add(egui::Label::new(small_muted(ui, label)).sense(Sense::click())).clicked() {
                            self.show_server = !self.show_server;
                        }
                        if self.config.lock().unwrap().is_development() {
                            badge(ui, Tone::Warning, "Desarrollo");
                        }
                    });
                    if self.show_server && self.login.is_none() {
                        ui.horizontal(|ui| {
                            if ui.selectable_label(self.server_input == PRODUCTION_API, "Producción").clicked() {
                                self.server_input = PRODUCTION_API.into();
                            }
                            if ui.selectable_label(self.server_input == DEVELOPMENT_API, "Desarrollo").clicked() {
                                self.server_input = DEVELOPMENT_API.into();
                            }
                        });
                        ui.add(egui::TextEdit::singleline(&mut self.server_input).desired_width(f32::INFINITY));
                    }
                });
            });
        });
    }

    fn setup_screen(&mut self, ctx: &egui::Context, view: &View) {
        egui::CentralPanel::default().show(ctx, |ui| {
            let width = 520.0_f32.min(ui.available_width() - 32.0);
            ui.add_space(((ui.available_height() - 360.0) / 2.0).max(24.0));
            ui.vertical_centered(|ui| {
                ui.set_max_width(width);
                card(ui, |ui| {
                    ui.label(theme::title("¿Dónde guardamos las copias?"));
                    ui.label(muted(
                        ui,
                        "Elige una carpeta de este equipo o de un disco externo. AGest Backup creará dentro una subcarpeta por tienda.",
                    ));
                    ui.add_space(12.0);
                    if !view.user.1.is_empty() {
                        ui.label(small_muted(ui, format!("Sesión: {} · {}", view.user.0, view.user.1)));
                    }
                    ui.add_space(8.0);
                    if button_sized(ui, Kind::Primary, "Elegir carpeta…", vec2(ui.available_width(), 40.0)).clicked() {
                        self.choose_folder();
                    }
                    if let Some(err) = &self.settings_error {
                        ui.add_space(8.0);
                        alert(ui, Tone::Danger, err);
                    }
                    ui.add_space(8.0);
                    if ui.add(egui::Label::new(small_muted(ui, "Cerrar sesión")).sense(Sense::click())).clicked() {
                        self.logout();
                    }
                });
            });
        });
    }

    fn sidebar(&mut self, ctx: &egui::Context, view: &View) {
        let p = if ctx.style().visuals.dark_mode {
            theme::DARK
        } else {
            theme::LIGHT
        };
        egui::SidePanel::left("nav")
            .exact_width(232.0)
            .resizable(false)
            .frame(
                Frame::NONE
                    .fill(p.sidebar)
                    .inner_margin(Margin::same(14))
                    .stroke(Stroke::new(1.0_f32, p.border)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    logo(ui, 32.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(strong("AGest Backup"));
                        let sub = if view.device_name.is_empty() {
                            "Copias de seguridad".to_string()
                        } else {
                            view.device_name.clone()
                        };
                        ui.label(small_muted(ui, sub));
                    });
                });
                ui.add_space(18.0);
                for (tab, icon, label) in [
                    (Tab::Home, "🏠", "Inicio"),
                    (Tab::Schedules, "📅", "Programación"),
                    (Tab::History, "🕘", "Historial"),
                    (Tab::Settings, "⚙", "Ajustes"),
                ] {
                    if nav_item(ui, self.tab == tab, icon, label).clicked() {
                        self.tab = tab;
                    }
                }

                ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                    let (dot, text) = if view.connected {
                        (
                            p.success,
                            view.last_sync
                                .map(|t| format!("Conectado · {}", relative(t)))
                                .unwrap_or_else(|| "Conectado".into()),
                        )
                    } else if view.last_error.is_some() {
                        (p.danger, "Sin conexión con AGest".into())
                    } else {
                        (p.muted_foreground, "Conectando…".into())
                    };
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("●").size(10.0).color(dot));
                        ui.label(small_muted(ui, text));
                    });
                    if !view.user.1.is_empty() {
                        ui.label(small_muted(ui, &view.user.1));
                        ui.label(strong(&view.user.0));
                    }
                    ui.separator();
                });
            });
    }

    fn main_screen(&mut self, ctx: &egui::Context, view: &View) {
        self.sidebar(ctx, view);
        let fill = ctx.style().visuals.panel_fill;
        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(fill).inner_margin(Margin::same(28)))
            .show(ctx, |ui| {
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let width = ui.available_width().min(980.0);
                        ui.set_max_width(width);
                        match self.tab {
                            Tab::Home => self.home(ui, view),
                            Tab::Schedules => self.schedules(ui, view),
                            Tab::History => self.history(ui, view),
                            Tab::Settings => self.settings(ui, view),
                        }
                    });
            });
    }

    fn home(&mut self, ui: &mut Ui, view: &View) {
        theme::page_header(ui, "Inicio", "Las copias de tus tiendas en este equipo.");
        if let Some(err) = &view.last_error {
            alert(
                ui,
                Tone::Warning,
                &format!("{err} Las copias se reanudarán al recuperar la conexión."),
            );
            ui.add_space(8.0);
        }

        if let Some(job) = &view.current {
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(theme::title(&format!("Copiando «{}»", job.shop_name)));
                });
                let p = palette(ui);
                let (label, fraction) = match job.stage {
                    Stage::Waiting => ("Preparando…".to_string(), None),
                    Stage::Building => ("El servidor está generando la copia…".to_string(), None),
                    Stage::Downloading => match job.total {
                        Some(total) if total > 0 => (
                            format!(
                                "Descargando {} de {}",
                                human_bytes(job.downloaded),
                                human_bytes(total)
                            ),
                            Some(job.downloaded as f32 / total as f32),
                        ),
                        _ => (format!("Descargando {}", human_bytes(job.downloaded)), None),
                    },
                    Stage::Verifying => ("Comprobando la copia (SHA-256)…".to_string(), Some(1.0)),
                };
                ui.label(muted(ui, label));
                let fraction = fraction.unwrap_or_else(|| {
                    ((job.started.elapsed().as_secs_f32() * 1.5).sin() * 0.5 + 0.5) * 0.25 + 0.05
                });
                theme::progress(ui, fraction, p.info);
            });
            ui.add_space(8.0);
        }

        let enabled = view
            .shops
            .iter()
            .filter(|(id, _)| view.schedule(*id).enabled)
            .count();
        let last = view.last_local.values().max().copied();
        ui.columns(4, |cols| {
            theme::stat(
                &mut cols[0],
                "Tiendas programadas",
                &format!("{enabled} / {}", view.shops.len()),
                "",
                Tone::Info,
            );
            theme::stat(
                &mut cols[1],
                "Última copia",
                &last.map(relative).unwrap_or_else(|| "Ninguna".into()),
                &last.map(format_dt).unwrap_or_default(),
                Tone::Success,
            );
            theme::stat(
                &mut cols[2],
                "En este equipo",
                &human_bytes(view.local_bytes),
                &format!("{} copias", view.local_count),
                Tone::Neutral,
            );
            let (free, hint, tone) = match view.disk {
                Some((total, free)) => (
                    human_bytes(free),
                    format!("libres de {}", human_bytes(total)),
                    if free.saturating_mul(10) < total {
                        Tone::Danger
                    } else {
                        Tone::Neutral
                    },
                ),
                None => ("—".into(), String::new(), Tone::Neutral),
            };
            theme::stat(&mut cols[3], "Disco libre", &free, &hint, tone);
        });
        ui.add_space(8.0);

        card(ui, |ui| {
            ui.label(theme::title("Tiendas"));
            ui.add_space(4.0);
            if view.shops.is_empty() {
                ui.label(muted(
                    ui,
                    if view.connected {
                        "Tu cuenta no tiene tiendas."
                    } else {
                        "Cargando tus tiendas…"
                    },
                ));
            }
            let now = Local::now();
            for (i, (shop_id, name)) in view.shops.iter().enumerate() {
                if i > 0 {
                    ui.separator();
                }
                let sc = view.schedule(*shop_id);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(strong(name));
                        ui.label(small_muted(ui, describe(&sc)));
                        let last = view
                            .last_local
                            .get(shop_id)
                            .map(|t| format!("Última: {}", relative(*t)))
                            .unwrap_or_else(|| "Sin copias en este equipo".into());
                        let next = next_slot(&sc, now)
                            .map(|t| format!(" · Próxima: {}", format_dt(t)))
                            .unwrap_or_default();
                        ui.label(small_muted(ui, format!("{last}{next}")));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let running = view.current.as_ref().is_some_and(|c| c.shop_id == *shop_id);
                        let busy = running || view.queued.contains(shop_id);
                        if ui
                            .add_enabled_ui(!busy, |ui| button(ui, Kind::Outline, "Copiar ahora"))
                            .inner
                            .clicked()
                        {
                            self.send(Command::RunNow(*shop_id));
                        }
                        if running {
                            badge(ui, Tone::Info, "Copiando");
                        } else if view.queued.contains(shop_id) {
                            badge(ui, Tone::Info, "En cola");
                        } else if let Some(run) = view.last_run(*shop_id) {
                            let (tone, label) = status_tone(&run.status);
                            let r = badge(ui, tone, label);
                            if let Some(err) = &run.error {
                                r.on_hover_text(err);
                            }
                        }
                    });
                });
            }
        });
        ui.add_space(8.0);
        self.disk_card(ui, view);
    }

    fn disk_card(&mut self, ui: &mut Ui, view: &View) {
        let dir = self.config.lock().unwrap().backup_dir.clone();
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::title("Carpeta de copias"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if button(ui, Kind::Outline, "Cambiar…").clicked() && self.choose_folder() {
                        self.shared.lock().unwrap().notify(
                            "Carpeta cambiada. Las copias anteriores siguen en la carpeta antigua.",
                            true,
                        );
                    }
                    if let Some(d) = &dir
                        && button(ui, Kind::Ghost, "Abrir").clicked()
                    {
                        let _ = open::that_detached(d);
                    }
                });
            });
            let path = dir
                .as_ref()
                .map(|d| d.display().to_string())
                .unwrap_or_else(|| "Sin elegir".into());
            ui.add(egui::Label::new(RichText::new(&path).monospace()).truncate())
                .on_hover_text(&path);
            if let Some((total, free)) = view.disk {
                ui.add_space(6.0);
                theme::disk_bar(ui, total, free, view.local_bytes);
                ui.horizontal(|ui| {
                    let p = palette(ui);
                    ui.label(RichText::new("●").size(10.0).color(p.info));
                    ui.label(small_muted(
                        ui,
                        format!("Copias de AGest: {}", human_bytes(view.local_bytes)),
                    ));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(small_muted(
                            ui,
                            format!("{} libres de {}", human_bytes(free), human_bytes(total)),
                        ));
                    });
                });
            }
            if let Some(err) = &self.settings_error {
                alert(ui, Tone::Danger, err);
            }
        });
    }

    fn schedules(&mut self, ui: &mut Ui, view: &View) {
        theme::page_header(
            ui,
            "Programación",
            "Cuándo copiar cada tienda. La hora es la de este equipo; si está apagado, la copia se hace al encenderlo. Se sincroniza con el panel web.",
        );
        if view.shops.is_empty() {
            card(ui, |ui| {
                ui.label(muted(ui, "Cargando tus tiendas…"));
            });
            return;
        }
        for (shop_id, name) in &view.shops {
            let server = view.schedule(*shop_id);
            let draft = self
                .drafts
                .entry(*shop_id)
                .or_insert_with(|| server.clone());
            let mut save = false;
            let mut discard = false;
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(theme::title(name));
                        ui.label(small_muted(ui, describe(draft)));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        toggle(ui, &mut draft.enabled);
                        ui.label(muted(ui, "Activa"));
                    });
                });
                ui.add_space(6.0);
                ui.add_enabled_ui(draft.enabled, |ui| {
                    egui::Grid::new(("sched", *shop_id))
                        .num_columns(4)
                        .spacing([16.0, 10.0])
                        .show(ui, |ui| {
                            ui.label(muted(ui, "Frecuencia"));
                            let freq_label = match draft.frequency.as_str() {
                                "weekly" => "Semanal",
                                "monthly" => "Mensual",
                                _ => "Diaria",
                            };
                            egui::ComboBox::from_id_salt(("freq", *shop_id))
                                .width(150.0)
                                .selected_text(freq_label)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut draft.frequency,
                                        "daily".to_string(),
                                        "Diaria",
                                    );
                                    ui.selectable_value(
                                        &mut draft.frequency,
                                        "weekly".to_string(),
                                        "Semanal",
                                    );
                                    ui.selectable_value(
                                        &mut draft.frequency,
                                        "monthly".to_string(),
                                        "Mensual",
                                    );
                                });
                            ui.label(muted(ui, "Hora"));
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut draft.time)
                                        .desired_width(64.0)
                                        .hint_text("03:00"),
                                );
                                if parse_time(&draft.time).is_none() {
                                    ui.label(
                                        RichText::new("HH:MM").size(12.0).color(palette(ui).danger),
                                    );
                                }
                            });
                            ui.end_row();

                            match draft.frequency.as_str() {
                                "weekly" => {
                                    ui.label(muted(ui, "Día"));
                                    let mut day = draft.weekday.unwrap_or(1);
                                    egui::ComboBox::from_id_salt(("wd", *shop_id))
                                        .width(150.0)
                                        .selected_text(weekday_name(day))
                                        .show_ui(ui, |ui| {
                                            for d in 1..=7u8 {
                                                ui.selectable_value(&mut day, d, weekday_name(d));
                                            }
                                        });
                                    draft.weekday = Some(day);
                                }
                                "monthly" => {
                                    ui.label(muted(ui, "Día del mes"));
                                    let mut day = draft.month_day.unwrap_or(1);
                                    egui::ComboBox::from_id_salt(("md", *shop_id))
                                        .width(150.0)
                                        .selected_text(day.to_string())
                                        .show_ui(ui, |ui| {
                                            for d in 1..=28u8 {
                                                ui.selectable_value(&mut day, d, d.to_string());
                                            }
                                        });
                                    draft.month_day = Some(day);
                                }
                                _ => {
                                    ui.label("");
                                    ui.label("");
                                }
                            }
                            ui.label(muted(ui, "Copias a conservar"));
                            ui.add(
                                egui::DragValue::new(&mut draft.retention_count)
                                    .range(1..=365)
                                    .speed(0.2),
                            );
                            ui.end_row();
                        });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        toggle(ui, &mut draft.include_files);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.label("Incluir ficheros");
                            ui.label(small_muted(
                                ui,
                                "Imágenes de productos, logos, justificantes y documentos.",
                            ));
                        });
                    });
                });
                if !same_schedule(draft, &server) {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let valid = parse_time(&draft.time).is_some();
                        if ui
                            .add_enabled_ui(valid, |ui| button(ui, Kind::Primary, "Guardar"))
                            .inner
                            .clicked()
                        {
                            save = true;
                        }
                        if button(ui, Kind::Ghost, "Descartar").clicked() {
                            discard = true;
                        }
                    });
                }
            });

            let dirty = !same_schedule(draft, &server);
            if save {
                let mut sc = draft.clone();
                sc.shop_id = *shop_id;
                if let Some(t) = parse_time(&sc.time) {
                    sc.time = t.format("%H:%M").to_string();
                }
                if sc.frequency != "weekly" {
                    sc.weekday = None;
                }
                if sc.frequency != "monthly" {
                    sc.month_day = None;
                }
                // se aplica ya en local para no «rebotar» mientras llega la respuesta
                if let Some(state) = self.shared.lock().unwrap().state.as_mut()
                    && let Some(existing) =
                        state.schedules.iter_mut().find(|s| s.shop_id == *shop_id)
                {
                    *existing = Schedule {
                        shop_name: existing.shop_name.clone(),
                        ..sc.clone()
                    };
                }
                self.drafts.insert(*shop_id, sc.clone());
                self.send(Command::SaveSchedule(sc));
            } else if discard || !dirty {
                // sin cambios pendientes: seguir lo que diga el servidor (p. ej. cambios desde la web)
                self.drafts.insert(*shop_id, server);
            }
            ui.add_space(8.0);
        }
    }

    fn history(&mut self, ui: &mut Ui, view: &View) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                theme::page_header(ui, "Historial", "Las últimas copias de este equipo.")
            });
            ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                if button(ui, Kind::Outline, "Actualizar").clicked() {
                    self.send(Command::Refresh);
                }
            });
        });
        card(ui, |ui| {
            if view.runs.is_empty() {
                ui.label(muted(ui, "Todavía no hay copias."));
                return;
            }
            egui::Grid::new("runs")
                .num_columns(7)
                .striped(true)
                .spacing([18.0, 10.0])
                .min_col_width(60.0)
                .show(ui, |ui| {
                    for h in [
                        "Fecha",
                        "Tienda",
                        "Estado",
                        "Origen",
                        "Tamaño",
                        "Duración",
                        "En el equipo",
                    ] {
                        ui.label(small_muted(ui, h));
                    }
                    ui.end_row();
                    for run in &view.runs {
                        let when =
                            parse_rfc3339(run.finished_at.as_deref().or(run.created_at.as_deref()));
                        ui.label(when.map(format_dt).unwrap_or_else(|| "—".into()));
                        ui.label(
                            run.shop_name
                                .clone()
                                .unwrap_or_else(|| format!("#{}", run.shop_id)),
                        );
                        let (tone, label) = status_tone(&run.status);
                        let r = badge(ui, tone, label);
                        if let Some(err) = &run.error {
                            r.on_hover_text(err);
                        }
                        ui.label(trigger_label(&run.trigger));
                        ui.label(
                            run.size_bytes
                                .map(human_bytes)
                                .unwrap_or_else(|| "—".into()),
                        );
                        let duration = run.duration_seconds.map(|s| {
                            if s < 60 {
                                format!("{s} s")
                            } else {
                                format!("{} min", s / 60)
                            }
                        });
                        ui.label(duration.unwrap_or_else(|| "—".into()));
                        if view.local_runs.contains(&run.id) {
                            ui.label(RichText::new("✔").color(palette(ui).success));
                        } else if run.pruned_at.is_some() {
                            ui.label(small_muted(ui, "Borrada"));
                        } else {
                            ui.label(small_muted(ui, "—"));
                        }
                        ui.end_row();
                    }
                });
        });
    }

    fn settings(&mut self, ui: &mut Ui, view: &View) {
        theme::page_header(ui, "Ajustes", "");
        self.disk_card(ui, view);
        ui.add_space(8.0);

        let (mut autostart, mut tray) = {
            let c = self.config.lock().unwrap();
            (c.start_with_system, c.minimize_to_tray)
        };
        card(ui, |ui| {
            ui.label(theme::title("Funcionamiento"));
            ui.horizontal(|ui| {
                if toggle(ui, &mut autostart).changed() {
                    self.set_autostart(autostart);
                }
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label("Iniciar con el sistema");
                    ui.label(small_muted(ui, "AGest Backup se abre minimizado al encender el equipo, para no perderse ninguna copia."));
                });
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if toggle(ui, &mut tray).changed() {
                    let mut c = self.config.lock().unwrap();
                    c.minimize_to_tray = tray;
                    let _ = c.save();
                }
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label("Seguir en la bandeja al cerrar la ventana");
                    ui.label(small_muted(
                        ui,
                        "Las copias programadas solo se hacen mientras la aplicación está abierta.",
                    ));
                });
            });
        });
        ui.add_space(8.0);

        let base = self.base();
        card(ui, |ui| {
            ui.label(theme::title("Cuenta"));
            theme::field_row(ui, "Usuario", |ui| {
                ui.label(format!("{} · {}", view.user.0, view.user.1));
            });
            theme::field_row(ui, "Este equipo", |ui| {
                ui.label(&view.device_name);
            });
            theme::field_row(ui, "Servidor", |ui| {
                ui.label(RichText::new(&base).monospace());
            });
            ui.add_space(6.0);
            ui.label(small_muted(ui, "Puedes renombrar o desautorizar este equipo desde Copias de seguridad en el panel web."));
            ui.add_space(6.0);
            if button(ui, Kind::Danger, "Cerrar sesión").clicked() {
                self.confirm_logout = true;
            }
        });
        ui.add_space(8.0);

        card(ui, |ui| {
            ui.label(theme::title("Acerca de"));
            theme::field_row(ui, "Versión", |ui| {
                ui.label(env!("CARGO_PKG_VERSION"));
            });
            theme::field_row(ui, "Datos de la app", |ui| {
                let dir = data_dir();
                if ui.link(dir.display().to_string()).clicked() {
                    let _ = open::that_detached(dir);
                }
            });
        });
    }

    fn logout_dialog(&mut self, ctx: &egui::Context) {
        if !self.confirm_logout {
            return;
        }
        let mut close = false;
        let mut confirm = false;
        egui::Window::new("¿Cerrar sesión?")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .fixed_size([380.0, 0.0])
            .show(ctx, |ui| {
                ui.label(muted(
                    ui,
                    "Este equipo dejará de hacer copias y se desautorizará en el panel. Las copias que ya están en el disco no se borran.",
                ));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if button(ui, Kind::Danger, "Cerrar sesión").clicked() {
                        confirm = true;
                    }
                    if button(ui, Kind::Outline, "Cancelar").clicked() {
                        close = true;
                    }
                });
            });
        if confirm {
            self.logout();
        } else if close {
            self.confirm_logout = false;
        }
    }

    fn toasts(&mut self, ctx: &egui::Context) {
        let fresh: Vec<Notice> = std::mem::take(&mut self.shared.lock().unwrap().notices);
        self.toasts.extend(fresh);
        self.toasts
            .retain(|t| t.at.elapsed() < Duration::from_secs(7));
        if self.toasts.is_empty() {
            return;
        }
        let shadow = ctx.style().visuals.popup_shadow;
        egui::Area::new("toasts".into())
            .anchor(Align2::RIGHT_BOTTOM, [-20.0, -20.0])
            .show(ctx, |ui| {
                for toast in self.toasts.iter().rev().take(3) {
                    let p = palette(ui);
                    Frame::NONE
                        .fill(p.card)
                        .stroke(Stroke::new(1.0_f32, p.border))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(14, 10))
                        .shadow(shadow)
                        .show(ui, |ui| {
                            ui.set_max_width(360.0);
                            ui.horizontal_wrapped(|ui| {
                                let (icon, color) = if toast.ok {
                                    ("✔", p.success)
                                } else {
                                    ("⚠", p.danger)
                                };
                                ui.label(RichText::new(icon).color(color));
                                ui.label(&toast.text);
                            });
                        });
                    ui.add_space(6.0);
                }
            });
    }

    fn handle_close(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        let quitting = self
            .tray
            .as_ref()
            .is_none_or(|t| t.quit.load(Ordering::Relaxed));
        let keep = self.config.lock().unwrap().minimize_to_tray && self.api.is_some();
        if !quitting && keep {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            if !self.hid_once {
                self.hid_once = true;
                self.shared.lock().unwrap().notify(
                    "AGest Backup sigue funcionando en la bandeja del sistema.",
                    true,
                );
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_login(ctx);
        self.handle_close(ctx);

        let lost = self.shared.lock().unwrap().auth_lost.take();
        if let Some(message) = lost {
            self.end_session(Some(message));
        }

        let view = View::from(&self.shared.lock().unwrap());
        let has_dir = self.config.lock().unwrap().backup_dir.is_some();
        if self.api.is_none() {
            self.login_screen(ctx);
        } else if !has_dir {
            self.setup_screen(ctx, &view);
        } else {
            self.main_screen(ctx, &view);
        }
        self.logout_dialog(ctx);
        self.toasts(ctx);

        let busy = view.current.is_some() || self.login.is_some() || !self.toasts.is_empty();
        ctx.request_repaint_after(if busy {
            Duration::from_millis(100)
        } else {
            Duration::from_secs(5)
        });
    }
}

// --- Piezas sueltas -------------------------------------------------------------

fn alert(ui: &mut Ui, tone: Tone, text: &str) {
    let (bg, fg) = theme::tone_colors(palette(ui), tone);
    Frame::NONE
        .fill(bg)
        .stroke(Stroke::new(1.0_f32, fg.gamma_multiply(0.3)))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).color(fg));
        });
}

fn nav_item(ui: &mut Ui, selected: bool, icon: &str, label: &str) -> egui::Response {
    let p = palette(ui);
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, 34.0), Sense::click());
    let fill = if selected {
        p.muted
    } else if response.hovered() {
        p.muted.gamma_multiply(0.6)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, CornerRadius::same(6), fill);
    let color = if selected {
        p.foreground
    } else {
        p.muted_foreground
    };
    let font = if selected {
        theme::semibold_font(14.0)
    } else {
        egui::FontId::proportional(14.0)
    };
    ui.painter().text(
        rect.left_center() + vec2(12.0, 0.0),
        Align2::LEFT_CENTER,
        icon,
        egui::FontId::proportional(14.0),
        color,
    );
    ui.painter().text(
        rect.left_center() + vec2(36.0, 0.0),
        Align2::LEFT_CENTER,
        label,
        font,
        color,
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    ui.add_space(2.0);
    response
}

/// Logotipo: cuadrado del color primario con el símbolo de copia.
fn logo(ui: &mut Ui, size: f32) {
    let p = palette(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same((size * 0.22) as u8), p.primary);
    let c = rect.center();
    ui.painter().circle_stroke(
        c,
        size * 0.26,
        Stroke::new(size * 0.08, p.primary_foreground),
    );
    ui.painter()
        .circle_filled(c, size * 0.07, p.primary_foreground);
}
