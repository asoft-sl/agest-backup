//! Icono en la bandeja del sistema: la app sigue copiando con la ventana
//! cerrada. Muestra el estado de las copias (en el menú, la información sobre
//! herramientas y un punto de color en el icono). Clic en el icono o «Abrir»
//! muestra la ventana; «Salir» la cierra.
//!
//! Con la ventana oculta, eframe no ejecuta `update()` y por tanto no aplica
//! los `ViewportCommand`. Por eso en Windows la ventana se muestra con la API
//! nativa y el estado se refresca con un temporizador del hilo principal.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Local};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::config::Config;
use crate::schedule::next_slot;
use crate::worker::{Shared, Stage};

const ICON_SIZE: u32 = 32;

pub struct Tray {
    pub quit: Arc<AtomicBool>,
}

/// Estado general que se refleja en el icono.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Idle,
    Ok,
    Busy,
    Warning,
    Error,
}

impl Level {
    fn badge(self) -> Option<[u8; 3]> {
        match self {
            Level::Idle => None,
            Level::Ok => Some([34, 197, 94]),
            Level::Busy => Some([59, 130, 246]),
            Level::Warning => Some([245, 158, 11]),
            Level::Error => Some([239, 68, 68]),
        }
    }
}

/// Resumen del estado de las copias para la bandeja.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub level: Level,
    pub headline: String,
    pub last: String,
    pub next: String,
}

fn when(t: DateTime<Local>, now: DateTime<Local>) -> String {
    let day = t.date_naive();
    let today = now.date_naive();
    if day == today {
        format!("hoy {}", t.format("%H:%M"))
    } else if Some(day) == today.pred_opt() {
        format!("ayer {}", t.format("%H:%M"))
    } else if Some(day) == today.succ_opt() {
        format!("mañana {}", t.format("%H:%M"))
    } else {
        t.format("%d/%m/%Y %H:%M").to_string()
    }
}

pub fn status(s: &Shared, cfg: &Config, now: DateTime<Local>) -> Status {
    let last = s
        .index
        .entries
        .iter()
        .max_by_key(|e| e.completed_at)
        .map(|e| {
            format!(
                "Última copia: {} · {}",
                e.shop_name,
                when(e.completed_at, now)
            )
        })
        .unwrap_or_else(|| "Última copia: ninguna todavía".into());

    let next = s
        .state
        .as_ref()
        .map(|st| {
            st.schedules
                .iter()
                .filter(|sc| sc.enabled)
                .filter_map(|sc| next_slot(sc, now).map(|t| (t, sc.shop_id)))
                .min_by_key(|(t, _)| *t)
        })
        .unwrap_or(None)
        .map(|(t, shop)| format!("Próxima copia: {} · {}", s.shop_name(shop), when(t, now)))
        .unwrap_or_else(|| "Próxima copia: sin programar".into());

    let (level, headline) = if s.state.is_none() && !s.connected && s.last_error.is_none() {
        (Level::Idle, "Sin sesión iniciada".to_string())
    } else if let Some(p) = &s.current {
        let stage = match p.stage {
            Stage::Waiting => "preparando".to_string(),
            Stage::Building => "el servidor genera la copia".to_string(),
            Stage::Downloading => match p.total {
                Some(t) if t > 0 => format!("descargando {}%", p.downloaded * 100 / t),
                _ => "descargando".to_string(),
            },
            Stage::Verifying => "comprobando".to_string(),
        };
        let queued = if s.queued.is_empty() {
            String::new()
        } else {
            format!(" (+{} en cola)", s.queued.len())
        };
        (
            Level::Busy,
            format!("Copiando «{}»: {stage}{queued}", p.shop_name),
        )
    } else if cfg.backup_dir.is_none() {
        (Level::Warning, "Falta elegir la carpeta de copias".into())
    } else if let Some((_, shop, t)) = &s.last_failure {
        (
            Level::Error,
            format!("Falló la copia de «{shop}» ({})", when(*t, now)),
        )
    } else if !s.connected && s.last_error.is_some() {
        (Level::Warning, "Sin conexión con AGest".into())
    } else {
        (Level::Ok, "Copias al día".into())
    };

    Status {
        level,
        headline,
        last,
        next,
    }
}

struct Live {
    icon: TrayIcon,
    headline: MenuItem,
    last: MenuItem,
    next: MenuItem,
    shared: Arc<Mutex<Shared>>,
    config: Arc<Mutex<Config>>,
    shown: Option<Status>,
}

