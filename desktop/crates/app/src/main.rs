//! Ventana de Camdesk. Al abrir no carga FFmpeg, no carga el runtime de NVIDIA,
//! no registra la cámara virtual, no crea el archivo mapeado, no corre adb,
//! no abre WASAPI y no captura la pantalla.
//!
//! El video no corre en este hilo: lo lleva `video::Video`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod theme;
mod video;
mod widgets;

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::time::Duration;

use eframe::egui;
use fx::{BlurAvailability, ColorParams, blur_availability};
use link::{Camera, Mode, Resolution, SessionConfig, Status, Transport};
use protocol::Rotation;
use theme::Tone;
use video::{Command, Report, Video};
use widgets::{Action, Icon};
use record::{MicId, MonitorId, RecordConfig, Recording, default_output, microphones, monitors};

fn main() -> eframe::Result<()> {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/camdesk.png"))
        .expect("icono inválido de Camdesk");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([1060.0, 640.0])
            .with_icon(icon),
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

const ROTATIONS: [(&str, &str, Option<Rotation>); 5] = [
    ("auto", "Automática", None),
    ("0", "0°", Some(Rotation::R0)),
    ("90", "90°", Some(Rotation::R90)),
    ("180", "180°", Some(Rotation::R180)),
    ("270", "270°", Some(Rotation::R270)),
];

struct CamdeskApp {
    mode: LinkMode,
    ip: String,
    resolution: Resolution,
    camera: Camera,
    rotation: Option<Rotation>,
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
    /// Alto que pidieron los docks en el cuadro anterior.
    dock_height: f32,
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
            rotation: None,
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
            dock_height: 360.0,
        };
        app.read_config();
        app
    }

    /// Cuatro docks en columnas: conexión, efectos, grabación y controles. Devuelve el alto usado.
    fn docks(&mut self, ui: &mut egui::Ui) -> f32 {
        let area = ui.available_rect_before_wrap();
        let width = (area.width() - 3.0) / 4.0;
        let mut bottom = area.top();
        for column in 0..4 {
            let left = area.left() + column as f32 * (width + 1.0);
            let inner = egui::Rect::from_min_size(
                egui::pos2(left + 16.0, area.top() + 14.0),
                egui::vec2(width - 32.0, 2000.0),
            );
            let mut child = ui.new_child(
                egui::UiBuilder::new().id_salt(("dock", column)).max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)),
            );
            child.spacing_mut().item_spacing.y = 7.0;
            match column {
                0 => self.connection(&mut child),
                1 => self.effects(&mut child),
                2 => self.record_ui(&mut child),
                _ => self.controls(&mut child),
            }
            bottom = bottom.max(child.min_rect().bottom() + 16.0);
        }
        let used = egui::Rect::from_min_max(area.min, egui::pos2(area.right(), bottom));
        ui.allocate_rect(used, egui::Sense::hover());
        let line = egui::Stroke::new(1.0_f32, theme::BORDER);
        for column in 1..4 {
            let x = area.left() + column as f32 * (width + 1.0) - 0.5;
            ui.painter().vline(x, ui.clip_rect().y_range(), line);
        }
        used.height()
    }

    fn connection(&mut self, ui: &mut egui::Ui) {
        widgets::section_title(ui, "CONEXIÓN");
        ui.add_space(4.0);
        widgets::field_label(ui, "Modo");
        ui.spacing_mut().button_padding.x = 4.0;
        self.dirty |= widgets::segmented(
            ui,
            &mut self.mode,
            &[
                ("Automática", LinkMode::Auto),
                ("Solo USB", LinkMode::Usb),
                ("Solo Wi-Fi", LinkMode::Wifi),
            ],
        );
        ui.add_space(4.0);
        let name = widgets::field_label(ui, "IP del teléfono");
        let ip = ui
            .add_enabled(
                self.mode != LinkMode::Usb,
                egui::TextEdit::singleline(&mut self.ip)
                    // Con color propio: si no, el ejemplo se ve como una IP ya cargada.
                    .hint_text(egui::RichText::new("192.168.1.37").color(theme::OFF))
                    .font(theme::mono(12.0))
                    .margin(egui::Margin::symmetric(10, 9))
                    .desired_width(ui.available_width() - 20.0),
            )
            .labelled_by(name.id);
        self.dirty |= ip.changed();
        ui.add_space(4.0);
        ui.spacing_mut().item_spacing.x = 10.0;
        ui.columns(2, |cols| {
            widgets::field_label(&mut cols[0], "Resolución");
            self.dirty |= widgets::segmented(&mut cols[0], &mut self.resolution, &[("720p", Resolution::P720), ("1080p", Resolution::P1080)]);
            widgets::field_label(&mut cols[1], "Cámara");
            self.dirty |= widgets::segmented(&mut cols[1], &mut self.camera, &[("Trasera", Camera::Back), ("Frontal", Camera::Front)]);
        });
        let previous = self.rotation;
        let (_, selected, _) = ROTATIONS.iter().find(|entry| entry.2 == previous).unwrap();
        let name = widgets::field_label(ui, "Rotación");
        egui::ComboBox::from_id_salt("rotacion")
            .width(ui.available_width())
            .selected_text(*selected)
            .show_ui(ui, |ui| {
                for (_, label, rotation) in ROTATIONS {
                    ui.selectable_value(&mut self.rotation, rotation, label);
                }
            })
            .response
            .labelled_by(name.id);
        if self.rotation != previous {
            self.dirty = true;
            if let Some(video) = &self.video {
                video.send(Command::Rotation(self.rotation));
            }
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
            self.notice = LAST_FRAME.into();
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
        ui.horizontal(|ui| {
            ui.spacing_mut().interact_size.y = 24.0;
            widgets::section_title(ui, "EFECTOS");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::ghost_button(ui, "Restablecer").clicked() {
                    self.color = ColorParams::default();
                    self.dirty = true;
                    self.send_color();
                }
            });
        });
        let available = matches!(self.blur, Some(BlurAvailability::Available));
        egui::Frame::new()
            .fill(theme::INSET)
            .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
            .corner_radius(8.0)
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add_enabled_ui(available, |ui| self.blur_controls(ui));
                if !self.blur_note.is_empty() {
                    ui.label(egui::RichText::new(&self.blur_note).font(theme::body(12.0)).color(theme::TEXT_SOFT));
                }
                if !available {
                    ui.hyperlink_to(
                        egui::RichText::new("Descargar el runtime").font(theme::body(12.0)),
                        "https://www.nvidia.com/en-us/geforce/broadcasting/broadcast-sdk/resources/",
                    );
                }
            });
        if !available && self.notice.is_empty() {
            self.notice = "Sin desenfoque\nSin GPU NVIDIA, o sin el runtime de NVIDIA Video Effects. La cámara sigue.".into();
        }
        let mut changed = false;
        for (label, value) in [
            ("Brillo", &mut self.color.brightness),
            ("Contraste", &mut self.color.contrast),
            ("Saturación", &mut self.color.saturation),
            ("Temperatura", &mut self.color.temperature),
        ] {
            let mut percent = (*value * 100.0).round();
            let shown = signed(percent);
            if widgets::slider(ui, label, &mut percent, -100.0..=100.0, &shown) {
                *value = percent / 100.0;
                changed = true;
            }
        }
        if changed {
            self.dirty = true;
            self.send_color();
        }
    }

    fn blur_controls(&mut self, ui: &mut egui::Ui) {
        if widgets::checkbox(ui, &mut self.blur_on, "Desenfocar fondo", Icon::None, "NVIDIA RTX").changed() {
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
        ui.add_enabled_ui(self.blur_on, |ui| {
            let mut percent = (self.blur_strength * 100.0).round();
            let shown = format!("{percent:.0} %");
            if widgets::slider(ui, "Fuerza", &mut percent, 0.0..=100.0, &shown) {
                self.blur_strength = percent / 100.0;
                self.dirty = true;
                if let Some(video) = &self.video {
                    video.send(Command::Blur { on: true, strength: self.blur_strength });
                }
            }
        });
    }

    fn record_ui(&mut self, ui: &mut egui::Ui) {
        widgets::section_title(ui, "GRABACIÓN");
        ui.add_space(4.0);
        self.ensure_monitors();
        let name = widgets::field_label(ui, "Monitor");
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
            })
            .response
            .labelled_by(name.id);
        ui.add_space(6.0);
        let level = if self.system_audio { self.meter_level(true) } else { 0.0 };
        self.dirty |= widgets::checkbox(ui, &mut self.system_audio, "Audio del sistema", Icon::Speaker, &level_db(level)).changed();
        widgets::level_meter(ui, level);
        ui.add_space(6.0);
        let level = if self.mic_on { self.meter_level(false) } else { 0.0 };
        self.dirty |= widgets::checkbox(ui, &mut self.mic_on, "Micrófono", Icon::Mic, &level_db(level)).changed();
        widgets::level_meter(ui, level);
        let mic_label = self
            .mics
            .iter()
            .find(|item| Some(&item.id) == self.mic.as_ref())
            .map(|item| item.name.clone())
            .unwrap_or_else(|| "Comunicaciones predeterminado".into());
        ui.add_enabled_ui(self.mic_on, |ui| {
            egui::ComboBox::from_id_salt("microfono")
                .width(ui.available_width())
                .truncate()
                .selected_text(mic_label)
                .show_ui(ui, |ui| {
                    // WASAPI recién se abre al desplegar la lista o al grabar.
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
        });
        ui.label(
            egui::RichText::new("El micrófono no sale por los parlantes. Los efectos no tocan la grabación.")
                .font(theme::body(11.0))
                .color(theme::MUTED),
        );
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        widgets::section_title(ui, "CONTROLES");
        ui.add_space(4.0);
        let (label, action) = if !self.connected {
            ("Conectar", Action::Primary)
        } else if self.shown_status() == "Conectando" || self.status == "Teléfono no autorizado" {
            ("Cancelar", Action::Neutral)
        } else {
            ("Desconectar", Action::Neutral)
        };
        if widgets::big_button(ui, label, action).clicked() {
            if !self.connected {
                self.connect(ui.ctx());
            } else {
                self.disconnect();
            }
        }
        ui.add_space(2.0);
        let recording = self.recording.is_some();
        let (label, action) = if recording { ("Detener", Action::Stop) } else { ("Grabar", Action::Record) };
        if widgets::big_button(ui, label, action).clicked() {
            if recording {
                self.stop_recording();
            } else {
                self.start_recording();
            }
        }
        if let Some(error) = self.recording.as_ref().and_then(|rec| rec.error()) {
            self.notice = error;
        }
        ui.add_space(6.0);
        self.notice_box(ui);
    }

    fn notice_box(&self, ui: &mut egui::Ui) {
        let text = if !self.notice.is_empty() {
            self.notice.as_str()
        } else if !self.connected && self.recording.is_none() {
            "Listo para conectar\nConectar el teléfono por USB o Wi-Fi. Nada se carga hasta entonces."
        } else {
            return;
        };
        let (title, body) = text.split_once('\n').unwrap_or((text, ""));
        let (fill, border, color) = Tone::of(title).colors();
        egui::Frame::new()
            .fill(fill)
            .stroke(egui::Stroke::new(1.0_f32, border))
            .corner_radius(8.0)
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.add(egui::Label::new(egui::RichText::new(title).font(theme::semibold(13.0)).color(color)).wrap());
                if !body.is_empty() {
                    // La ruta del MP4 va en monoespaciada.
                    let font = if title == "Grabación guardada" { theme::mono(12.0) } else { theme::body(12.0) };
                    ui.add(egui::Label::new(egui::RichText::new(body).font(font).color(theme::TEXT_SOFT)).wrap());
                }
            });
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
        video.send(Command::Rotation(self.rotation));
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
                    self.notice = "En vivo\nLa vista previa y cellCam reciben la imagen. La grabación no.".into();
                }
            }
        } else {
            self.live_transport = None;
        }
        // Con Incompatible y Ocupado link no reintenta y el hilo de video ya soltó la sesión.
        if matches!(status, Status::Incompatible) {
            self.notice = "Versión incompatible\ncellCam habla otra versión del protocolo. Actualizar el teléfono o Camdesk.".into();
            self.connected = false;
        }
        if matches!(status, Status::Ocupado) {
            self.notice = "Teléfono ocupado\nOtro cliente ya está conectado. Cerrar OBS y volver a conectar.".into();
            self.connected = false;
        }
        if matches!(status, Status::NoAutorizado) {
            self.notice = "Teléfono no autorizado\nAceptar la depuración USB en el teléfono. Camdesk reintenta cada 2 segundos.".into();
        }
        if matches!(status, Status::Desconectado) && self.preview.is_some() && self.recording.is_none() {
            self.notice = LAST_FRAME.into();
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
            self.notice = "Modo rendimiento\nEl desenfoque pasó a modo rendimiento para sostener 30 fps.".into();
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
            "Teléfono vertical\nLa vista previa muestra el teléfono. Las barras las agrega Zoom.".into()
        } else {
            "En vivo\nLa vista previa y cellCam reciben la imagen. La grabación no.".into()
        };
    }

    /// La vista previa vive en un marco 16:9, el formato de la cámara virtual.
    fn stage(&self, ui: &mut egui::Ui) {
        let area = ui.available_rect_before_wrap();
        ui.allocate_rect(area, egui::Sense::hover());
        let caption_height = 24.0;
        let width = area.width().min(theme::PREVIEW_MAX_WIDTH).min((area.height() - caption_height) * 16.0 / 9.0).max(160.0);
        let size = egui::vec2(width, width * 9.0 / 16.0);
        let top = area.center().y - (size.y + caption_height) * 0.5;
        let frame = egui::Rect::from_min_size(egui::pos2(area.center().x - size.x * 0.5, top), size);
        let painter = ui.painter();
        painter.rect_filled(frame, 10.0, theme::VIEWPORT);
        let live = self.status.starts_with("En vivo");
        let frozen = self.status == "Desconectado" && self.preview.is_some();
        let painted = (live || frozen) && self.paint_frame(ui, frame);
        if !painted {
            let wrap = (frame.width() - 48.0).min(440.0);
            let mut job = egui::text::LayoutJob::simple(self.empty_copy(), theme::body(14.0), theme::MUTED, wrap);
            job.halign = egui::Align::Center;
            let galley = painter.layout_job(job);
            let block = 36.0 + 12.0 + galley.size().y;
            let top = frame.center().y - block * 0.5;
            widgets::paint_phone(painter, egui::pos2(frame.center().x, top + 18.0));
            // Centrado: el origen del bloque de texto es su eje.
            painter.galley(egui::pos2(frame.center().x, top + 48.0), galley, theme::MUTED);
        }
        painter.rect_stroke(frame, 10.0, egui::Stroke::new(1.0_f32, theme::BORDER), egui::StrokeKind::Outside);
        let inset = egui::vec2(12.0, 12.0);
        if painted && live {
            widgets::badge(painter, frame.left_top() + inset, egui::Align2::LEFT_TOP, theme::LIVE, "EN VIVO", theme::LIVE_TEXT);
        }
        if let Some(recording) = &self.recording {
            let text = format!("REC {}", format_elapsed(recording.elapsed()));
            let anchor = egui::pos2(frame.right() - inset.x, frame.top() + inset.y);
            widgets::badge(painter, anchor, egui::Align2::RIGHT_TOP, theme::REC, &text, theme::REC_TEXT);
        }
        if painted && frozen {
            widgets::badge(painter, frame.right_bottom() - inset, egui::Align2::RIGHT_BOTTOM, theme::WARN, "Último cuadro", theme::WARN);
        }
        painter.text(
            egui::pos2(frame.center().x, frame.bottom() + 14.0),
            egui::Align2::CENTER_CENTER,
            self.caption(),
            theme::mono(12.0),
            theme::MUTED,
        );
    }

    fn paint_frame(&self, ui: &egui::Ui, frame: egui::Rect) -> bool {
        let Some(texture) = &self.preview else { return false };
        let size = texture.size_vec2();
        if size.x <= 0.0 || size.y <= 0.0 {
            return false;
        }
        let scale = (frame.width() / size.x).min(frame.height() / size.y);
        let fitted = egui::Rect::from_center_size(frame.center(), size * scale);
        let radius = if fitted.width() >= frame.width() - 1.0 { 10 } else { 0 };
        egui::Image::from_texture(texture)
            .corner_radius(egui::CornerRadius::same(radius))
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
                    "Grabando 00:00\nGraba el monitor. No usa el teléfono ni los efectos.".into()
                } else {
                    "Grabando sin cámara\nLa grabación es otro trabajo: graba el monitor aunque cellCam no tenga imagen.".into()
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
            self.notice = format!("Grabando {elapsed}\nGraba el monitor. No usa el teléfono ni los efectos.");
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
                "rotation" => {
                    self.rotation = ROTATIONS.iter().find(|entry| entry.0 == value.trim()).and_then(|entry| entry.2);
                }
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
            "mode={mode}\nip={}\nresolution={}\ncamera={}\nrotation={}\nbrightness={}\ncontrast={}\nsaturation={}\ntemperature={}\nblur={}\nsystem_audio={}\nmic={}\nmonitor={}\n",
            self.ip,
            match self.resolution {
                Resolution::P720 => "720",
                Resolution::P1080 => "1080",
            },
            match self.camera {
                Camera::Back => "back",
                Camera::Front => "front",
            },
            ROTATIONS.iter().find(|entry| entry.2 == self.rotation).unwrap().0,
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

impl CamdeskApp {
    fn header(&self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            widgets::logo(ui);
            ui.label(egui::RichText::new("Camdesk").font(theme::semibold(14.0)));
        });
    }

    fn footer(&self, ui: &mut egui::Ui) {
        let small = |text: &str, color: egui::Color32| egui::RichText::new(text).font(theme::mono(11.0)).color(color);
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let shown = self.shown_status();
            let color = if shown == "Desconectado" && self.preview.is_some() { theme::WARN } else { theme::status_dot(shown) };
            widgets::dot(ui, color);
            ui.label(small(shown, theme::TEXT_SOFT));
            ui.add_space(12.0);
            ui.label(small("cellCam 1920×1080 NV12 · 30 fps", theme::TEXT_SOFT));
            if let Some(recording) = &self.recording {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(small(&encoder_label(recording.encoder()), theme::TEXT_SOFT));
                    ui.add_space(12.0);
                    ui.label(small(&format!("REC {}", format_elapsed(recording.elapsed())), theme::REC_TEXT));
                });
            }
        });
    }
}

