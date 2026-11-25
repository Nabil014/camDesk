//! Controles del diseño. Los pintados a mano declaran nombre y estado para el lector de pantalla.

use eframe::egui::{self, pos2, vec2, Color32, CornerRadius, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, WidgetInfo, WidgetType};

use crate::theme::{self, body, mono, semibold};

pub fn section_title(ui: &mut Ui, title: &str) {
    ui.label(egui::RichText::new(title).font(semibold(11.0)).color(theme::MUTED));
}

pub fn field_label(ui: &mut Ui, text: &str) -> Response {
    ui.label(egui::RichText::new(text).font(body(12.0)).color(theme::TEXT_SOFT))
}

/// Grupo de opciones excluyentes, todas a la vista.
pub fn segmented<T: Copy + PartialEq>(ui: &mut Ui, value: &mut T, options: &[(&str, T)]) -> bool {
    let mut changed = false;
    egui::Frame::new()
        .fill(theme::STAGE)
        .stroke(Stroke::new(1.0_f32, theme::SEGMENT_BORDER))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::same(2))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.columns(options.len(), |cols| {
                for (col, (label, option)) in cols.iter_mut().zip(options) {
                    let selected = *value == *option;
                    let color = if selected { theme::TEXT_STRONG } else { theme::MUTED };
                    let button = egui::Button::new(egui::RichText::new(*label).font(body(12.0)).color(color))
                        .fill(if selected { theme::SELECTED } else { Color32::TRANSPARENT })
                        .stroke(Stroke::NONE)
                        .corner_radius(6.0)
                        .wrap_mode(egui::TextWrapMode::Extend)
                        .min_size(vec2(col.available_width(), 30.0));
                    // Justificado: el botón llena la columna y el texto queda al centro.
                    let response = col.vertical_centered_justified(|ui| ui.add(button)).inner;
                    let enabled = col.is_enabled();
                    response.widget_info(|| WidgetInfo::selected(WidgetType::Button, enabled, selected, *label));
                    if response.clicked() && !selected {
                        *value = *option;
                        changed = true;
                    }
                }
            });
        });
    changed
}

/// Ícono entre la casilla y el texto.
#[derive(Clone, Copy)]
pub enum Icon {
    None,
    Speaker,
    Mic,
}

/// Casilla con texto y, a la derecha, un dato corto (la GPU o el nivel en dB).
pub fn checkbox(ui: &mut Ui, on: &mut bool, label: &str, icon: Icon, trailing: &str) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let (enabled, selected) = (ui.is_enabled(), *on);
    response.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, enabled, selected, label));
    let painter = ui.painter();
    let square = Rect::from_min_size(pos2(rect.left(), rect.center().y - 8.0), vec2(16.0, 16.0));
    if *on {
        painter.rect_filled(square, 3.0, theme::ACCENT);
        painter.add(Shape::line(
            vec![square.min + vec2(3.5, 8.5), square.min + vec2(6.5, 11.5), square.min + vec2(12.5, 4.5)],
            Stroke::new(2.0_f32, theme::INK),
        ));
    } else {
        painter.rect(square, 3.0, theme::STAGE, Stroke::new(1.0_f32, theme::OFF), StrokeKind::Inside);
    }
    if response.has_focus() {
        painter.rect_stroke(square.expand(2.0), 4.0, Stroke::new(2.0_f32, theme::ACCENT), StrokeKind::Outside);
    }
    let mut x = square.right() + 8.0;
    let icon_at = Rect::from_min_size(pos2(x, rect.center().y - 7.5), vec2(15.0, 15.0));
    match icon {
        Icon::None => {}
        Icon::Speaker => paint_speaker(painter, icon_at),
        Icon::Mic => paint_mic(painter, icon_at),
    }
    if !matches!(icon, Icon::None) {
        x = icon_at.right() + 8.0;
    }
    painter.text(pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, label, body(13.0), theme::TEXT);
    if !trailing.is_empty() {
        painter.text(rect.right_center(), egui::Align2::RIGHT_CENTER, trailing, mono(11.0), theme::MUTED);
    }
    response
}

/// Rótulo a la izquierda, valor a la derecha y la barra debajo.
pub fn slider(ui: &mut Ui, label: &str, value: &mut f32, range: std::ops::RangeInclusive<f32>, shown: &str) -> bool {
    ui.scope(|ui| slider_rows(ui, label, value, range, shown)).inner
}

