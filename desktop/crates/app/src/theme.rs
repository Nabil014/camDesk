//! Tema de Camdesk: consola oscura inspirada en OBS. Vista previa arriba, paneles abajo y pie de estado.
//!
//! Un solo acento azul para las acciones. Verde, ámbar y rojo quedan para el estado.
//! Las fuentes salen de las que ya tiene Windows (Segoe UI y Cascadia Mono o Consolas). Si no
//! están, quedan las de egui. No se descarga nada y no se empaquetan fuentes.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontFamily, FontId, Layout, Margin, Pos2, Rect, Response, RichText,
    Sense, Stroke, StrokeKind, Ui, WidgetInfo, WidgetType,
};

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// Fondo del escenario de la vista previa.
pub const STAGE: Color32 = rgb(0x0b, 0x0c, 0x0f);
/// Barra de título.
pub const TITLE: Color32 = rgb(0x10, 0x11, 0x15);
/// Fondo de los paneles de abajo.
pub const PANEL: Color32 = rgb(0x12, 0x13, 0x18);
/// Tarjeta dentro de un panel.
pub const CARD: Color32 = rgb(0x0e, 0x0f, 0x13);
/// Pie de estado.
pub const FOOTER: Color32 = rgb(0x0e, 0x0f, 0x12);
/// Campos de texto, desplegables y selectores.
pub const FIELD: Color32 = rgb(0x0b, 0x0c, 0x0f);
/// Interior del cuadro de la vista previa.
pub const FRAME: Color32 = rgb(0x07, 0x08, 0x0a);
pub const BORDER: Color32 = rgb(0x1f, 0x22, 0x29);
pub const FIELD_BORDER: Color32 = rgb(0x26, 0x2a, 0x33);
pub const RAISED: Color32 = rgb(0x26, 0x2a, 0x33);
pub const METER_TRACK: Color32 = rgb(0x1c, 0x1f, 0x26);

pub const TEXT: Color32 = rgb(0xe6, 0xe8, 0xec);
pub const TEXT_BRIGHT: Color32 = rgb(0xf2, 0xf4, 0xf7);
/// Texto secundario: títulos de panel, epígrafe.
pub const MUTED: Color32 = rgb(0x8b, 0x93, 0xa1);
/// Etiquetas de los controles y cuerpo de los avisos.
pub const SOFT: Color32 = rgb(0xa3, 0xaa, 0xb6);
/// Texto oscuro sobre el acento o el rojo.
pub const INK: Color32 = rgb(0x0b, 0x0d, 0x10);

pub const ACCENT: Color32 = rgb(0x4c, 0x8d, 0xff);
pub const ACCENT_TEXT: Color32 = rgb(0x7f, 0xb0, 0xff);
pub const LIVE: Color32 = rgb(0x3f, 0xcf, 0x8e);
pub const LIVE_TEXT: Color32 = rgb(0x7f, 0xe3, 0xb2);
pub const WARN: Color32 = rgb(0xf5, 0xb8, 0x4a);
pub const WARN_TEXT: Color32 = rgb(0xf5, 0xc6, 0x6e);
pub const REC: Color32 = rgb(0xff, 0x5a, 0x5f);
pub const REC_TEXT: Color32 = rgb(0xff, 0x9a, 0x9d);
/// Punto de estado apagado.
pub const IDLE: Color32 = rgb(0x5a, 0x60, 0x6b);

