//! Aspecto de la app: la misma paleta y formas que el panel web de AGest
//! (shadcn/ui: neutros, primario casi negro, bordes suaves, radio 8 px).

use std::sync::Arc;

use egui::{
    Align, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Frame, Layout,
    Margin, Response, RichText, Sense, Shadow, Stroke, TextStyle, Theme, Ui, Vec2, Visuals, vec2,
};

#[derive(Clone, Copy)]
pub struct Palette {
    pub background: Color32,
    pub card: Color32,
    pub foreground: Color32,
    pub muted: Color32,
    pub muted_foreground: Color32,
    pub border: Color32,
    pub primary: Color32,
    pub primary_foreground: Color32,
    pub sidebar: Color32,
    pub ring: Color32,
    pub success: Color32,
    pub success_surface: Color32,
    pub warning: Color32,
    pub warning_surface: Color32,
    pub danger: Color32,
    pub danger_surface: Color32,
    pub info: Color32,
    pub info_surface: Color32,
}

const fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

pub const LIGHT: Palette = Palette {
    background: hex(0xFAFAFA),
    card: hex(0xFFFFFF),
    foreground: hex(0x171717),
    muted: hex(0xF5F5F5),
    muted_foreground: hex(0x666666),
    border: hex(0xE5E5E5),
    primary: hex(0x171717),
    primary_foreground: hex(0xFAFAFA),
    sidebar: hex(0xFFFFFF),
    ring: hex(0x1659E7),
    success: hex(0x0D9668),
    success_surface: hex(0xECFDF5),
    warning: hex(0xBD6304),
    warning_surface: hex(0xFFF7ED),
    danger: hex(0xC52121),
    danger_surface: hex(0xFEF2F2),
    info: hex(0x1659E7),
    info_surface: hex(0xEFF6FF),
};

pub const DARK: Palette = Palette {
    background: hex(0x121212),
    card: hex(0x1B1B1B),
    foreground: hex(0xF5F5F5),
    muted: hex(0x262626),
    muted_foreground: hex(0xA3A3A3),
    border: hex(0x333333),
    primary: hex(0xF5F5F5),
    primary_foreground: hex(0x1A1A1A),
    sidebar: hex(0x161616),
    ring: hex(0x4C82F7),
    success: hex(0x34D399),
    success_surface: hex(0x0F2A20),
    warning: hex(0xF59E0B),
    warning_surface: hex(0x2E2210),
    danger: hex(0xF87171),
    danger_surface: hex(0x2E1515),
    info: hex(0x60A5FA),
    info_surface: hex(0x132339),
};

pub fn palette(ui: &Ui) -> Palette {
    if ui.visuals().dark_mode { DARK } else { LIGHT }
}

pub const SEMIBOLD: &str = "semibold";

fn semibold() -> FontFamily {
    FontFamily::Name(SEMIBOLD.into())
}

/// Segoe UI en Windows (la fuente `system-ui` que usa la web), con las de egui de respaldo.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let candidates: &[(&str, &[&str])] = &[
        (
            "ui",
            &[
                "C:\\Windows\\Fonts\\segoeui.ttf",
                "/System/Library/Fonts/SFNS.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            ],
        ),
        (
            SEMIBOLD,
            &[
                "C:\\Windows\\Fonts\\seguisb.ttf",
                "C:\\Windows\\Fonts\\segoeuib.ttf",
                "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
            ],
        ),
        ("symbols", &["C:\\Windows\\Fonts\\seguisym.ttf"]),
    ];
    let mut loaded = Vec::new();
    for (name, paths) in candidates {
        if let Some(bytes) = paths.iter().find_map(|p| std::fs::read(p).ok()) {
            fonts
                .font_data
                .insert((*name).to_string(), Arc::new(FontData::from_owned(bytes)));
            loaded.push(*name);
        }
    }
    let defaults = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let mut proportional = Vec::new();
    if loaded.contains(&"ui") {
        proportional.push("ui".to_string());
    }
    proportional.extend(defaults.iter().cloned());
    if loaded.contains(&"symbols") {
        proportional.push("symbols".to_string());
    }
    let mut bold = Vec::new();
    if loaded.contains(&SEMIBOLD) {
        bold.push(SEMIBOLD.to_string());
    }
    bold.extend(proportional.iter().cloned());
    fonts
        .families
        .insert(FontFamily::Proportional, proportional);
    fonts.families.insert(semibold(), bold);
    ctx.set_fonts(fonts);
}

