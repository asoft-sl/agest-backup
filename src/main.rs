#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! AGest Backup: copias de seguridad de las tiendas de AGest en este equipo.

mod api;
mod app;
mod cli;
mod config;
mod oauth;
mod schedule;
mod store;
mod theme;
mod tray;
mod worker;

use std::sync::Arc;

/// Icono de la app (cuadrado oscuro redondeado con un disco blanco), en RGBA.
pub fn icon_rgba(size: u32) -> (Vec<u8>, u32) {
    let s = size as f32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    let radius = s * 0.22;
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // esquinas redondeadas
            let cx = fx.clamp(radius, s - radius);
            let cy = fy.clamp(radius, s - radius);
            let corner = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
            let inside = (radius - corner + 0.5).clamp(0.0, 1.0);
            if inside <= 0.0 {
                continue;
            }
            let d = ((fx - s / 2.0).powi(2) + (fy - s / 2.0).powi(2)).sqrt();
            let ring = (d - s * 0.26).abs() < s * 0.045;
            let dot = d < s * 0.075;
            let (r, g, b) = if ring || dot {
                (250, 250, 250)
            } else {
                (23, 23, 23)
            };
            let i = ((y * size + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&[r, g, b, (inside * 255.0) as u8]);
        }
    }
    (rgba, size)
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args
        .first()
        .is_some_and(|a| cli::COMMANDS.contains(&a.as_str()))
    {
        std::process::exit(cli::run(&args));
    }
    let start_hidden = std::env::args().any(|a| a == "--minimized");
    let (rgba, size) = icon_rgba(64);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("AGest Backup")
            .with_inner_size([1080.0, 720.0])
            .with_min_inner_size([760.0, 520.0])
            .with_visible(!start_hidden)
            .with_icon(Arc::new(egui::IconData {
                rgba,
                width: size,
                height: size,
            })),
        persist_window: false,
        ..Default::default()
    };

    eframe::run_native(
        "AGest Backup",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, start_hidden)))),
    )
}