fn slider_rows(ui: &mut Ui, label: &str, value: &mut f32, range: std::ops::RangeInclusive<f32>, shown: &str) -> bool {
    // Filas bajas: el alto de interacción general es el de los campos, de 34 px.
    ui.spacing_mut().interact_size.y = 16.0;
    ui.spacing_mut().item_spacing.y = 2.0;
    let name = ui
        .horizontal(|ui| {
            let name = field_label(ui, label);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(shown).font(mono(12.0)).color(theme::TEXT_SOFT));
            });
            name
        })
        .inner;
    ui.scope(|ui| {
        ui.spacing_mut().slider_width = ui.available_width();
        ui.spacing_mut().slider_rail_height = 4.0;
        let widgets = &mut ui.visuals_mut().widgets;
        for state in [&mut widgets.inactive, &mut widgets.hovered, &mut widgets.active] {
            state.bg_fill = theme::RAIL;
            state.fg_stroke = Stroke::new(4.0_f32, theme::ACCENT);
            state.bg_stroke = Stroke::NONE;
        }
        ui.visuals_mut().selection.bg_fill = theme::ACCENT;
        ui.add(egui::Slider::new(value, range).show_value(false).integer().trailing_fill(true))
            .labelled_by(name.id)
            .changed()
    })
    .inner
}

pub fn level_meter(ui: &mut Ui, level: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 6.0), Sense::hover());
    ui.painter().rect_filled(rect, 3.0, theme::RAIL);
    let fill = rect.width() * level.clamp(0.0, 1.0);
    if fill > 0.5 {
        ui.painter().rect_filled(Rect::from_min_size(rect.min, vec2(fill, rect.height())), 3.0, theme::LIVE);
    }
}

#[derive(Clone, Copy)]
pub enum Action {
    /// La acción principal: Conectar.
    Primary,
    /// Desconectar o Cancelar.
    Neutral,
    Record,
    Stop,
}

pub fn big_button(ui: &mut Ui, text: &str, action: Action) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), theme::BUTTON_HEIGHT), Sense::click());
    let enabled = ui.is_enabled();
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, text));
    let (mut fill, color, border) = match action {
        Action::Primary => (theme::ACCENT, theme::INK, Stroke::NONE),
        Action::Neutral => (theme::NEUTRAL, theme::TEXT, Stroke::new(1.0_f32, theme::NEUTRAL_BORDER)),
        Action::Record => (theme::REC, theme::INK, Stroke::NONE),
        Action::Stop => (Color32::TRANSPARENT, theme::REC_TEXT, Stroke::new(1.0_f32, theme::REC)),
    };
    if response.hovered() {
        fill = match action {
            Action::Stop => theme::REC.gamma_multiply(0.12),
            Action::Neutral => theme::SELECTED,
            _ => fill.gamma_multiply(0.9),
        };
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let painter = ui.painter();
    painter.rect(rect, CornerRadius::same(8), fill, border, StrokeKind::Inside);
    if response.has_focus() {
        painter.rect_stroke(rect.expand(2.0), 10.0, Stroke::new(2.0_f32, theme::ACCENT), StrokeKind::Outside);
    }
    let galley = painter.layout_no_wrap(text.to_owned(), semibold(14.0), color);
    let mark = match action {
        Action::Record | Action::Stop => 18.0,
        Action::Primary | Action::Neutral => 0.0,
    };
    let left = rect.center().x - (galley.size().x + mark) * 0.5;
    let dot = pos2(left + 5.0, rect.center().y);
    match action {
        Action::Record => {
            painter.circle_filled(dot, 5.0, color);
        }
        Action::Stop => {
            painter.rect_filled(Rect::from_center_size(dot, vec2(10.0, 10.0)), 2.0, color);
        }
        Action::Primary | Action::Neutral => {}
    }
    painter.galley(pos2(left + mark, rect.center().y - galley.size().y * 0.5), galley, color);
    response
}

/// Botón chico y sin relleno, como «Restablecer».
pub fn ghost_button(ui: &mut Ui, text: &str) -> Response {
    ui.scope(|ui| {
        ui.spacing_mut().interact_size.y = 24.0;
        ui.add(
            egui::Button::new(egui::RichText::new(text).font(body(12.0)).color(theme::TEXT_SOFT))
                .fill(Color32::TRANSPARENT)
                .stroke(Stroke::new(1.0_f32, theme::GHOST_BORDER))
                .corner_radius(6.0),
        )
    })
    .inner
}