fn visuals(p: Palette, dark: bool) -> Visuals {
    let mut v = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    let radius = CornerRadius::same(6);
    v.panel_fill = p.background;
    v.window_fill = p.card;
    v.window_stroke = Stroke::new(1.0_f32, p.border);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_shadow = Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 90 } else { 28 }),
    };
    v.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 12,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 80 } else { 22 }),
    };
    v.extreme_bg_color = p.card;
    v.faint_bg_color = p.muted;
    v.code_bg_color = p.muted;
    v.hyperlink_color = p.info;
    v.selection.bg_fill = p.ring.gamma_multiply(0.25);
    v.selection.stroke = Stroke::new(1.0_f32, p.ring);
    v.override_text_color = Some(p.foreground);
    v.warn_fg_color = p.warning;
    v.error_fg_color = p.danger;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.card;
    w.noninteractive.weak_bg_fill = p.card;
    w.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0_f32, p.foreground);
    w.noninteractive.corner_radius = radius;

    for (state, fill) in [
        (&mut w.inactive, p.card),
        (&mut w.hovered, p.muted),
        (&mut w.active, p.muted),
        (&mut w.open, p.muted),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = Stroke::new(1.0_f32, p.border);
        state.fg_stroke = Stroke::new(1.0_f32, p.foreground);
        state.corner_radius = radius;
        state.expansion = 0.0;
    }
    w.hovered.bg_stroke = Stroke::new(1.0_f32, p.muted_foreground.gamma_multiply(0.5));
    w.active.bg_stroke = Stroke::new(1.0_f32, p.ring);
    v
}

pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.set_visuals_of(Theme::Light, visuals(LIGHT, false));
    ctx.set_visuals_of(Theme::Dark, visuals(DARK, true));
    for theme in [Theme::Light, Theme::Dark] {
        ctx.style_mut_of(theme, |s| {
            s.spacing.item_spacing = vec2(8.0, 8.0);
            s.spacing.button_padding = vec2(12.0, 6.0);
            s.spacing.interact_size = vec2(36.0, 32.0);
            s.spacing.combo_height = 260.0;
            s.text_styles
                .insert(TextStyle::Body, FontId::new(14.0, FontFamily::Proportional));
            s.text_styles
                .insert(TextStyle::Button, FontId::new(14.0, semibold()));
            s.text_styles.insert(
                TextStyle::Small,
                FontId::new(12.0, FontFamily::Proportional),
            );
            s.text_styles
                .insert(TextStyle::Heading, FontId::new(22.0, semibold()));
            s.text_styles.insert(
                TextStyle::Monospace,
                FontId::new(13.0, FontFamily::Monospace),
            );
            s.interaction.selectable_labels = false;
        });
    }
    ctx.options_mut(|o| o.theme_preference = egui::ThemePreference::System);
}

// --- Piezas -----------------------------------------------------------------

pub fn card_frame(ui: &Ui) -> Frame {
    let p = palette(ui);
    Frame::NONE
        .fill(p.card)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(18))
        .shadow(Shadow {
            offset: [0, 1],
            blur: 3,
            spread: 0,
            color: Color32::from_black_alpha(if ui.visuals().dark_mode { 40 } else { 10 }),
        })
}

pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    card_frame(ui)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

pub fn title(text: &str) -> RichText {
    RichText::new(text).family(semibold()).size(16.0)
}

pub fn strong(text: impl Into<String>) -> RichText {
    RichText::new(text).family(semibold())
}

pub fn muted(ui: &Ui, text: impl Into<String>) -> RichText {
    RichText::new(text).color(palette(ui).muted_foreground)
}

pub fn small_muted(ui: &Ui, text: impl Into<String>) -> RichText {
    RichText::new(text)
        .size(12.0)
        .color(palette(ui).muted_foreground)
}

pub fn page_header(ui: &mut Ui, title: &str, subtitle: &str) {
    ui.label(RichText::new(title).family(semibold()).size(24.0));
    if !subtitle.is_empty() {
        ui.label(muted(ui, subtitle));
    }
    ui.add_space(12.0);
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Primary,
    Outline,
    Danger,
    Ghost,
}

pub fn button(ui: &mut Ui, kind: Kind, text: &str) -> Response {
    button_sized(ui, kind, text, Vec2::ZERO)
}

