//! Icono en la bandeja del sistema: la app sigue copiando con la ventana
//! cerrada. Clic en el icono o «Abrir» muestra la ventana; «Salir» la cierra.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

pub struct Tray {
    _icon: TrayIcon,
    pub quit: Arc<AtomicBool>,
}

pub fn create(ctx: &egui::Context) -> Option<Tray> {
    let open = MenuItem::new("Abrir AGest Backup", true, None);
    let quit_item = MenuItem::new("Salir", true, None);
    let menu = Menu::new();
    menu.append(&open).ok()?;
    menu.append(&PredefinedMenuItem::separator()).ok()?;
    menu.append(&quit_item).ok()?;

    let (rgba, size) = crate::icon_rgba(32);
    let icon = tray_icon::Icon::from_rgba(rgba, size, size).ok()?;
    let tray = TrayIconBuilder::new()
        .with_tooltip("AGest Backup")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
        .ok()?;

    let quit = Arc::new(AtomicBool::new(false));
    let show = |ctx: &egui::Context| {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    };

    {
        let ctx = ctx.clone();
        let quit = quit.clone();
        let (open_id, quit_id) = (open.id().clone(), quit_item.id().clone());
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if event.id == open_id {
                show(&ctx);
            } else if event.id == quit_id {
                quit.store(true, Ordering::Relaxed);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                ctx.request_repaint();
            }
        }));
    }
    {
        let ctx = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show(&ctx);
            }
        }));
    }

    Some(Tray { _icon: tray, quit })
}