pub fn dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.5, color);
}

/// Etiqueta sobre la vista previa. `anchor` es la esquina que indica `align`.
pub fn badge(painter: &egui::Painter, anchor: egui::Pos2, align: egui::Align2, mark: Color32, text: &str, color: Color32) {
    let galley = painter.layout_no_wrap(text.to_owned(), mono(11.0), color);
    let pad = vec2(9.0, 4.0);
    let size = galley.size() + pad * 2.0 + vec2(13.0, 0.0);
    let rect = align.anchor_size(anchor, size);
    painter.rect_filled(rect, 6.0, Color32::from_rgba_unmultiplied(7, 8, 10, 184));
    painter.circle_filled(pos2(rect.left() + pad.x + 3.5, rect.center().y), 3.5, mark);
    painter.galley(rect.min + pad + vec2(13.0, 0.0), galley, color);
}

/// Píldora con punto y texto, para la barra de arriba.
pub fn pill(ui: &mut Ui, color: Color32, text: &str) {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), body(12.0), theme::TEXT_SOFT);
    let (rect, response) = ui.allocate_exact_size(vec2(galley.size().x + 35.0, 26.0), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text));
    let painter = ui.painter();
    painter.rect_stroke(rect, 13.0, Stroke::new(1.0_f32, theme::FIELD_BORDER), StrokeKind::Inside);
    painter.circle_filled(pos2(rect.left() + 13.5, rect.center().y), 3.5, color);
    painter.galley(pos2(rect.left() + 25.0, rect.center().y - galley.size().y * 0.5), galley, theme::TEXT_SOFT);
}

/// La marca de Camdesk: una cámara de línea.
pub fn logo(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
    let at = |x: f32, y: f32| rect.min + vec2(x, y) * (22.0 / 24.0);
    let line = Stroke::new(1.7_f32, theme::ACCENT);
    let painter = ui.painter();
    painter.rect_stroke(Rect::from_min_max(at(2.5, 6.0), at(15.5, 18.0)), 3.0, line, StrokeKind::Middle);
    painter.add(Shape::line(vec![at(15.5, 10.5), at(21.0, 7.5), at(21.0, 16.5), at(15.5, 13.5)], line));
    painter.circle_stroke(at(9.0, 12.0), 2.0, line);
}

/// Teléfono de línea para la vista previa vacía.
pub fn paint_phone(painter: &egui::Painter, center: egui::Pos2) {
    let line = Stroke::new(1.5_f32, theme::OFF);
    let body = Rect::from_center_size(center, vec2(20.0, 36.0));
    painter.rect_stroke(body, 5.0, line, StrokeKind::Middle);
    painter.line_segment([center + vec2(-2.5, 13.0), center + vec2(2.5, 13.0)], line);
}

fn paint_speaker(painter: &egui::Painter, rect: Rect) {
    let at = |x: f32, y: f32| rect.min + vec2(x, y) * (rect.width() / 24.0);
    let line = Stroke::new(1.3_f32, theme::TEXT_SOFT);
    painter.add(Shape::closed_line(vec![at(11.0, 5.0), at(6.0, 9.0), at(3.0, 9.0), at(3.0, 15.0), at(6.0, 15.0), at(11.0, 19.0)], line));
    painter.add(Shape::line(vec![at(15.5, 8.5), at(17.5, 12.0), at(15.5, 15.5)], line));
    painter.add(Shape::line(vec![at(19.0, 5.5), at(22.0, 12.0), at(19.0, 18.5)], line));
}

fn paint_mic(painter: &egui::Painter, rect: Rect) {
    let at = |x: f32, y: f32| rect.min + vec2(x, y) * (rect.width() / 24.0);
    let line = Stroke::new(1.3_f32, theme::TEXT_SOFT);
    painter.rect_stroke(Rect::from_min_max(at(9.0, 3.0), at(15.0, 14.0)), 2.0, line, StrokeKind::Middle);
    painter.add(Shape::line(vec![at(5.5, 11.0), at(7.5, 15.5), at(12.0, 17.5), at(16.5, 15.5), at(18.5, 11.0)], line));
    painter.line_segment([at(12.0, 17.5), at(12.0, 21.0)], line);
}