thread_local! {
    // TrayIcon y MenuItem no son Send: viven en el hilo principal.
    static LIVE: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Vuelve a calcular el estado y actualiza icono, tooltip y menú si cambió.
/// Debe llamarse desde el hilo principal.
pub fn refresh() {
    let install = LIVE.with(|live| {
        let mut live = live.borrow_mut();
        let Some(live) = live.as_mut() else {
            return false;
        };
        let (st, install) = {
            let cfg = live.config.lock().unwrap().clone();
            let s = live.shared.lock().unwrap();
            let busy = s.current.is_some() || !s.queued.is_empty();
            (
                status(&s, &cfg, Local::now()),
                crate::update::should_auto_install(cfg.auto_update, busy),
            )
        };
        if install {
            return true;
        }
        if live.shown.as_ref() == Some(&st) {
            return false;
        }
        live.headline.set_text(&st.headline);
        live.last.set_text(&st.last);
        live.next.set_text(&st.next);
        let tip = format!("AGest Backup\n{}\n{}", st.headline, st.last);
        // Windows corta la información sobre herramientas a 127 caracteres.
        let tip: String = tip.chars().take(127).collect();
        let _ = live.icon.set_tooltip(Some(tip));
        if live.shown.as_ref().map(|s| s.level) != Some(st.level) {
            let (rgba, size) = crate::icon_with_badge(ICON_SIZE, st.level.badge());
            if let Ok(icon) = tray_icon::Icon::from_rgba(rgba, size, size) {
                let _ = live.icon.set_icon(Some(icon));
            }
        }
        live.shown = Some(st);
        false
    });
    // Actualización descargada y app en la bandeja sin copias: se instala y
    // se arranca la versión nueva, también en la bandeja.
    if install && crate::update::install_and_relaunch(true).is_ok() {
        exit_now();
    }
}

/// Quita el icono de la bandeja (si no, en Windows queda un icono fantasma) y
/// termina el proceso.
pub fn exit_now() -> ! {
    LIVE.with(|live| drop(live.borrow_mut().take()));
    std::process::exit(0);
}

pub fn create(
    cc: &eframe::CreationContext<'_>,
    shared: Arc<Mutex<Shared>>,
    config: Arc<Mutex<Config>>,
) -> Option<Tray> {
    let ctx = cc.egui_ctx.clone();
    let headline = MenuItem::new("AGest Backup", false, None);
    let last = MenuItem::new("Última copia: —", false, None);
    let next = MenuItem::new("Próxima copia: —", false, None);
    let open = MenuItem::new("Abrir AGest Backup", true, None);
    let quit_item = MenuItem::new("Salir", true, None);
    let menu = Menu::new();
    menu.append(&headline).ok()?;
    menu.append(&last).ok()?;
    menu.append(&next).ok()?;
    menu.append(&PredefinedMenuItem::separator()).ok()?;
    menu.append(&open).ok()?;
    menu.append(&PredefinedMenuItem::separator()).ok()?;
    menu.append(&quit_item).ok()?;

    let (rgba, size) = crate::icon_rgba(ICON_SIZE);
    let icon = tray_icon::Icon::from_rgba(rgba, size, size).ok()?;
    let tray = TrayIconBuilder::new()
        .with_tooltip("AGest Backup")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
        .ok()?;

    let window = native::Window::from(cc);
    let quit = Arc::new(AtomicBool::new(false));

    {
        let ctx = ctx.clone();
        let quit = quit.clone();
        let (open_id, quit_id) = (open.id().clone(), quit_item.id().clone());
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if event.id == open_id {
                window.show(&ctx);
            } else if event.id == quit_id {
                quit.store(true, Ordering::Relaxed);
                // La ventana tiene que estar visible para que eframe procese el cierre.
                window.show(&ctx);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                ctx.request_repaint();
            }
        }));
    }
    {
        let ctx = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| match event {
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
            | TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } => window.show(&ctx),
            // Antes de abrir el menú contextual: que muestre el estado actual.
            TrayIconEvent::Click {
                button: MouseButton::Right,
                button_state: MouseButtonState::Down,
                ..
            }
            | TrayIconEvent::Enter { .. } => refresh(),
            _ => {}
        }));
    }

    LIVE.with(|live| {
        *live.borrow_mut() = Some(Live {
            icon: tray,
            headline,
            last,
            next,
            shared,
            config,
            shown: None,
        })
    });
    refresh();
    native::start_refresh_timer();

    Some(Tray { quit })
}

#[cfg(windows)]
mod native {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IsIconic, SW_RESTORE, SW_SHOW, SetForegroundWindow, SetTimer, ShowWindow,
    };

    /// HWND de la ventana principal (como entero, para poder moverlo entre hilos).
    #[derive(Clone, Copy)]
    pub struct Window(Option<isize>);

    impl Window {
        pub fn from(cc: &eframe::CreationContext<'_>) -> Self {
            let hwnd = cc.window_handle().ok().and_then(|h| match h.as_raw() {
                RawWindowHandle::Win32(w) => Some(w.hwnd.get()),
                _ => None,
            });
            Self(hwnd)
        }

        pub fn show(&self, ctx: &egui::Context) {
            crate::update::WINDOW_HIDDEN.store(false, std::sync::atomic::Ordering::Relaxed);
            if let Some(hwnd) = self.0 {
                let hwnd = hwnd as HWND;
                unsafe {
                    ShowWindow(
                        hwnd,
                        if IsIconic(hwnd) != 0 {
                            SW_RESTORE
                        } else {
                            SW_SHOW
                        },
                    );
                    SetForegroundWindow(hwnd);
                }
            }
            // Para que egui sepa que vuelve a estar visible.
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            ctx.request_repaint();
        }
    }

    unsafe extern "system" fn on_timer(_: HWND, _: u32, _: usize, _: u32) {
        super::refresh();
    }

    /// Temporizador del hilo principal: el bucle de mensajes de winit lo atiende
    /// aunque la ventana esté oculta.
    pub fn start_refresh_timer() {
        unsafe {
            SetTimer(std::ptr::null_mut(), 0, 2000, Some(on_timer));
        }
    }
}

#[cfg(not(windows))]
mod native {
    #[derive(Clone, Copy)]
    pub struct Window;

    impl Window {
        pub fn from(_: &eframe::CreationContext<'_>) -> Self {
            Self
        }

        pub fn show(&self, ctx: &egui::Context) {
            crate::update::WINDOW_HIDDEN.store(false, std::sync::atomic::Ordering::Relaxed);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            ctx.request_repaint();
        }
    }

    /// Fuera de Windows el estado se refresca desde `App::update`.
    pub fn start_refresh_timer() {}
}