const SEMIBOLD: &str = "semibold";

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    let mut style = (*ctx.style()).clone();
    let visuals = &mut style.visuals;
    visuals.dark_mode = true;
    visuals.panel_fill = PANEL;
    visuals.window_fill = PANEL;
    visuals.window_stroke = Stroke::new(1.0_f32, FIELD_BORDER);
    visuals.window_corner_radius = CornerRadius::same(8);
    visuals.menu_corner_radius = CornerRadius::same(8);
    visuals.extreme_bg_color = FIELD;
    visuals.faint_bg_color = CARD;
    visuals.override_text_color = Some(TEXT);
    visuals.hyperlink_color = ACCENT_TEXT;
    visuals.selection.bg_fill = rgb(0x2d, 0x5b, 0xa8);
    visuals.selection.stroke = Stroke::new(1.0_f32, ACCENT);
    visuals.slider_trailing_fill = true;

    let radius = CornerRadius::same(7);
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = radius;
        widget.fg_stroke.color = TEXT;
        widget.expansion = 0.0;
    }
    visuals.widgets.noninteractive.bg_fill = PANEL;
    visuals.widgets.noninteractive.weak_bg_fill = PANEL;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    // Desplegables y campos de texto: fondo hundido con borde fino.
    visuals.widgets.inactive.bg_fill = RAISED;
    visuals.widgets.inactive.weak_bg_fill = FIELD;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, FIELD_BORDER);
    visuals.widgets.hovered.bg_fill = rgb(0x31, 0x36, 0x41);
    visuals.widgets.hovered.weak_bg_fill = rgb(0x15, 0x17, 0x1c);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, rgb(0x3a, 0x40, 0x4c));
    visuals.widgets.active.bg_fill = rgb(0x3a, 0x40, 0x4c);
    visuals.widgets.active.weak_bg_fill = rgb(0x1a, 0x1c, 0x22);
    visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, ACCENT);
    visuals.widgets.open.bg_fill = FIELD;
    visuals.widgets.open.weak_bg_fill = FIELD;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0_f32, ACCENT);

    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    style.spacing.interact_size.y = 30.0;
    style.spacing.slider_rail_height = 4.0;
    style.spacing.combo_height = 240.0;

    use egui::TextStyle;
    style.text_styles = [
        (TextStyle::Small, FontId::proportional(11.0)),
        (TextStyle::Body, FontId::proportional(13.0)),
        (TextStyle::Button, FontId::proportional(13.0)),
        (TextStyle::Heading, semibold(15.0)),
        (TextStyle::Monospace, FontId::monospace(12.0)),
    ]
    .into();
    ctx.set_style(style);
}

/// Segoe UI, Segoe UI Semibold y Cascadia Mono (o Consolas) desde `%WINDIR%\Fonts`.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let dir = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("Fonts");
    let load = |fonts: &mut egui::FontDefinitions, key: &str, file: &str| match std::fs::read(dir.join(file)) {
        Ok(bytes) => {
            fonts.font_data.insert(key.into(), Arc::new(egui::FontData::from_owned(bytes)));
            true
        }
        Err(_) => false,
    };
    let regular = load(&mut fonts, "segoe", "segoeui.ttf");
    let bold = load(&mut fonts, "segoe-semibold", "seguisb.ttf");
    let mono = load(&mut fonts, "mono", "CascadiaMono.ttf") || load(&mut fonts, "mono", "consola.ttf");

    let mut proportional = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let mut strong = proportional.clone();
    if regular {
        proportional.insert(0, "segoe".into());
    }
    if bold {
        strong.insert(0, "segoe-semibold".into());
    } else if regular {
        strong.insert(0, "segoe".into());
    }
    fonts.families.insert(FontFamily::Proportional, proportional);
    fonts.families.insert(FontFamily::Name(SEMIBOLD.into()), strong);
    if mono {
        fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "mono".into());
    }
    ctx.set_fonts(fonts);
}

/// Fondo de un panel de abajo, a lo alto de toda la fila.
pub fn dock(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(PANEL)
        .inner_margin(Margin { left: 16, right: 16, top: 14, bottom: 12 })
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            egui::ScrollArea::vertical()
                .id_salt(id)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // `columns` justifica el texto; los paneles van alineados a la izquierda.
                    ui.with_layout(Layout::top_down(Align::Min), |ui| {
                        ui.spacing_mut().item_spacing.y = 10.0;
                        add(ui);
                    });
                });
        });
}

