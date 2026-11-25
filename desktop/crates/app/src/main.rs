//! Ventana de Camdesk. Al abrir no carga FFmpeg, no carga el runtime de NVIDIA,
//! no registra la cámara virtual, no crea el archivo mapeado, no corre adb,
//! no abre WASAPI y no captura la pantalla.
//!
//! El video no corre en este hilo: lo lleva `video::Video`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod theme;
mod video;

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::time::Duration;

use eframe::egui;
use fx::{BlurAvailability, ColorParams, blur_availability};
use link::{Camera, Mode, Resolution, SessionConfig, Status, Transport};
use theme::{Action, Tone};
use video::{Command, Report, Video};
use record::{MicId, MonitorId, RecordConfig, Recording, default_output, microphones, monitors};

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([960.0, 680.0])
            .with_icon(app_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "Camdesk",
        options,
        Box::new(|cc| {
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(CamdeskApp::load()))
        }),
    )
}

/// Icono de la ventana y de la barra de tareas. Sin él, eframe pone el logo de egui.
fn app_icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")).unwrap_or_default()
}

struct CamdeskApp {
    mode: LinkMode,
    ip: String,
    resolution: Resolution,
    camera: Camera,
    color: ColorParams,
    blur_on: bool,
    blur_strength: f32,
    blur: Option<BlurAvailability>,
    blur_note: String,
    /// Aviso de modo rendimiento mientras el desenfoque siga prendido.
    blur_perf: Option<String>,
    system_audio: bool,
    mic_on: bool,
    monitors: Vec<record::MonitorInfo>,
    mics: Vec<record::MicInfo>,
    monitor: Option<MonitorId>,
    monitor_size: Option<(u32, u32)>,
    mic: Option<MicId>,
    status: String,
    live_transport: Option<Transport>,
    notice: String,
    /// Hay una sesión con el teléfono abierta en el hilo de video.
    connected: bool,
    /// El hilo de video ya tiene un decoder y un escritor del archivo mapeado.
    has_decoder: bool,
    has_writer: bool,
    video: Option<Video>,
    vcam: Option<vcam::VirtualCamera>,
    preview: Option<egui::TextureHandle>,
    recording: Option<Recording>,
    dirty: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LinkMode {
    Auto,
    Usb,
    Wifi,
}

impl CamdeskApp {
    fn load() -> Self {
        let mut app = Self {
            mode: LinkMode::Auto,
            ip: String::new(),
            resolution: Resolution::P720,
            camera: Camera::Back,
            color: ColorParams::default(),
            blur_on: false,
            blur_strength: 0.5,
            blur: None,
            blur_note: String::new(),
            blur_perf: None,
            system_audio: true,
            mic_on: true,
            monitors: Vec::new(),
            mics: Vec::new(),
            monitor: None,
            monitor_size: None,
            mic: None,
            status: "Desconectado".into(),
            live_transport: None,
            notice: String::new(),
            connected: false,
            has_decoder: false,
            has_writer: false,
            video: None,
            vcam: None,
            preview: None,
            recording: None,
            dirty: false,
        };
        app.read_config();
        app
    }