pub fn button_sized(ui: &mut Ui, kind: Kind, text: &str, min: Vec2) -> Response {
    let p = palette(ui);
    let (fill, fg, stroke) = match kind {
        Kind::Primary => (p.primary, p.primary_foreground, Stroke::NONE),
        Kind::Outline => (p.card, p.foreground, Stroke::new(1.0_f32, p.border)),
        Kind::Danger => (p.danger, Color32::WHITE, Stroke::NONE),
        Kind::Ghost => (Color32::TRANSPARENT, p.foreground, Stroke::NONE),
    };
    let enabled = ui.is_enabled();
    let fg = if enabled { fg } else { fg.gamma_multiply(0.6) };
    let fill = if enabled {
        fill
    } else {
        fill.gamma_multiply(0.6)
    };
    let button = egui::Button::new(RichText::new(text).color(fg).family(semibold()))
        .fill(fill)
        .stroke(stroke)
        .corner_radius(CornerRadius::same(6))
        .min_size(vec2(min.x, min.y.max(34.0)));
    let response = ui.add(button);
    if response.hovered() && enabled {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

#[derive(Clone, Copy)]
pub enum Tone {
    Neutral,
    Success,
    Warning,
    Danger,
    Info,
}

pub fn tone_colors(p: Palette, tone: Tone) -> (Color32, Color32) {
    match tone {
        Tone::Neutral => (p.muted, p.muted_foreground),
        Tone::Success => (p.success_surface, p.success),
        Tone::Warning => (p.warning_surface, p.warning),
        Tone::Danger => (p.danger_surface, p.danger),
        Tone::Info => (p.info_surface, p.info),
    }
}

pub fn badge(ui: &mut Ui, tone: Tone, text: &str) -> Response {
    // pintado a mano: un Frame se estira a la altura de la fila
    let p = palette(ui);
    let (bg, fg) = tone_colors(p, tone);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), FontId::new(12.0, semibold()), fg);
    let size = galley.size() + vec2(16.0, 6.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(6),
        Stroke::new(1.0_f32, fg.gamma_multiply(0.25)),
        egui::StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, fg);
    response
}

/// Interruptor tipo «switch» del panel web.
pub fn toggle(ui: &mut Ui, on: &mut bool) -> Response {
    let p = palette(ui);
    let size = vec2(36.0, 20.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() && ui.is_enabled() {
        *on = !*on;
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let bg = if *on { p.primary } else { p.border };
        let bg = if ui.is_enabled() {
            bg
        } else {
            bg.gamma_multiply(0.5)
        };
        ui.painter().rect_filled(rect, CornerRadius::same(10), bg);
        let x = egui::lerp((rect.left() + 10.0)..=(rect.right() - 10.0), t);
        let knob = if *on {
            p.primary_foreground
        } else {
            Color32::WHITE
        };
        ui.painter()
            .circle_filled(egui::pos2(x, rect.center().y), 8.0, knob);
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// Barra de progreso plana (fracción 0..1).
pub fn progress(ui: &mut Ui, fraction: f32, color: Color32) {
    let p = palette(ui);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 8.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(4), p.muted);
    let mut fill = rect;
    fill.set_width(rect.width() * fraction.clamp(0.0, 1.0));
    if fill.width() > 0.5 {
        ui.painter().rect_filled(fill, CornerRadius::same(4), color);
    }
}

/// Barra de disco en dos tramos: copias de AGest + resto ocupado.
pub fn disk_bar(ui: &mut Ui, total: u64, free: u64, backups: u64) {
    let p = palette(ui);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 10.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(5), p.muted);
    if total == 0 {
        return;
    }
    let used = total.saturating_sub(free) as f32 / total as f32;
    let ours = (backups as f32 / total as f32).min(used);
    let low = (free as f32 / total as f32) < 0.1;
    let mut used_rect = rect;
    used_rect.set_width(rect.width() * used);
    ui.painter().rect_filled(
        used_rect,
        CornerRadius::same(5),
        if low {
            p.danger
        } else {
            p.muted_foreground.gamma_multiply(0.55)
        },
    );
    let mut ours_rect = rect;
    ours_rect.set_width((rect.width() * ours).max(if backups > 0 { 3.0 } else { 0.0 }));
    if ours_rect.width() > 0.0 {
        ui.painter()
            .rect_filled(ours_rect, CornerRadius::same(5), p.info);
    }
}

pub fn stat(ui: &mut Ui, label: &str, value: &str, hint: &str, tone: Tone) {
    card(ui, |ui| {
        let p = palette(ui);
        let (_, fg) = tone_colors(p, tone);
        ui.horizontal(|ui| {
            ui.label(RichText::new("●").size(10.0).color(fg));
            ui.label(small_muted(ui, label));
        });
        ui.label(RichText::new(value).family(semibold()).size(20.0));
        // siempre una línea de pista, para que las tarjetas midan lo mismo
        ui.label(small_muted(ui, if hint.is_empty() { " " } else { hint }));
    });
}

/// Fila «etiqueta · valor» alineada.
pub fn field_row(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [150.0, 20.0],
            egui::Label::new(muted(ui, label)).halign(Align::LEFT),
        );
        ui.with_layout(Layout::left_to_right(Align::Center), add);
    });
}

pub fn semibold_font(size: f32) -> FontId {
    FontId::new(size, semibold())
}

pub fn heading(text: &str) -> RichText {
    RichText::new(text).family(semibold()).size(24.0)
}
