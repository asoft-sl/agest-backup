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
mod update;
mod worker;

use std::sync::Arc;

/// Uvas del logo de ASoft (coordenadas sobre un lienzo de 1000×1000).
pub const GRAPES: [(f32, f32); 8] = [
    (422.0, 375.0),
    (578.0, 375.0),
    (344.0, 500.0),
    (500.0, 500.0),
    (656.0, 500.0),
    (422.0, 625.0),
    (578.0, 625.0),
    (500.0, 750.0),
];
pub const GRAPE_R: f32 = 62.0;

/// ¿El punto (en coordenadas del logo) cae dentro de las uvas o del rabillo?
fn in_grapes(x: f32, y: f32) -> bool {
    if GRAPES
        .iter()
        .any(|(cx, cy)| (x - cx).powi(2) + (y - cy).powi(2) <= GRAPE_R * GRAPE_R)
    {
        return true;
    }
    // Rabillo: curva cuadrática de (498, 312) a (562, 168), 48 de grosor.
    if !(150.0..=320.0).contains(&y) {
        return false;
    }
    let (p0, p1, p2) = (
        (498.0f32, 312.0f32),
        (506.0f32, 215.0f32),
        (562.0f32, 168.0f32),
    );
    (0..=24).any(|i| {
        let t = i as f32 / 24.0;
        let u = 1.0 - t;
        let bx = u * u * p0.0 + 2.0 * u * t * p1.0 + t * t * p2.0;
        let by = u * u * p0.1 + 2.0 * u * t * p1.1 + t * t * p2.1;
        (x - bx).powi(2) + (y - by).powi(2) <= 24.0 * 24.0 && y <= 312.0
    })
}

/// Icono de la app: las uvas de ASoft en blanco sobre un cuadrado oscuro redondeado.
pub fn icon_rgba(size: u32) -> (Vec<u8>, u32) {
    icon_with_badge(size, None)
}

/// Igual que [`icon_rgba`], con un punto de color abajo a la derecha (estado).
pub fn icon_with_badge(size: u32, badge: Option<[u8; 3]>) -> (Vec<u8>, u32) {
    const SS: u32 = 4; // submuestreo para suavizar bordes
    let s = size as f32;
    let radius = s * 0.22;
    // Las uvas ocupan del y=158 al y=812 del logo; las centramos en el cuadrado.
    let k = 0.76 * s / 654.0;
    let (ox, oy) = (500.0, 485.0);
    let (bx, by, br) = (s * 0.8, s * 0.8, s * 0.17);
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let (mut bg, mut fg, mut dot) = (0u32, 0u32, 0u32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let fx = x as f32 + (sx as f32 + 0.5) / SS as f32;
                    let fy = y as f32 + (sy as f32 + 0.5) / SS as f32;
                    if badge.is_some() {
                        let d = ((fx - bx).powi(2) + (fy - by).powi(2)).sqrt();
                        if d <= br {
                            dot += 1;
                            continue;
                        }
                        if d <= br + s * 0.06 {
                            continue; // hueco transparente alrededor del punto
                        }
                    }
                    let cx = fx.clamp(radius, s - radius);
                    let cy = fy.clamp(radius, s - radius);
                    if (fx - cx).powi(2) + (fy - cy).powi(2) > radius * radius {
                        continue;
                    }
                    bg += 1;
                    if in_grapes(ox + (fx - s / 2.0) / k, oy + (fy - s / 2.0) / k) {
                        fg += 1;
                    }
                }
            }
            let n = (SS * SS) as f32;
            let [dr, dg, db] = badge.unwrap_or([0, 0, 0]);
            // Mezcla: fondo #171717, uvas blancas y punto de estado.
            let a = (bg + dot) as f32 / n;
            if a <= 0.0 {
                continue;
            }
            let w = |v: u8, cnt: u32| v as f32 * cnt as f32;
            let total = (bg + dot) as f32;
            let mix = |base: u8, dotc: u8| {
                ((w(base, bg - fg) + w(250, fg) + w(dotc, dot)) / total).round() as u8
            };
            let i = ((y * size + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&[
                mix(23, dr),
                mix(23, dg),
                mix(23, db),
                (a * 255.0).round() as u8,
            ]);
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
    update::init(&args);
    let start_hidden = args.iter().any(|a| a == "--minimized");
    update::WINDOW_HIDDEN.store(start_hidden, std::sync::atomic::Ordering::Relaxed);
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