    /// Barra de título: nombre a la izquierda y estado de cellCam a la derecha.
    fn title_bar(&self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("titulo")
            .exact_height(44.0)
            .frame(egui::Frame::new().fill(theme::TITLE).inner_margin(egui::Margin::symmetric(16, 0)))
            .show_separator_line(true)
            .show(ctx, |ui| {
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    theme::logo(ui);
                    ui.label(egui::RichText::new("Camdesk").font(theme::semibold(14.0)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (dot, text) = if self.status.starts_with("En vivo") {
                            (theme::LIVE, "cellCam en vivo")
                        } else if self.vcam.is_some() {
                            (theme::IDLE, "cellCam en espera")
                        } else {
                            (theme::IDLE, "cellCam se registra al conectar")
                        };
                        theme::chip(ui, dot, text);
                    });
                });
            });
    }

    /// Paneles de abajo, como los docks de OBS. El alto se arrastra desde el borde de arriba.
    fn docks(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("paneles")
            .resizable(true)
            .default_height(320.0)
            .height_range(220.0..=460.0)
            .frame(egui::Frame::new().fill(theme::BORDER))
            .show_separator_line(true)
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(1.0, 0.0);
                ui.columns(4, |cols| {
                    theme::dock(&mut cols[0], "conexion", |ui| self.connection(ui));
                    theme::dock(&mut cols[1], "efectos", |ui| self.effects(ui));
                    theme::dock(&mut cols[2], "grabacion", |ui| self.record_ui(ui));
                    theme::dock(&mut cols[3], "controles", |ui| self.controls(ui));
                });
            });
    }

    fn connection(&mut self, ui: &mut egui::Ui) {
        theme::dock_header(ui, "Conexión", |_| ());
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            theme::field_label(ui, "Modo");
            self.dirty |= theme::segments(
                ui,
                &mut self.mode,
                &[
                    ("Automática", LinkMode::Auto),
                    ("Solo USB", LinkMode::Usb),
                    ("Solo Wi-Fi", LinkMode::Wifi),
                ],
            );
        });
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            let label = theme::field_label(ui, "IP del teléfono");
            let hint = if self.mode == LinkMode::Wifi { "192.168.1.37" } else { "Si hace falta" };
            let ip = ui
                .add_enabled(
                    self.mode != LinkMode::Usb,
                    egui::TextEdit::singleline(&mut self.ip)
                        .hint_text(egui::RichText::new(hint).color(theme::MUTED))
                        .font(egui::FontId::monospace(12.0))
                        .margin(egui::Margin::symmetric(10, 8))
                        .desired_width(ui.available_width()),
                )
                .labelled_by(label.id);
            self.dirty |= ip.changed();
        });
        ui.columns(2, |cols| {
            cols[0].spacing_mut().item_spacing.y = 6.0;
            cols[1].spacing_mut().item_spacing.y = 6.0;
            theme::field_label(&mut cols[0], "Resolución");
            self.dirty |=
                theme::segments(&mut cols[0], &mut self.resolution, &[("720p", Resolution::P720), ("1080p", Resolution::P1080)]);
            theme::field_label(&mut cols[1], "Cámara");
            self.dirty |= theme::segments(&mut cols[1], &mut self.camera, &[("Trasera", Camera::Back), ("Frontal", Camera::Front)]);
        });
    }

    /// Conectar y Grabar, y el aviso del momento.
    fn controls(&mut self, ui: &mut egui::Ui) {
        theme::dock_header(ui, "Controles", |_| ());
        let (label, kind) = if !self.connected {
            ("Conectar", Action::Primary)
        } else if self.shown_status() == "Conectando" || self.status == "Teléfono no autorizado" {
            ("Cancelar", Action::Quiet)
        } else {
            ("Desconectar", Action::Quiet)
        };
        if theme::action_button(ui, label, kind).clicked() {
            if !self.connected {
                self.connect(ui.ctx());
            } else {
                self.disconnect();
            }
        }
        let recording = self.recording.is_some();
        let (label, kind) = if recording { ("Detener", Action::Stop) } else { ("Grabar", Action::Record) };
        if theme::action_button(ui, label, kind).clicked() {
            if recording {
                self.stop_recording();
            } else {
                self.start_recording();
            }
        }
        if !self.notice.is_empty() {
            let (lead, body) = self.notice.split_once('\n').unwrap_or((self.notice.as_str(), ""));
            theme::notice_card(ui, notice_tone(lead), lead, body, lead == "Grabación guardada");
        } else if !self.connected && self.preview.is_none() && !recording {
            theme::notice_card(
                ui,
                Tone::Neutral,
                "Listo para conectar",
                "Conectar el teléfono por USB o Wi-Fi. Nada se carga hasta entonces.",
                false,
            );
        }
    }

    fn retrying(&self) -> bool {
        self.connected
            && self.preview.is_none()
            && !self.status.starts_with("En vivo")
            && self.status != "Ocupado"
            && self.status != "Versión incompatible"
            && self.status != "Teléfono no autorizado"
    }

    fn shown_status(&self) -> &str {
        if self.retrying() { "Conectando" } else { self.status.as_str() }
    }

    fn disconnect(&mut self) {
        // El hilo de video suelta la sesión: adb y el join no traban la ventana.
        if let Some(video) = &self.video {
            video.send(Command::Stop);
        }
        self.connected = false;
        self.status = "Desconectado".into();
        if self.preview.is_some() && self.recording.is_none() {
            self.notice = "Último cuadro\nSi se corta la red, la cámara virtual no pasa a negro.".into();
        } else if self.notice.starts_with("Teléfono no autorizado") || self.notice.starts_with("Conectando") {
            self.notice.clear();
        }
    }

    fn effects(&mut self, ui: &mut egui::Ui) {
        if self.blur.is_none() {
            self.blur = Some(blur_availability());
            self.blur_note = match self.blur {
                Some(BlurAvailability::Available) => String::new(),
                Some(BlurAvailability::NoNvidiaGpu) => "Sin GPU NVIDIA. No hay efecto de respaldo.".into(),
                Some(BlurAvailability::NoRuntime) | None => "Sin el runtime de NVIDIA Video Effects. No hay efecto de respaldo.".into(),
            };
        }
        let adjusted = color_adjusted(&self.color);
        let reset = theme::dock_header(ui, "Efectos", |ui| {
            let button = egui::Button::new(egui::RichText::new("Restablecer").size(12.0).color(theme::SOFT))
                .fill(egui::Color32::TRANSPARENT)
                .stroke(egui::Stroke::new(1.0_f32, theme::FIELD_BORDER))
                .corner_radius(6);
            ui.add_enabled(adjusted, button).clicked()
        });
        if reset {
            self.color = ColorParams::default();
            self.dirty = true;
            self.send_color();
        }
        let available = matches!(self.blur, Some(BlurAvailability::Available));
        egui::Frame::new()
            .fill(theme::CARD)
            .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
            .corner_radius(8)
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| self.blur_card(ui, available));
        if !self.blur_note.is_empty() {
            ui.add(egui::Label::new(egui::RichText::new(&self.blur_note).size(12.0).color(theme::MUTED)).wrap());
        }
        if !available && self.notice.is_empty() {
            self.notice = "Sin desenfoque\nSin GPU NVIDIA, o sin el runtime de NVIDIA Video Effects. La cámara sigue.".into();
        }
        if !available {
            ui.hyperlink_to(
                egui::RichText::new("Descargar el runtime").size(12.0),
                "https://www.nvidia.com/en-us/geforce/broadcasting/broadcast-sdk/resources/",
            );
        }
        let signed = |value: f32| {
            let percent = (value * 100.0).round() as i32;
            if percent > 0 { format!("+{percent}") } else { percent.to_string() }
        };
        let mut changed = false;
        changed |= theme::slider_row(ui, "Brillo", &mut self.color.brightness, -1.0..=1.0, signed);
        changed |= theme::slider_row(ui, "Contraste", &mut self.color.contrast, -1.0..=1.0, signed);
        changed |= theme::slider_row(ui, "Saturación", &mut self.color.saturation, -1.0..=1.0, signed);
        changed |= theme::slider_row(ui, "Temperatura", &mut self.color.temperature, -1.0..=1.0, signed);
        if changed {
            self.dirty = true;
            self.send_color();
        }
    }

    fn blur_card(&mut self, ui: &mut egui::Ui, available: bool) {
        ui.set_width(ui.available_width());
        ui.add_enabled_ui(available, |ui| {
            if theme::toggle(ui, &mut self.blur_on, "Desenfocar fondo").changed() {
                self.dirty = true;
                // El runtime de NVIDIA se carga en el hilo de video, que es el que usa el efecto.
                // Si no carga, llega BlurFailed y la casilla vuelve a apagarse.
                let ctx = ui.ctx().clone();
                let command = Command::Blur { on: self.blur_on, strength: self.blur_strength };
                match self.ensure_video(&ctx) {
                    Some(video) => video.send(command),
                    None => self.blur_on = false,
                }
                if !self.blur_on {
                    self.blur_perf = None;
                    if self.notice.starts_with("Modo rendimiento") {
                        self.notice.clear();
                    }
                }
            }
            let mut percent = (self.blur_strength * 100.0).round();
            let blur_on = self.blur_on;
            let moved = ui
                .add_enabled_ui(blur_on, |ui| {
                    theme::slider_row(ui, "Fuerza", &mut percent, 0.0..=100.0, |value| format!("{} %", value.round()))
                })
                .inner;
            if moved {
                self.blur_strength = percent.round() / 100.0;
                self.dirty = true;
                if let Some(video) = &self.video {
                    video.send(Command::Blur { on: true, strength: self.blur_strength });
                }
            }
        });
    }

    fn record_ui(&mut self, ui: &mut egui::Ui) {
        theme::dock_header(ui, "Grabación", |_| ());
        self.ensure_monitors();
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            theme::field_label(ui, "Monitor");
            let label = self.monitor_label();
            egui::ComboBox::from_id_salt("monitor")
                .width(ui.available_width())
                .selected_text(label)
                .show_ui(ui, |ui| {
                    self.ensure_monitors();
                    for item in &self.monitors {
                        let selected = self.monitor.as_ref() == Some(&item.id);
                        if ui.selectable_label(selected, monitor_text(item)).clicked() {
                            self.monitor = Some(item.id.clone());
                            self.monitor_size = Some((item.width, item.height));
                            self.dirty = true;
                        }
                    }
                });
        });
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            self.dirty |= theme::toggle(ui, &mut self.system_audio, "Audio del sistema").changed();
            let level = if self.system_audio { self.meter_level(true) } else { 0.0 };
            theme::level_meter(ui, level);
        });
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            self.dirty |= theme::toggle(ui, &mut self.mic_on, "Micrófono").changed();
            let level = if self.mic_on { self.meter_level(false) } else { 0.0 };
            theme::level_meter(ui, level);
        });
        if self.mic_on {
            let mic_label = self
                .mics
                .iter()
                .find(|item| Some(&item.id) == self.mic.as_ref())
                .map(|item| item.name.clone())
                .unwrap_or_else(|| "Elegir micrófono".into());
            egui::ComboBox::from_id_salt("microfono")
                .width(ui.available_width())
                .selected_text(mic_label)
                .show_ui(ui, |ui| {
                    if self.mics.is_empty() {
                        self.mics = microphones();
                        if self.mic.is_none() {
                            self.mic = self.mics.first().map(|item| item.id.clone());
                        }
                    }
                    for item in &self.mics {
                        let selected = self.mic.as_ref() == Some(&item.id);
                        if ui.selectable_label(selected, &item.name).clicked() {
                            self.mic = Some(item.id.clone());
                            self.dirty = true;
                        }
                    }
                });
        }
        ui.add(
            egui::Label::new(
                egui::RichText::new("El micrófono no sale por los parlantes. Los efectos no tocan la grabación.")
                    .size(11.0)
                    .color(theme::MUTED),
            )
            .wrap(),
        );
        if let Some(rec) = &self.recording {
            if let Some(error) = rec.error() {
                self.notice = error;
            }
        }
    }

    /// Crea el hilo de video la primera vez que hace falta: al conectar o al prender el desenfoque.
    fn ensure_video(&mut self, ctx: &egui::Context) -> Option<&Video> {
        if self.video.is_none() {
            match Video::spawn(ctx.clone()) {
                Ok(video) => {
                    video.send(Command::Color(self.color));
                    self.video = Some(video);
                }
                Err(error) => {
                    self.notice = format!("No se pudo crear el hilo de video\n{error}");
                    app_log(&self.notice);
                    return None;
                }
            }
        }
        self.video.as_ref()
    }

    fn send_color(&self) {
        if let Some(video) = &self.video {
            video.send(Command::Color(self.color));
        }
    }

    fn connect(&mut self, ctx: &egui::Context) {
        let wifi = parse_ip(&self.ip);
        let mode = match self.mode {
            LinkMode::Auto => Mode::Auto,
            LinkMode::Usb => Mode::Usb,
            LinkMode::Wifi => match wifi {
                Some(ip) => Mode::Wifi(ip),
                None => {
                    self.notice = "Falta una IP válida".into();
                    return;
                }
            },
        };
        if self.mode == LinkMode::Auto && !self.ip.is_empty() && wifi.is_none() {
            self.notice = "La IP no es válida".into();
            return;
        }
        if self.ensure_video(ctx).is_none() {
            return;
        }
        let warning = self.ensure_pipeline();
        let Some(video) = &self.video else { return };
        video.send(Command::Start(SessionConfig {
            mode,
            resolution: self.resolution,
            camera: self.camera,
            wifi,
        }));
        self.connected = true;
        self.status = "Conectando".into();
        self.notice = warning.unwrap_or_else(|| {
            "Conectando\nUSB si el teléfono está autorizado. Si no, la IP.".into()
        });
    }

    /// Decoder y archivo mapeado se crean al primer Conectar y pasan al hilo de video, que los guarda
    /// hasta cerrar la ventana. El archivo no se recrea en la ejecución.
    fn ensure_pipeline(&mut self) -> Option<String> {
        point_at_ffmpeg();
        let mut warning = None;
        let mut decoder = None;
        if !self.has_decoder {
            match decode::Decoder::new() {
                Ok(created) => decoder = Some(created),
                Err(error) => note(&mut warning, error.to_string()),
            }
        }
        let mut writer = None;
        if !self.has_writer {
            let path = vcam_shm::default_path();
            match vcam_shm::Writer::create(&path) {
                Ok(created) => writer = Some(created),
                Err(error) if error.raw_os_error() == Some(1224) => match vcam_shm::Writer::open_existing(&path) {
                    Ok(created) => writer = Some(created),
                    Err(error) => note(&mut warning, format!("archivo de la cámara: {error}")),
                },
                Err(error) => note(&mut warning, format!("archivo de la cámara: {error}")),
            }
        }
        if decoder.is_some() || writer.is_some() {
            self.has_decoder |= decoder.is_some();
            self.has_writer |= writer.is_some();
            if let Some(video) = &self.video {
                video.send(Command::Pipeline { decoder, writer });
            }
        }
        point_at_vcam_dll();
        if self.vcam.is_none() {
            match vcam::VirtualCamera::create() {
                Ok(camera) => self.vcam = Some(camera),
                Err(error) => note(&mut warning, error.message),
            }
        }
        if let Some(text) = &warning {
            app_log(text);
        }
        warning
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let mut batch = Vec::new();
        if let Some(video) = self.video.as_ref() {
            while let Some(report) = video.try_report() {
                batch.push(report);
            }
        }
        for report in batch {
            match report {
                Report::Status(status) => self.on_status(status),
                Report::StartFailed(text) => {
                    self.connected = false;
                    self.status = "Desconectado".into();
                    self.notice = text;
                }
                Report::BlurReady => {
                    self.blur_note.clear();
                    if self.notice.starts_with("Sin desenfoque") {
                        self.notice.clear();
                    }
                }
                Report::BlurFailed(text) => {
                    self.blur_on = false;
                    self.blur_perf = None;
                    self.blur_note = text;
                }
                Report::BlurStopped(text) => {
                    self.blur_on = false;
                    self.blur_perf = None;
                    self.blur_note = text;
                    self.notice = "Sin desenfoque\nEl efecto falló y se apagó. La cámara sigue.".into();
                }
                Report::BlurNotice(text) => {
                    if self.blur_on {
                        self.blur_note = text.clone();
                        self.blur_perf = Some(text);
                    }
                }
                Report::DecodeError(text) => {
                    app_log(&text);
                    self.notice = text;
                }
            }
        }
        let image = self.video.as_ref().and_then(Video::take_preview);
        if let Some(image) = image {
            self.upload_preview(ctx, image);
        }
    }

    fn on_status(&mut self, status: Status) {
        if let Status::EnVivo { camera, transport, .. } = status {
            self.live_transport = Some(transport);
            if camera != self.camera {
                self.camera = camera;
                self.dirty = true;
                self.notice = match camera {
                    Camera::Back => "El teléfono se quedó en la cámara trasera".into(),
                    Camera::Front => "El teléfono se quedó en la cámara frontal".into(),
                };
            } else if self.notice.starts_with("Conectando")
                || self.notice.starts_with("Teléfono no autorizado")
                || self.notice.is_empty()
            {
                if self.blur_unavailable() {
                    self.notice.clear();
                } else {
                    self.notice = "En vivo
La vista previa y cellCam reciben la imagen. La grabación no.".into();
                }
            }
        } else {
            self.live_transport = None;
        }
        // Con Incompatible y Ocupado link no reintenta y el hilo de video ya soltó la sesión.
        if matches!(status, Status::Incompatible) {
            self.notice = "Versión incompatible
cellCam habla otra versión del protocolo. Actualizar el teléfono o Camdesk.".into();
            self.connected = false;
        }
        if matches!(status, Status::Ocupado) {
            self.notice = "Teléfono ocupado
Otro cliente ya está conectado. Cerrar OBS y volver a conectar.".into();
            self.connected = false;
        }
        if matches!(status, Status::NoAutorizado) {
            self.notice = "Teléfono no autorizado
Aceptar la depuración USB en el teléfono. Camdesk reintenta cada 2 segundos.".into();
        }
        if matches!(status, Status::Desconectado) && self.preview.is_some() && self.recording.is_none() {
            self.notice = "Último cuadro
Si se corta la red, la cámara virtual no pasa a negro.".into();
        }
        let hold_unauthorized = matches!(status, Status::Conectando)
            && self.notice.starts_with("Teléfono no autorizado")
            && self.preview.is_none();
        if !hold_unauthorized {
            self.status = status_text(status);
        }
    }

    fn blur_unavailable(&self) -> bool {
        matches!(self.blur, Some(BlurAvailability::NoNvidiaGpu | BlurAvailability::NoRuntime))
    }

    /// El cuadro ya viene rotado y con efectos del hilo de video, y ya está en la cámara virtual.
    fn upload_preview(&mut self, ctx: &egui::Context, image: egui::ColorImage) {
        let [width, height] = image.size;
        if let Some(texture) = self.preview.as_mut() {
            texture.set(image, egui::TextureOptions::LINEAR);
        } else {
            self.preview = Some(ctx.load_texture("preview", image, egui::TextureOptions::LINEAR));
        }
        self.note_performance();
        self.note_orientation(width as u32, height as u32);
    }

    fn note_performance(&mut self) {
        if !self.blur_on || self.blur_perf.is_none() {
            return;
        }
        let replace = self.notice.is_empty()
            || self.notice.starts_with("En vivo")
            || self.notice.starts_with("Teléfono vertical")
            || self.notice.starts_with("Modo rendimiento");
        if replace {
            self.notice = "Modo rendimiento
El desenfoque pasó a modo rendimiento para sostener 30 fps.".into();
        }
    }

    fn note_orientation(&mut self, width: u32, height: u32) {
        if !self.status.starts_with("En vivo") || self.recording.is_some() {
            return;
        }
        if self.blur_unavailable() {
            return;
        }
        let replace = self.notice.is_empty()
            || self.notice.starts_with("En vivo")
            || self.notice.starts_with("Teléfono vertical");
        if !replace {
            return;
        }
        self.notice = if height > width {
            "Teléfono vertical
La vista previa muestra el teléfono. Las barras las agrega Zoom.".into()
        } else {
            "En vivo
La vista previa y cellCam reciben la imagen. La grabación no.".into()
        };
    }

    /// Escenario oscuro con el cuadro 16:9 de cellCam al centro y el epígrafe debajo.
    fn preview_pane(&self, ui: &mut egui::Ui) {
        let available = ui.available_size();
        let (rect, _) = ui.allocate_exact_size(available, egui::Sense::hover());
        let caption_h = 26.0;
        let room = egui::Rect::from_min_max(rect.min, egui::pos2(rect.right(), rect.bottom() - caption_h));
        let width = room.width().min(room.height() * 16.0 / 9.0).max(1.0);
        let stage = egui::Rect::from_center_size(room.center(), egui::vec2(width, width * 9.0 / 16.0));
        let painter = ui.painter();
        painter.rect_filled(stage, 10.0, theme::FRAME);
        let live = self.status.starts_with("En vivo");
        let frozen = self.status == "Desconectado" && self.preview.is_some();
        if !((live || frozen) && self.paint_frame(ui, stage)) {
            theme::phone_glyph(ui, stage.center() - egui::vec2(0.0, 28.0));
            let top = egui::pos2(stage.center().x, stage.center().y + 2.0);
            theme::centered_text(ui, top, self.empty_copy(), stage.width().min(420.0), theme::MUTED);
        }
        if live {
            theme::badge(ui, stage.left_top() + egui::vec2(12.0, 12.0), egui::Align2::LEFT_TOP, "EN VIVO", theme::LIVE, theme::LIVE_TEXT);
        }
        if let Some(recording) = &self.recording {
            let text = format!("REC {}", format_elapsed(recording.elapsed()));
            theme::badge(ui, stage.right_top() + egui::vec2(-12.0, 12.0), egui::Align2::RIGHT_TOP, &text, theme::REC, theme::REC_TEXT);
        }
        if frozen {
            theme::badge(ui, stage.right_bottom() - egui::vec2(12.0, 12.0), egui::Align2::RIGHT_BOTTOM, "Último cuadro", theme::WARN, theme::WARN_TEXT);
        }
        painter.rect_stroke(stage, 10.0, egui::Stroke::new(1.0_f32, theme::BORDER), egui::StrokeKind::Outside);
        painter.text(
            egui::pos2(rect.center().x, stage.bottom() + caption_h * 0.5 + 2.0),
            egui::Align2::CENTER_CENTER,
            self.caption(),
            egui::FontId::monospace(12.0),
            theme::MUTED,
        );
    }

    /// La imagen entra entera en el cuadro; si el teléfono está vertical quedan barras.
    fn paint_frame(&self, ui: &egui::Ui, stage: egui::Rect) -> bool {
        let Some(texture) = &self.preview else { return false };
        let size = texture.size_vec2();
        if size.x <= 0.0 || size.y <= 0.0 {
            return false;
        }
        let scale = (stage.width() / size.x).min(stage.height() / size.y);
        let fitted = egui::Rect::from_center_size(stage.center(), egui::vec2(size.x * scale, size.y * scale));
        egui::Image::from_texture(texture)
            .corner_radius(egui::CornerRadius::same(10))
            .paint_at(ui, fitted);
        true
    }

    fn caption(&self) -> String {
        let Some(texture) = &self.preview else {
            if self.recording.is_some() {
                return "cellCam permanece en negro".into();
            }
            return match self.shown_status() {
                "Conectando" | "Teléfono no autorizado" => "Reintento cada 2 segundos".into(),
                "Versión incompatible" => "No hay reintento útil".into(),
                _ => "Zoom recibe cellCam a 1920×1080".into(),
            };
        };
        if self.status == "Desconectado" {
            return "Zoom se quedó en esta imagen".into();
        }
        if !self.status.starts_with("En vivo") {
            return "Zoom recibe cellCam a 1920×1080".into();
        }
        let size = texture.size_vec2();
        let cam = match self.camera {
            Camera::Back => "Trasera",
            Camera::Front => "Frontal",
        };
        let link = match self.live_transport {
            Some(Transport::Usb) => "USB",
            Some(Transport::Wifi) => "Wi-Fi",
            None => match self.mode {
                LinkMode::Wifi => "Wi-Fi",
                _ => "USB",
            },
        };
        let bars = if size.y > size.x { " con barras" } else { "" };
        format!("{}×{} · {cam} · {link} · Zoom recibe 1920×1080{bars}", size.x as u32, size.y as u32)
    }

    fn empty_copy(&self) -> String {
        if self.recording.is_some() && self.preview.is_none() {
            return "Grabando sin cámara. La vista previa sigue vacía.".into();
        }
        if self.retrying() || self.status == "Conectando" {
            return "Buscando el teléfono. USB primero. Si no hay cable, sigue por la IP.".into();
        }
        match self.status.as_str() {
            "Ocupado" | "Teléfono no autorizado" | "Versión incompatible" => "Sin imagen.".into(),
            _ => "La imagen aparece al conectar. Hasta el primer cuadro, Zoom ve la cámara en negro.".into(),
        }
    }

    fn ensure_monitors(&mut self) {
        if !self.monitors.is_empty() {
            return;
        }
        self.monitors = monitors();
        self.monitor = self.picked_monitor();
    }

    fn picked_monitor(&self) -> Option<MonitorId> {
        if let Some((width, height)) = self.monitor_size {
            if let Some(item) = self.monitors.iter().find(|item| item.width == width && item.height == height) {
                return Some(item.id.clone());
            }
        }
        self.monitors.first().map(|item| item.id.clone())
    }

    fn meter_level(&self, system: bool) -> f32 {
        let Some(recording) = &self.recording else { return 0.0 };
        let (system_level, mic_level) = recording.levels();
        if system { system_level } else { mic_level }
    }

    fn monitor_label(&self) -> String {
        self.monitors
            .iter()
            .find(|item| Some(&item.id) == self.monitor.as_ref())
            .map(monitor_text)
            .or_else(|| self.monitor_size.map(|(width, height)| format!("{width}×{height}")))
            .unwrap_or_else(|| "Pantalla".into())
    }

    fn start_recording(&mut self) {
        point_at_ffmpeg();
        self.ensure_monitors();
        let Some(monitor) = self.monitor.clone().or_else(|| self.picked_monitor()) else {
            self.notice = "No hay un monitor para grabar".into();
            return;
        };
        self.monitor = Some(monitor.clone());
        let mic = if self.mic_on {
            if self.mics.is_empty() {
                self.mics = microphones();
            }
            let id = self.mic.clone().or_else(|| self.mics.first().map(|item| item.id.clone()));
            self.mic = id.clone();
            id
        } else {
            None
        };
        let output = default_output();
        match Recording::start(RecordConfig {
            monitor,
            system_audio: self.system_audio,
            mic,
            output: output.clone(),
        }) {
            Ok(recording) => {
                self.recording = Some(recording);
                self.notice = if self.status.starts_with("En vivo") {
                    "Grabando 00:00\nGraba el monitor. No usa el teléfono ni los efectos. El micrófono no sale por los parlantes.".into()
                } else {
                    "Grabando sin cámara\nLa grabación es otro trabajo. Empieza aunque no haya teléfono.".into()
                };
            }
            Err(error) => self.notice = error.message,
        }
    }

    fn stop_recording(&mut self) {
        if let Some(recording) = self.recording.take() {
            match recording.stop() {
                Ok(path) => self.notice = format!("Grabación guardada\n{}", path.display()),
                Err(error) => self.notice = error.message,
            }
        }
    }

    fn refresh_recording_notice(&mut self) {
        let Some(recording) = &self.recording else { return };
        if self.status.starts_with("En vivo") && self.notice.starts_with("Grabando ") {
            let elapsed = format_elapsed(recording.elapsed());
            self.notice = format!(
                "Grabando {elapsed}\nGraba el monitor. No usa el teléfono ni los efectos. El micrófono no sale por los parlantes."
            );
        }
    }

    fn read_config(&mut self) {
        let Ok(text) = std::fs::read_to_string(config_path()) else { return };
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            match key.trim() {
                "mode" => {
                    self.mode = match value.trim() {
                        "usb" => LinkMode::Usb,
                        "wifi" => LinkMode::Wifi,
                        _ => LinkMode::Auto,
                    }
                }
                "ip" => self.ip = value.trim().to_string(),
                "resolution" => {
                    self.resolution = if value.trim() == "1080" { Resolution::P1080 } else { Resolution::P720 }
                }
                "camera" => self.camera = if value.trim() == "front" { Camera::Front } else { Camera::Back },
                "brightness" => self.color.brightness = value.trim().parse().unwrap_or(0.0),
                "contrast" => self.color.contrast = value.trim().parse().unwrap_or(0.0),
                "saturation" => self.color.saturation = value.trim().parse().unwrap_or(0.0),
                "temperature" => self.color.temperature = value.trim().parse().unwrap_or(0.0),
                "blur" => self.blur_strength = value.trim().parse().unwrap_or(0.5),
                "system_audio" => self.system_audio = value.trim() != "0",
                "mic" => self.mic_on = value.trim() != "0",
                "monitor" => {
                    self.monitor_size = value.trim().split_once('x').and_then(|(width, height)| {
                        Some((width.parse().ok()?, height.parse().ok()?))
                    });
                }
                _ => {}
            }
        }
    }

    fn write_config(&self) {
        let path = config_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mode = match self.mode {
            LinkMode::Auto => "auto",
            LinkMode::Usb => "usb",
            LinkMode::Wifi => "wifi",
        };
        let text = format!(
            "mode={mode}\nip={}\nresolution={}\ncamera={}\nbrightness={}\ncontrast={}\nsaturation={}\ntemperature={}\nblur={}\nsystem_audio={}\nmic={}\nmonitor={}\n",
            self.ip,
            match self.resolution {
                Resolution::P720 => "720",
                Resolution::P1080 => "1080",
            },
            match self.camera {
                Camera::Back => "back",
                Camera::Front => "front",
            },
            self.color.brightness,
            self.color.contrast,
            self.color.saturation,
            self.color.temperature,
            self.blur_strength,
            if self.system_audio { "1" } else { "0" },
            if self.mic_on { "1" } else { "0" },
            match self.monitor_size {
                Some((width, height)) => format!("{width}x{height}"),
                None => String::new(),
            },
        );
        let _ = std::fs::write(path, text);
    }
}