/// Fila de título de un panel, de 22 px en todos, con lugar a la derecha para un botón chico.
pub fn dock_header<R>(ui: &mut Ui, title: &str, right: impl FnOnce(&mut Ui) -> R) -> R {
    let size = egui::vec2(ui.available_width(), 22.0);
    ui.allocate_ui_with_layout(size, Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_size(size);
        ui.label(dock_title(title));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().interact_size.y = 22.0;
            ui.spacing_mut().button_padding = egui::vec2(8.0, 2.0);
            right(ui)
        })
        .inner
    })
    .inner
}

/// Título de un panel: mayúsculas chicas, separadas.
pub fn dock_title(title: &str) -> RichText {
    RichText::new(title.to_uppercase())
        .font(semibold(11.0))
        .color(MUTED)
        .extra_letter_spacing(0.7)
}

/// Etiqueta encima de un control.
pub fn field_label(ui: &mut Ui, text: &str) -> Response {
    ui.label(RichText::new(text).size(12.0).color(SOFT))
}

pub fn segments<T: Copy + PartialEq>(ui: &mut Ui, value: &mut T, options: &[(&str, T)]) -> bool {
    let mut changed = false;
    egui::Frame::new()
        .fill(FIELD)
        .stroke(Stroke::new(1.0_f32, rgb(0x22, 0x25, 0x2d)))
        .corner_radius(8)
        .inner_margin(Margin::same(2))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.columns(options.len(), |cols| {
                for (col, (label, option)) in cols.iter_mut().zip(options) {
                    let selected = *value == *option;
                    let text = RichText::new(*label)
                        .font(semibold(12.0))
                        .color(if selected { TEXT_BRIGHT } else { MUTED });
                    let button = egui::Button::new(text)
                        .fill(if selected { RAISED } else { Color32::TRANSPARENT })
                        .stroke(Stroke::NONE)
                        .corner_radius(6)
                        .wrap_mode(egui::TextWrapMode::Extend)
                        .selected(selected)
                        .min_size(egui::vec2(col.available_width(), 28.0));
                    if col.add(button).clicked() && !selected {
                        *value = *option;
                        changed = true;
                    }
                }
            });
        });
    changed
}

/// Interruptor pintado a mano: etiqueta a la izquierda, llave a la derecha.
pub fn toggle(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 26.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    // Sin esto, un lector de pantalla no sabe qué casilla es ni si está prendida.
    let (enabled, selected) = (ui.is_enabled(), *on);
    response.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, enabled, selected, label));
    let hovered = response.hovered() && enabled;
    let painter = ui.painter();
    painter.text(rect.left_center(), Align2::LEFT_CENTER, label, FontId::proportional(13.0), TEXT);
    let switch = Rect::from_center_size(egui::pos2(rect.right() - 16.0, rect.center().y), egui::vec2(32.0, 18.0));
    let track = match (*on, hovered) {
        (true, false) => ACCENT,
        (true, true) => rgb(0x6a, 0xa1, 0xff),
        (false, false) => RAISED,
        (false, true) => rgb(0x31, 0x36, 0x41),
    };
    painter.rect_filled(switch, 9.0, track);
    let knob_x = if *on { switch.right() - 9.0 } else { switch.left() + 9.0 };
    let knob = if *on { TEXT_BRIGHT } else { MUTED };
    painter.circle_filled(egui::pos2(knob_x, switch.center().y), 6.5, knob);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Etiqueta y valor en una fila, y el deslizador a todo el ancho debajo.
pub fn slider_row(
    ui: &mut Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    shown: impl Fn(f32) -> String,
) -> bool {
    ui.spacing_mut().item_spacing.y = 4.0;
    let label_id = ui
        .horizontal(|ui| {
            let label = field_label(ui, label);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(shown(*value)).font(FontId::monospace(12.0)).color(SOFT));
            });
            label.id
        })
        .inner;
    let response = ui
        .scope(|ui| {
            ui.spacing_mut().slider_width = ui.available_width();
            ui.spacing_mut().interact_size.y = 18.0;
            ui.add(egui::Slider::new(value, range).show_value(false).trailing_fill(true))
        })
        .inner
        .labelled_by(label_id);
    ui.spacing_mut().item_spacing.y = 10.0;
    response.changed()
}