impl eframe::App for CamdeskApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        self.refresh_recording_notice();
        let bar = |fill: egui::Color32| egui::Frame::new().fill(fill).inner_margin(egui::Margin::symmetric(16, 0));
        egui::TopBottomPanel::top("cabecera")
            .exact_height(theme::HEADER_HEIGHT)
            .frame(bar(theme::TITLEBAR))
            .show(ctx, |ui| self.header(ui));
        egui::TopBottomPanel::bottom("pie")
            .exact_height(theme::FOOTER_HEIGHT)
            .frame(bar(theme::FOOTER))
            .show(ctx, |ui| self.footer(ui));
        // Los docks piden su alto. En una ventana baja se quedan con una parte y se desplazan.
        let room = ctx.screen_rect().height() - theme::HEADER_HEIGHT - theme::FOOTER_HEIGHT;
        let height = self.dock_height.min(room * 0.64);
        let mut wanted = self.dock_height;
        egui::TopBottomPanel::bottom("docks")
            .exact_height(height)
            .frame(egui::Frame::new().fill(theme::PANEL))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| wanted = self.docks(ui));
            });
        if (wanted - self.dock_height).abs() > 0.5 {
            self.dock_height = wanted;
            ctx.request_repaint();
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::STAGE).inner_margin(egui::Margin::symmetric(16, 14)))
            .show(ctx, |ui| self.stage(ui));
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

const LAST_FRAME: &str = "Último cuadro\nZoom se quedó en esta imagen. La cámara virtual no pasa a negro.";

fn signed(value: f32) -> String {
    if value > 0.0 {
        format!("+{value:.0}")
    } else {
        format!("{value:.0}")
    }
}

/// `record` entrega el pico de 0 a 1, lineal entre −48 dB y 0 dB.
fn level_db(level: f32) -> String {
    if level <= 0.0 {
        return "−∞ dB".into();
    }
    let below = 48.0 * (1.0 - level.min(1.0));
    if below < 0.5 {
        "0 dB".into()
    } else {
        format!("−{below:.0} dB")
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