impl eframe::App for CamdeskApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        self.refresh_recording_notice();
        self.title_bar(ctx);
        egui::TopBottomPanel::bottom("estado")
            .exact_height(28.0)
            .frame(egui::Frame::new().fill(theme::FOOTER).inner_margin(egui::Margin::symmetric(16, 0)))
            .show_separator_line(true)
            .show(ctx, |ui| {
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let mono = egui::FontId::monospace(11.0);
                    let shown = self.shown_status();
                    let tone = if shown == "Desconectado" && self.preview.is_some() {
                        theme::WARN
                    } else {
                        status_color(shown)
                    };
                    ui.spacing_mut().item_spacing.x = 7.0;
                    theme::status_dot(ui, tone);
                    ui.label(egui::RichText::new(shown).font(mono.clone()).color(theme::TEXT));
                    ui.add_space(14.0);
                    ui.label(egui::RichText::new("cellCam 1920×1080 NV12 · 30 fps").font(mono.clone()).color(theme::MUTED));
                    if let Some(recording) = &self.recording {
                        let elapsed = format_elapsed(recording.elapsed());
                        let encoder = encoder_label(recording.encoder());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(egui::RichText::new(encoder).font(mono.clone()).color(theme::SOFT));
                            ui.add_space(14.0);
                            ui.label(egui::RichText::new(format!("REC {elapsed}")).font(mono).color(theme::REC_TEXT));
                        });
                    }
                });
            });
        self.docks(ctx);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::STAGE).inner_margin(egui::Margin::symmetric(20, 16)))
            .show(ctx, |ui| self.preview_pane(ui));
        if self.dirty {
            self.write_config();
            self.dirty = false;
        }
        // Los cuadros piden su propio repintado desde el hilo de video. Esto mueve el pie y el epígrafe.
        if self.connected {
            ctx.request_repaint_after(Duration::from_millis(100));
        } else if self.recording.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
}