pub fn level_meter(ui: &mut Ui, level: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 6.0), Sense::hover());
    ui.painter().rect_filled(rect, 3.0, METER_TRACK);
    let fill = rect.width() * level.clamp(0.0, 1.0);
    if fill > 0.5 {
        let color = if level > 0.94 { REC } else if level > 0.8 { WARN } else { LIVE };
        ui.painter().rect_filled(Rect::from_min_size(rect.min, egui::vec2(fill, rect.height())), 3.0, color);
    }
}

#[derive(Clone, Copy)]
pub enum Action {
    /// Conectar: el acento.
    Primary,
    /// Desconectar o Cancelar.
    Quiet,
    /// Grabar: rojo lleno con un punto.
    Record,
    /// Detener: borde rojo con un cuadrado.
    Stop,
}

/// Botón grande de los controles, 44 px de alto.
pub fn action_button(ui: &mut Ui, text: &str, kind: Action) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 44.0), Sense::click());
    let enabled = ui.is_enabled();
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, text));
    let hovered = response.hovered() && enabled;
    let (fill, stroke, ink) = match kind {
        Action::Primary => (ACCENT, Stroke::NONE, INK),
        Action::Quiet => (rgb(0x1a, 0x1c, 0x22), Stroke::new(1.0_f32, rgb(0x2e, 0x32, 0x3c)), TEXT),
        Action::Record => (REC, Stroke::NONE, INK),
        Action::Stop => (Color32::TRANSPARENT, Stroke::new(1.0_f32, REC), REC_TEXT),
    };
    let fill = if hovered { lift(fill) } else { fill };
    let painter = ui.painter();
    painter.rect(rect, 8.0, fill, stroke, StrokeKind::Inside);
    let galley = painter.layout_no_wrap(text.to_owned(), semibold(14.0), ink);
    let glyph = matches!(kind, Action::Record | Action::Stop);
    let glyph_w = if glyph { 18.0 } else { 0.0 };
    let left = rect.center().x - (galley.size().x + glyph_w) / 2.0;
    if glyph {
        let center = egui::pos2(left + 5.0, rect.center().y);
        match kind {
            Action::Record => {
                painter.circle_filled(center, 5.0, INK);
            }
            _ => {
                painter.rect_filled(Rect::from_center_size(center, egui::vec2(10.0, 10.0)), 2.0, REC_TEXT);
            }
        }
    }
    let pos = egui::pos2(left + glyph_w, rect.center().y - galley.size().y / 2.0);
    painter.galley(pos, galley, ink);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn lift(color: Color32) -> Color32 {
    if color.a() == 0 {
        return Color32::from_rgba_unmultiplied(255, 90, 95, 22);
    }
    let up = |c: u8| c.saturating_add(20);
    rgb(up(color.r()), up(color.g()), up(color.b()))
}

/// Tono de un aviso: fondo, borde y color del título.
#[derive(Clone, Copy)]
pub enum Tone {
    Live,
    Info,
    Warn,
    Danger,
    Neutral,
}

impl Tone {
    fn colors(self) -> (Color32, Color32, Color32) {
        match self {
            Tone::Live => (rgb(0x0f, 0x1d, 0x17), rgb(0x1f, 0x3a, 0x2e), LIVE_TEXT),
            Tone::Info => (rgb(0x0f, 0x16, 0x26), rgb(0x1f, 0x2c, 0x47), rgb(0xa9, 0xc8, 0xff)),
            Tone::Warn => (rgb(0x1e, 0x18, 0x0d), rgb(0x3d, 0x31, 0x19), WARN_TEXT),
            Tone::Danger => (rgb(0x20, 0x12, 0x1a), rgb(0x3e, 0x1f, 0x27), REC_TEXT),
            Tone::Neutral => (CARD, BORDER, TEXT),
        }
    }
}

