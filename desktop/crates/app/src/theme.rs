//! Tokens del diseño «Camdesk — consola oscura»: colores, fuentes y estilo de egui.
//! Un solo acento azul para acciones. Verde, ámbar y rojo solo para estado.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke};

const fn hex(value: u32) -> Color32 {
    Color32::from_rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

// Superficies
pub const STAGE: Color32 = hex(0x0B0C0F);
pub const VIEWPORT: Color32 = hex(0x07080A);
pub const PANEL: Color32 = hex(0x121318);
pub const INSET: Color32 = hex(0x0E0F13);
pub const TITLEBAR: Color32 = hex(0x101115);
pub const FOOTER: Color32 = hex(0x0E0F12);

// Bordes y rellenos
pub const BORDER: Color32 = hex(0x1F2229);
pub const FIELD_BORDER: Color32 = hex(0x262A33);
pub const SEGMENT_BORDER: Color32 = hex(0x22252D);
pub const GHOST_BORDER: Color32 = hex(0x2A2E37);
pub const NEUTRAL: Color32 = hex(0x1A1C22);
pub const NEUTRAL_BORDER: Color32 = hex(0x2E323C);
pub const SELECTED: Color32 = hex(0x262A33);
pub const RAIL: Color32 = hex(0x1C1F26);
pub const OFF: Color32 = hex(0x5A606B);

// Texto
pub const TEXT: Color32 = hex(0xE6E8EC);
pub const TEXT_STRONG: Color32 = hex(0xF2F4F7);
pub const TEXT_SOFT: Color32 = hex(0xA3AAB6);
pub const MUTED: Color32 = hex(0x8B93A1);
pub const INK: Color32 = hex(0x0B0D10);

// Acción y estado
pub const ACCENT: Color32 = hex(0x4C8DFF);
pub const LINK: Color32 = hex(0x7FB0FF);
pub const LIVE: Color32 = hex(0x3FCF8E);
pub const LIVE_TEXT: Color32 = hex(0x7FE3B2);
pub const WARN: Color32 = hex(0xF5B84A);
pub const REC: Color32 = hex(0xFF5A5F);
pub const REC_TEXT: Color32 = hex(0xFF9A9D);

// Medidas
pub const HEADER_HEIGHT: f32 = 44.0;
pub const FOOTER_HEIGHT: f32 = 28.0;
pub const BUTTON_HEIGHT: f32 = 44.0;
pub const FIELD_HEIGHT: f32 = 34.0;
pub const PREVIEW_MAX_WIDTH: f32 = 900.0;

pub fn body(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}

pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// El diseño pide Inter y JetBrains Mono. No se descargan ni se empaquetan: se usan las
/// instaladas en Windows y, si faltan, Segoe UI y Cascadia Mono.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut add = |name: &str, files: &[&str], family: FontFamily| {
        let Some(bytes) = files.iter().find_map(|file| read_font(file)) else { return };
        fonts.font_data.insert(name.to_string(), Arc::new(FontData::from_owned(bytes)));
        fonts.families.entry(family).or_default().insert(0, name.to_string());
    };
    add("ui", &["Inter-Regular.ttf", "segoeui.ttf"], FontFamily::Proportional);
    add("ui-mono", &["JetBrainsMono-Regular.ttf", "CascadiaMono.ttf", "consola.ttf"], FontFamily::Monospace);
    // La familia tiene que existir aunque no haya una seminegrita: cae en la proporcional.
    let fallback = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    fonts.families.insert(FontFamily::Name("semibold".into()), fallback);
    if let Some(bytes) = ["Inter-SemiBold.ttf", "seguisb.ttf"].iter().find_map(|file| read_font(file)) {
        fonts.font_data.insert("ui-semibold".into(), Arc::new(FontData::from_owned(bytes)));
        fonts.families.entry(FontFamily::Name("semibold".into())).or_default().insert(0, "ui-semibold".into());
    }
    ctx.set_fonts(fonts);
}

fn read_font(file: &str) -> Option<Vec<u8>> {
    let mut dirs = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join("Microsoft").join("Windows").join("Fonts"));
    }
    let windows = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    dirs.push(windows.join("Fonts"));
    dirs.iter().find_map(|dir| std::fs::read(dir.join(file)).ok())
}

pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    let mut style = (*ctx.style()).clone();
    let visuals = &mut style.visuals;
    visuals.dark_mode = true;
    visuals.panel_fill = STAGE;
    visuals.window_fill = PANEL;
    visuals.window_stroke = Stroke::new(1.0_f32, FIELD_BORDER);
    visuals.extreme_bg_color = STAGE;
    visuals.faint_bg_color = INSET;
    visuals.override_text_color = Some(TEXT);
    visuals.hyperlink_color = LINK;
    visuals.selection.bg_fill = SELECTED;
    visuals.selection.stroke = Stroke::new(1.0_f32, ACCENT);
    let radius = CornerRadius::same(7);
    let widgets = &mut visuals.widgets;
    for state in [
        &mut widgets.noninteractive,
        &mut widgets.inactive,
        &mut widgets.hovered,
        &mut widgets.active,
        &mut widgets.open,
    ] {
        state.corner_radius = radius;
        state.fg_stroke = Stroke::new(1.0_f32, TEXT);
        state.bg_fill = STAGE;
        state.weak_bg_fill = STAGE;
        state.bg_stroke = Stroke::new(1.0_f32, FIELD_BORDER);
        state.expansion = 0.0;
    }
    widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    widgets.hovered.bg_stroke = Stroke::new(1.0_f32, NEUTRAL_BORDER);
    widgets.hovered.weak_bg_fill = NEUTRAL;
    widgets.active.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    widgets.open.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    style.spacing.interact_size.y = FIELD_HEIGHT;
    style.text_styles.insert(egui::TextStyle::Body, body(13.0));
    style.text_styles.insert(egui::TextStyle::Button, body(13.0));
    style.text_styles.insert(egui::TextStyle::Small, body(11.0));
    style.text_styles.insert(egui::TextStyle::Monospace, mono(12.0));
    ctx.set_style(style);
}

/// Color del aviso. Se decide por el título, que es lo que la app ya compara.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Ok,
    Info,
    Warn,
    Error,
    Rec,
    Idle,
}

impl Tone {
    pub fn of(title: &str) -> Self {
        let lower = title.to_lowercase();
        if lower.starts_with("grabando") {
            Self::Rec
        } else if lower.starts_with("en vivo") {
            Self::Ok
        } else if lower.starts_with("teléfono vertical") || lower.starts_with("conectando") {
            Self::Info
        } else if lower.starts_with("último cuadro") || lower.starts_with("grabación guardada") || lower.starts_with("listo") {
            Self::Idle
        } else if lower.contains("incompatible") || lower.contains("error") {
            Self::Error
        } else {
            Self::Warn
        }
    }

    /// (fondo, borde, título)
    pub fn colors(self) -> (Color32, Color32, Color32) {
        match self {
            Self::Ok => (hex(0x0F1D17), hex(0x1F3A2E), LIVE_TEXT),
            Self::Info => (hex(0x0F1626), hex(0x1F2C47), hex(0xA9C8FF)),
            Self::Warn => (hex(0x1E180D), hex(0x3D3119), hex(0xF5C66E)),
            Self::Error | Self::Rec => (hex(0x20121A), hex(0x3E1F27), REC_TEXT),
            Self::Idle => (INSET, BORDER, TEXT),
        }
    }
}

/// Punto del pie de estado.
pub fn status_dot(status: &str) -> Color32 {
    if status.starts_with("En vivo") {
        LIVE
    } else if status == "Conectando" {
        ACCENT
    } else if status == "Ocupado" || status == "Teléfono no autorizado" {
        WARN
    } else if status == "Versión incompatible" {
        REC
    } else {
        OFF
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cada_aviso_tiene_su_tono() {
        for (title, tone) in [
            ("En vivo", Tone::Ok),
            ("Teléfono vertical", Tone::Info),
            ("Teléfono ocupado", Tone::Warn),
            ("Teléfono no autorizado", Tone::Warn),
            ("Versión incompatible", Tone::Error),
            ("Modo rendimiento", Tone::Warn),
            ("Sin desenfoque", Tone::Warn),
            ("Último cuadro", Tone::Idle),
            ("Grabación guardada", Tone::Idle),
            ("Grabando 00:08", Tone::Rec),
            ("Grabando sin cámara", Tone::Rec),
            ("Falta una IP válida", Tone::Warn),
        ] {
            assert_eq!(Tone::of(title), tone, "{title}");
        }
    }
}