fn color_adjusted(color: &ColorParams) -> bool {
    color.brightness.abs() > 0.001 || color.contrast.abs() > 0.001 || color.saturation.abs() > 0.001 || color.temperature.abs() > 0.001
}

/// Color del punto del pie.
fn status_color(status: &str) -> egui::Color32 {
    if status.starts_with("En vivo") {
        theme::LIVE
    } else if status == "Conectando" {
        theme::ACCENT
    } else if status == "Ocupado" || status == "Teléfono no autorizado" {
        theme::WARN
    } else if status == "Versión incompatible" {
        theme::REC
    } else {
        theme::IDLE
    }
}

/// Tono del aviso según su primera línea.
fn notice_tone(lead: &str) -> Tone {
    let lower = lead.to_lowercase();
    if lower.contains("incompatible") || lower.contains("error") || lower.contains("no hay") || lower.contains("no se pudo") {
        Tone::Danger
    } else if lower.starts_with("grabando") {
        Tone::Danger
    } else if lower.contains("guardad") || lower.contains("último cuadro") || lower.contains("ultimo cuadro") {
        Tone::Neutral
    } else if lower.starts_with("en vivo") {
        Tone::Live
    } else if lower.contains("falta")
        || lower.contains("no es válida")
        || lower.contains("ocup")
        || lower.contains("autoriz")
        || lower.contains("nvidia")
        || lower.contains("runtime")
        || lower.contains("gpu")
        || lower.contains("desenfo")
        || lower.contains("rendimiento")
    {
        Tone::Warn
    } else {
        Tone::Info
    }
}