/// Tarjeta de aviso: título fuerte y una línea de explicación.
pub fn notice_card(ui: &mut Ui, tone: Tone, title: &str, body: &str, mono_body: bool) {
    let (fill, border, title_color) = tone.colors();
    egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0_f32, border))
        .corner_radius(8)
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.label(RichText::new(title).font(semibold(13.0)).color(title_color));
            if !body.is_empty() {
                let font = if mono_body { FontId::monospace(11.0) } else { FontId::proportional(12.0) };
                ui.add(egui::Label::new(RichText::new(body).font(font).color(SOFT)).wrap());
            }
        });
}

/// Marca sobre la vista previa («EN VIVO», «REC 00:08», «Último cuadro»).
pub fn badge(ui: &Ui, anchor: Pos2, align: Align2, text: &str, dot: Color32, color: Color32) {
    let painter = ui.painter();
    let galley = painter.layout_no_wrap(text.to_owned(), semibold(11.0), color);
    let pad = egui::vec2(9.0, 4.0);
    let dot_w = 13.0;
    let size = egui::vec2(galley.size().x + dot_w, galley.size().y) + pad * 2.0;
    let rect = align.anchor_size(anchor, size);
    painter.rect_filled(rect, 6.0, Color32::from_rgba_unmultiplied(7, 8, 10, 190));
    painter.circle_filled(egui::pos2(rect.left() + pad.x + 3.5, rect.center().y), 3.5, dot);
    painter.galley(egui::pos2(rect.left() + pad.x + dot_w, rect.top() + pad.y), galley, color);
}

/// Píldora de la barra de título: punto de estado y texto.
pub fn chip(ui: &mut Ui, dot: Color32, text: &str) {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), FontId::proportional(12.0), SOFT);
    let size = egui::vec2(galley.size().x + 10.0 + 7.0 + 7.0 + 10.0, 24.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    painter.rect_stroke(rect, 12.0, Stroke::new(1.0_f32, FIELD_BORDER), StrokeKind::Inside);
    painter.circle_filled(egui::pos2(rect.left() + 13.5, rect.center().y), 3.5, dot);
    painter.galley(egui::pos2(rect.left() + 24.0, rect.center().y - galley.size().y / 2.0), galley, SOFT);
}

/// Texto centrado, en varias líneas si hace falta, con el centro de arriba en `top`.
pub fn centered_text(ui: &Ui, top: Pos2, text: String, width: f32, color: Color32) {
    let mut job = egui::text::LayoutJob::simple(text, FontId::proportional(14.0), color, width);
    job.halign = Align::Center;
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(top, galley, color);
}

/// Punto de estado de 7 px dentro de una fila.
pub fn status_dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(7.0, 7.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.5, color);
}

/// Ícono de la barra de título: una cámara de trazo fino en el acento.
pub fn logo(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), Sense::hover());
    let stroke = Stroke::new(1.6_f32, ACCENT);
    let body = Rect::from_min_size(rect.min + egui::vec2(2.0, 5.5), egui::vec2(12.5, 11.0));
    let painter = ui.painter();
    painter.rect_stroke(body, 3.0, stroke, StrokeKind::Middle);
    painter.circle_stroke(body.center(), 2.2, stroke);
    let x = body.right();
    painter.add(egui::Shape::closed_line(
        vec![
            egui::pos2(x, body.top() + 4.0),
            egui::pos2(rect.right() - 1.5, body.top() + 1.0),
            egui::pos2(rect.right() - 1.5, body.bottom() - 1.0),
            egui::pos2(x, body.bottom() - 4.0),
        ],
        stroke,
    ));
}

/// Teléfono de trazo fino para la vista previa vacía.
pub fn phone_glyph(ui: &Ui, center: Pos2) {
    let stroke = Stroke::new(1.5_f32, rgb(0x4a, 0x50, 0x5c));
    let body = Rect::from_center_size(center, egui::vec2(18.0, 32.0));
    ui.painter().rect_stroke(body, 4.0, stroke, StrokeKind::Middle);
    ui.painter().line_segment(
        [egui::pos2(center.x - 2.0, body.bottom() - 5.0), egui::pos2(center.x + 2.0, body.bottom() - 5.0)],
        stroke,
    );
}