fn note(warning: &mut Option<String>, text: String) {
    match warning {
        Some(existing) => {
            existing.push_str(". ");
            existing.push_str(&text);
        }
        None => *warning = Some(text),
    }
}

fn point_at_ffmpeg() {
    if let Some(dir) = std::env::var_os("CAMDESK_FFMPEG_DIR") {
        if std::path::PathBuf::from(&dir).join("avcodec-62.dll").is_file() {
            return;
        }
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(mut dir) = exe.parent().map(|path| path.to_path_buf()) else { return };
    if dir.join("avcodec-62.dll").is_file() {
        return;
    }
    for _ in 0..5 {
        if let Some(found) = ffmpeg_bin(&dir.join("third_party")) {
            std::env::set_var("CAMDESK_FFMPEG_DIR", found);
            return;
        }
        if !dir.pop() {
            break;
        }
    }
}

fn ffmpeg_bin(third_party: &std::path::Path) -> Option<std::path::PathBuf> {
    let entries = std::fs::read_dir(third_party).ok()?;
    for entry in entries.flatten() {
        let bin = entry.path().join("bin");
        if bin.join("avcodec-62.dll").is_file() {
            return Some(bin);
        }
    }
    None
}

fn point_at_vcam_dll() {
    if std::env::var_os("CAMDESK_VCAM_DLL").is_some() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(dir) = exe.parent() else { return };
    let mut best: Option<std::path::PathBuf> = None;
    let mut consider = |path: std::path::PathBuf| {
        if !path.is_file() {
            return;
        }
        let replace = match &best {
            None => true,
            Some(current) => path.metadata().and_then(|meta| meta.modified()).ok() > current.metadata().and_then(|meta| meta.modified()).ok(),
        };
        if replace {
            best = Some(path);
        }
    };
    consider(dir.join("vcam.dll"));
    consider(dir.join("..").join("vcam.dll"));
    if let Ok(entries) = std::fs::read_dir(dir.join("build").join("vcam")) {
        for entry in entries.flatten() {
            consider(entry.path().join("out").join("vcam.dll"));
        }
    }
    if let Some(path) = best {
        std::env::set_var("CAMDESK_VCAM_DLL", path);
    }
}

fn app_log(message: &str) {
    let path = config_path().with_file_name("camdesk.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = std::io::Write::write_all(&mut file, message.as_bytes());
        let _ = std::io::Write::write_all(&mut file, b"\n");
    }
}

fn monitor_text(item: &record::MonitorInfo) -> String {
    format!("{} {}x{}", item.name, item.width, item.height)
}

fn status_text(status: Status) -> String {
    match status {
        Status::Conectando => "Conectando".into(),
        Status::EnVivo { width, height, .. } => format!("En vivo · {width}×{height}"),
        Status::Desconectado => "Desconectado".into(),
        Status::Ocupado => "Ocupado".into(),
        Status::NoAutorizado => "Teléfono no autorizado".into(),
        Status::Incompatible => "Versión incompatible".into(),
    }
}

fn parse_ip(text: &str) -> Option<Ipv4Addr> {
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        text.parse().ok()
    }
}

fn encoder_label(name: &str) -> String {
    let short = match name {
        "h264_nvenc" => "NVENC",
        "h264_amf" => "AMF",
        "h264_qsv" => "Quick Sync",
        "h264_mf" => "Media Foundation",
        other => other,
    };
    format!("{short} · 30 fps")
}

fn format_elapsed(elapsed: Duration) -> String {
    let total = elapsed.as_secs();
    format!("{:02}:{:02}", total / 60, total % 60)
}

fn config_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("Camdesk").join("config.toml")
}
