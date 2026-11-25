//! Ventana de Camdesk. Al abrir no carga FFmpeg, no carga el runtime de NVIDIA,
//! no registra la cámara virtual, no crea el archivo mapeado, no corre adb,
//! no abre WASAPI y no captura la pantalla.

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use eframe::egui;
use fx::{BackgroundBlur, BlurAvailability, ColorCorrector, ColorParams, blur_availability};
use link::{Camera, Event, Mode, Resolution, Session, SessionConfig, Status, Transport};
use record::{MicId, MonitorId, RecordConfig, Recording, default_output, microphones, monitors};

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1120.0, 700.0])
            .with_min_inner_size([960.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Camdesk",
        options,
        Box::new(|cc| {
            apply_style(&cc.egui_ctx);
            Ok(Box::new(CamdeskApp::load()))
        }),
    )
}

struct CamdeskApp {
    mode: LinkMode,
    ip: String,
    resolution: Resolution,
    camera: Camera,
    color: ColorParams,
    color_open: bool,
    blur_on: bool,
    blur_strength: f32,
    blur: Option<BlurAvailability>,
    blur_note: String,
    _blur_fx: Option<BackgroundBlur>,
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
    session: Option<Session>,
    events: Option<Receiver<Event>>,
    decoder: Option<decode::Decoder>,
    writer: Option<vcam_shm::Writer>,
    vcam: Option<vcam::VirtualCamera>,
    color_fx: ColorCorrector,
    preview: Option<egui::TextureHandle>,
    rgba: Vec<u8>,
    canvas_y: Vec<u8>,
    canvas_uv: Vec<u8>,
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
            color_open: false,
            blur_on: false,
            blur_strength: 0.5,
            blur: None,
            blur_note: String::new(),
            _blur_fx: None,
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
            session: None,
            events: None,
            decoder: None,
            writer: None,
            vcam: None,
            color_fx: ColorCorrector::new(),
            preview: None,
            rgba: Vec::new(),
            canvas_y: Vec::new(),
            canvas_uv: Vec::new(),
            recording: None,
            dirty: false,
        };
        app.read_config();
        app
    }

    fn rail(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("riel")
            .exact_width(320.0)
            .resizable(false)
            .show_separator_line(true)
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin::symmetric(16, 8)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    self.connection(ui);
                    self.effects(ui);
                    self.record_ui(ui);
                });
            });
    }

    fn connection(&mut self, ui: &mut egui::Ui) {
        section(ui, "Conexión");
        self.dirty |= segments(
            ui,
            &mut self.mode,
            &[
                ("Automática", LinkMode::Auto),
                ("Solo USB", LinkMode::Usb),
                ("Solo Wi-Fi", LinkMode::Wifi),
            ],
        );
        let hint = if self.mode == LinkMode::Wifi { "IP del teléfono" } else { "IP, si hace falta" };
        let ip = ui.add_enabled(
            self.mode != LinkMode::Usb,
            egui::TextEdit::singleline(&mut self.ip).hint_text(hint).desired_width(ui.available_width()),
        );
        self.dirty |= ip.changed();
        self.dirty |= segments(ui, &mut self.resolution, &[("720p", Resolution::P720), ("1080p", Resolution::P1080)]);
        self.dirty |= segments(ui, &mut self.camera, &[("Trasera", Camera::Back), ("Frontal", Camera::Front)]);
        let label = if self.session.is_none() {
            "Conectar"
        } else if self.shown_status() == "Conectando" || self.status == "Teléfono no autorizado" {
            "Cancelar"
        } else {
            "Desconectar"
        };
        if action_button(ui, label, Action::Primary).clicked() {
            if self.session.is_none() {
                self.connect();
            } else {
                self.disconnect();
            }
        }
    }

    fn retrying(&self) -> bool {
        self.session.is_some()
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
        self.session = None;
        self.events = None;
        if let Some(decoder) = self.decoder.as_mut() {
            decoder.reset();
        }
        self.status = "Desconectado".into();
        if self.preview.is_some() && self.recording.is_none() {
            self.notice = "Último cuadro\nSi se corta la red, la cámara virtual no pasa a negro.".into();
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
        section(ui, "Imagen");
        let available = matches!(self.blur, Some(BlurAvailability::Available));
        ui.add_enabled_ui(available, |ui| {
            if toggle(ui, &mut self.blur_on, "Desenfocar fondo").changed() {
                self.dirty = true;
                if self.blur_on {
                    match BackgroundBlur::new() {
                        Ok(mut fx) => {
                            fx.set_strength(self.blur_strength);
                            self._blur_fx = Some(fx);
                            self.blur_note.clear();
                        }
                        Err(error) => {
                            self.blur_on = false;
                            self._blur_fx = None;
                            self.blur_note = error.message;
                        }
                    }
                } else {
                    self._blur_fx = None;
                }
            }
            if self.blur_on {
                let mut percent = (self.blur_strength * 100.0).round();
                if labeled_slider(ui, "Fuerza", &mut percent, 0.0..=100.0, true) {
                    self.blur_strength = percent / 100.0;
                    self.dirty = true;
                    if let Some(fx) = self._blur_fx.as_mut() {
                        fx.set_strength(self.blur_strength);
                    }
                }
            }
        });
        if !self.blur_note.is_empty() {
            ui.label(egui::RichText::new(&self.blur_note).size(12.0).color(MUTED));
        }
        if !available {
            ui.hyperlink_to(
                "Descargar el runtime",
                "https://www.nvidia.com/en-us/geforce/broadcasting/broadcast-sdk/resources/",
            );
        }
        if disclosure(ui, "Corrección de color", color_adjusted(&self.color).then_some("Ajustada")).clicked() {
            self.color_open = !self.color_open;
        }
        if self.color_open {
            let mut changed = false;
            changed |= labeled_slider(ui, "Brillo", &mut self.color.brightness, -1.0..=1.0, false);
            changed |= labeled_slider(ui, "Contraste", &mut self.color.contrast, -1.0..=1.0, false);
            changed |= labeled_slider(ui, "Saturación", &mut self.color.saturation, -1.0..=1.0, false);
            changed |= labeled_slider(ui, "Temperatura", &mut self.color.temperature, -1.0..=1.0, false);
            if changed {
                self.dirty = true;
            }
            if ui
                .add(
                    egui::Button::new(egui::RichText::new("Restablecer").color(INFO))
                        .fill(egui::Color32::TRANSPARENT)
                        .stroke(egui::Stroke::NONE),
                )
                .clicked()
            {
                self.color = ColorParams::default();
                self.dirty = true;
            }
        }
    }

    fn record_ui(&mut self, ui: &mut egui::Ui) {
        section(ui, "Grabación");
        self.ensure_monitors();
        ui.horizontal(|ui| {
            ui.label("Monitor");
            let label = self.monitor_label();
            egui::ComboBox::from_id_salt("monitor")
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
        self.dirty |= toggle(ui, &mut self.system_audio, "Audio del sistema").changed();
        if self.system_audio {
            level_meter(ui, self.meter_level(true));
        }
        self.dirty |= toggle(ui, &mut self.mic_on, "Micrófono").changed();
        if self.mic_on {
            level_meter(ui, self.meter_level(false));
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
        let recording = self.recording.is_some();
        let label = if recording { "Detener" } else { "Grabar" };
        let kind = if recording { Action::Danger } else { Action::Quiet };
        if action_button(ui, label, kind).clicked() {
            if recording {
                self.stop_recording();
            } else {
                self.start_recording();
            }
        }
        if let Some(rec) = &self.recording {
            if let Some(error) = rec.error() {
                self.notice = error;
            }
        }
    }

    fn connect(&mut self) {
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
        let warning = self.ensure_pipeline();
        match Session::start(SessionConfig {
            mode,
            resolution: self.resolution,
            camera: self.camera,
            wifi,
        }) {
            Ok((session, events)) => {
                self.session = Some(session);
                self.events = Some(events);
                self.status = "Conectando".into();
                self.notice = warning.unwrap_or_else(|| {
                    "Conectando\nUSB si el teléfono está autorizado. Si no, la IP.".into()
                });
            }
            Err(error) => self.notice = error.to_string(),
        }
    }

    fn ensure_pipeline(&mut self) -> Option<String> {
        point_at_ffmpeg();
        let mut warning = None;
        if self.decoder.is_none() {
            match decode::Decoder::new() {
                Ok(decoder) => self.decoder = Some(decoder),
                Err(error) => note(&mut warning, error.to_string()),
            }
        }
        if self.writer.is_none() {
            let path = vcam_shm::default_path();
            match vcam_shm::Writer::create(&path) {
                Ok(writer) => self.writer = Some(writer),
                Err(error) if error.raw_os_error() == Some(1224) => match vcam_shm::Writer::open_existing(&path) {
                    Ok(writer) => self.writer = Some(writer),
                    Err(error) => note(&mut warning, format!("archivo de la cámara: {error}")),
                },
                Err(error) => note(&mut warning, format!("archivo de la cámara: {error}")),
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
        if let Some(events) = self.events.as_ref() {
            while let Ok(event) = events.try_recv() {
                batch.push(event);
            }
        }
        let mut latest = None;
        for event in batch {
            match event {
                Event::Status(status) => {
                    if matches!(status, Status::Conectando) {
                        if let Some(decoder) = self.decoder.as_mut() {
                            decoder.reset();
                        }
                    }
                    if let Status::EnVivo { camera, transport, .. } = status {
                        self.live_transport = Some(transport);
                        if camera != self.camera {
                            self.camera = camera;
                            self.dirty = true;
                            self.notice = match camera {
                                Camera::Back => "El teléfono se quedó en la cámara trasera".into(),
                                Camera::Front => "El teléfono se quedó en la cámara frontal".into(),
                            };
                        } else if self.notice.starts_with("Conectando") {
                            self.notice.clear();
                        }
                    } else {
                        self.live_transport = None;
                    }
                    if matches!(status, Status::Desconectado) && self.preview.is_some() && self.recording.is_none() {
                        self.notice = "Último cuadro\nSi se corta la red, la cámara virtual no pasa a negro.".into();
                    }
                    self.status = status_text(status);
                }
                Event::Frame(frame) => {
                    if let Some(image) = self.push_frame(frame) {
                        latest = Some(image);
                    }
                }
            }
        }
        if let Some(image) = latest {
            self.publish(ctx, image);
        }
    }

    fn push_frame(&mut self, frame: protocol::Frame) -> Option<decode::Decoded> {
        let decoder = self.decoder.as_mut()?;
        match decoder.push(&frame) {
            Ok(image) => image,
            Err(decode::DecodeError::NeedKeyframe) => {
                if let Some(session) = &self.session {
                    session.request_keyframe();
                }
                None
            }
            Err(decode::DecodeError::Message(text)) => {
                self.notice = text.clone();
                app_log(&text);
                None
            }
        }
    }

    fn publish(&mut self, ctx: &egui::Context, mut decoded: decode::Decoded) {
        let width = decoded.width as usize;
        let height = decoded.height as usize;
        let stride = width.saturating_mul(4);
        if self.blur_on {
            let failed = self
                ._blur_fx
                .as_mut()
                .and_then(|fx| fx.apply(&mut decoded.bgra, width, height, stride).err());
            if let Some(error) = failed {
                self.blur_on = false;
                self._blur_fx = None;
                self.blur_note = error.message.clone();
                self.notice = error.message;
            } else if let Some(note) = self._blur_fx.as_ref().and_then(|fx| fx.notice()) {
                self.blur_note = note.to_string();
            }
        }
        self.color_fx.set(self.color);
        self.color_fx.apply(&mut decoded.bgra, width, height, stride);
        self.upload_preview(ctx, &decoded);
        self.write_canvas(&decoded);
    }

    fn upload_preview(&mut self, ctx: &egui::Context, decoded: &decode::Decoded) {
        let width = decoded.width as usize;
        let height = decoded.height as usize;
        self.rgba.resize(width.saturating_mul(height).saturating_mul(4), 0);
        for (dst, src) in self.rgba.chunks_exact_mut(4).zip(decoded.bgra.chunks_exact(4)) {
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            dst[3] = 255;
        }
        let image = egui::ColorImage::from_rgba_unmultiplied([width, height], &self.rgba);
        if let Some(texture) = self.preview.as_mut() {
            texture.set(image, egui::TextureOptions::LINEAR);
        } else {
            self.preview = Some(ctx.load_texture("preview", image, egui::TextureOptions::LINEAR));
        }
    }

    fn write_canvas(&mut self, decoded: &decode::Decoded) {
        self.canvas_y.resize(vcam_shm::Y_BYTES, 16);
        self.canvas_uv.resize(vcam_shm::UV_BYTES, 128);
        if let Some(decoder) = self.decoder.as_mut() {
            decoder.to_canvas_nv12(decoded, &mut self.canvas_y, &mut self.canvas_uv, vcam_shm::STRIDE as usize);
        }
        let y = std::mem::take(&mut self.canvas_y);
        let uv = std::mem::take(&mut self.canvas_uv);
        if let Some(writer) = self.writer.as_mut() {
            let _ = writer.write(decoded.pts_us.saturating_mul(10), |dst_y, dst_uv| {
                if dst_y.len() == y.len() && dst_uv.len() == uv.len() {
                    dst_y.copy_from_slice(&y);
                    dst_uv.copy_from_slice(&uv);
                }
            });
        }
        self.canvas_y = y;
        self.canvas_uv = uv;
    }

    fn preview_pane(&self, ui: &mut egui::Ui) {
        let available = ui.available_size();
        let (rect, _) = ui.allocate_exact_size(available, egui::Sense::hover());
        let caption = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - 22.0), rect.right_bottom());
        let canvas = egui::Rect::from_min_max(rect.min, egui::pos2(rect.right(), caption.top())).shrink(8.0);
        let live = self.status.starts_with("En vivo");
        let frozen = self.status == "Desconectado" && self.preview.is_some();
        if (live || frozen) && self.paint_frame(ui, canvas) {
            // La imagen ya está pintada.
        } else {
            let galley = ui.painter().layout(self.empty_copy(), egui::FontId::proportional(14.0), MUTED, canvas.width().min(420.0));
            ui.painter().galley(canvas.center() - galley.size() * 0.5, galley, MUTED);
        }
        ui.painter().text(
            caption.left_center(),
            egui::Align2::LEFT_CENTER,
            self.caption(),
            egui::FontId::proportional(12.0),
            MUTED,
        );
    }

    fn paint_frame(&self, ui: &egui::Ui, canvas: egui::Rect) -> bool {
        let Some(texture) = &self.preview else { return false };
        let size = texture.size_vec2();
        if size.x <= 0.0 || size.y <= 0.0 {
            return false;
        }
        let scale = (canvas.width() / size.x).min(canvas.height() / size.y);
        let fitted = egui::Rect::from_center_size(canvas.center(), egui::vec2(size.x * scale, size.y * scale));
        egui::Image::from_texture(texture)
            .corner_radius(egui::CornerRadius::same(12))
            .paint_at(ui, fitted);
        if self.recording.is_some() {
            paint_badge(ui, fitted.left_top() + egui::vec2(12.0, 12.0), egui::Align2::LEFT_TOP, "REC", REC);
        }
        if self.status == "Desconectado" {
            paint_badge(ui, fitted.right_bottom() - egui::vec2(12.0, 12.0), egui::Align2::RIGHT_BOTTOM, "Último cuadro", WARN);
        }
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
        self.rail(ctx);
        if !self.notice.is_empty() {
            egui::TopBottomPanel::top("aviso")
                .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin::symmetric(18, 8)))
                .show_separator_line(false)
                .show(ctx, |ui| {
                    let (lead, body) = self.notice.split_once('\n').unwrap_or((self.notice.as_str(), ""));
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(lead).strong().color(notice_color(lead)));
                        if !body.is_empty() {
                            ui.label(egui::RichText::new(body).color(MUTED));
                        }
                    });
                });
        }
        egui::TopBottomPanel::bottom("estado")
            .exact_height(36.0)
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin::symmetric(18, 0)))
            .show_separator_line(true)
            .show(ctx, |ui| {
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new("cellCam").color(MUTED));
                    let shown = self.shown_status();
                    let status_tone = if shown == "Desconectado" && self.preview.is_some() {
                        WARN
                    } else {
                        status_color(shown)
                    };
                    ui.label(egui::RichText::new(shown).color(status_tone));
                    if let Some(recording) = &self.recording {
                        let elapsed = format_elapsed(recording.elapsed());
                        let encoder = encoder_label(recording.encoder());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(egui::RichText::new(format!("REC  {elapsed}")).color(REC).strong());
                            ui.add_space(14.0);
                            ui.label(egui::RichText::new(encoder).color(MUTED));
                        });
                    }
                });
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin::symmetric(16, 10)))
            .show(ctx, |ui| self.preview_pane(ui));
        if self.dirty {
            self.write_config();
            self.dirty = false;
        }
        if self.session.is_some() {
            ctx.request_repaint_after(Duration::from_millis(16));
        } else if self.recording.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
}

const BG: egui::Color32 = egui::Color32::from_rgb(0x12, 0x12, 0x12);
const FIELD: egui::Color32 = egui::Color32::from_rgb(0x1b, 0x1b, 0x1b);
const TEXT: egui::Color32 = egui::Color32::from_rgb(0xf2, 0xf2, 0xf2);
const MUTED: egui::Color32 = egui::Color32::from_rgb(0x9c, 0x9c, 0x9c);
const LIVE: egui::Color32 = egui::Color32::from_rgb(0x3d, 0xba, 0x86);
const REC: egui::Color32 = egui::Color32::from_rgb(0xe2, 0x3d, 0x3d);
const WARN: egui::Color32 = egui::Color32::from_rgb(0xd7, 0xa4, 0x41);
const INFO: egui::Color32 = egui::Color32::from_rgb(0x9e, 0xc0, 0xdc);
const DANGER: egui::Color32 = egui::Color32::from_rgb(0xe0, 0x7a, 0x7a);
const INK: egui::Color32 = egui::Color32::from_rgb(0x14, 0x14, 0x14);

enum Action {
    Primary,
    Quiet,
    Danger,
}

fn apply_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals.dark_mode = true;
    style.visuals.panel_fill = BG;
    style.visuals.window_fill = BG;
    style.visuals.extreme_bg_color = FIELD;
    style.visuals.faint_bg_color = FIELD;
    style.visuals.override_text_color = Some(TEXT);
    style.visuals.hyperlink_color = INFO;
    style.visuals.selection.bg_fill = egui::Color32::from_rgb(0x2c, 0x2c, 0x2c);
    style.visuals.selection.stroke = egui::Stroke::NONE;
    style.visuals.window_stroke = egui::Stroke::NONE;
    let radius = egui::CornerRadius::same(8);
    for visuals in [
        &mut style.visuals.widgets.noninteractive,
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        visuals.corner_radius = radius;
        visuals.bg_stroke = egui::Stroke::NONE;
    }
    style.visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(22));
    style.visuals.widgets.noninteractive.fg_stroke.color = TEXT;
    style.visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(0x2a, 0x2a, 0x2a);
    style.visuals.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
    style.visuals.widgets.inactive.fg_stroke.color = TEXT;
    style.visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(0x32, 0x32, 0x32);
    style.visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(0x2a, 0x2a, 0x2a);
    style.visuals.widgets.hovered.fg_stroke.color = TEXT;
    style.visuals.widgets.active.bg_fill = egui::Color32::from_rgb(0x3a, 0x3a, 0x3a);
    style.visuals.widgets.active.weak_bg_fill = egui::Color32::from_rgb(0x33, 0x33, 0x33);
    style.visuals.widgets.active.fg_stroke.color = TEXT;
    style.visuals.widgets.open.bg_fill = FIELD;
    style.visuals.widgets.open.fg_stroke.color = TEXT;
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    ctx.set_style(style);
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(8.0);
    ui.label(egui::RichText::new(title.to_uppercase()).size(11.0).color(MUTED).strong());
    ui.add_space(2.0);
}

fn segments<T: Copy + PartialEq>(ui: &mut egui::Ui, value: &mut T, options: &[(&str, T)]) -> bool {
    let mut changed = false;
    egui::Frame::new()
        .fill(FIELD)
        .corner_radius(9.0)
        .inner_margin(egui::Margin::same(2))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.columns(options.len(), |cols| {
                for (col, (label, option)) in cols.iter_mut().zip(options) {
                    col.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                    let selected = *value == *option;
                    let text = egui::RichText::new(*label).size(12.0).color(if selected { TEXT } else { MUTED });
                    let button = egui::Button::new(text)
                        .fill(if selected { egui::Color32::from_rgb(0x2c, 0x2c, 0x2c) } else { egui::Color32::TRANSPARENT })
                        .stroke(egui::Stroke::NONE)
                        .corner_radius(7.0)
                        .wrap_mode(egui::TextWrapMode::Extend)
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

fn level_meter(ui: &mut egui::Ui, level: f32) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 3.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 2.0, egui::Color32::from_rgb(0x2a, 0x2a, 0x2a));
    let fill = rect.width() * level.clamp(0.0, 1.0);
    if fill > 0.5 {
        ui.painter().rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(fill, rect.height())),
            2.0,
            LIVE,
        );
    }
}

fn toggle(ui: &mut egui::Ui, on: &mut bool, label: &str) -> egui::Response {
    let (rect, mut response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let painter = ui.painter();
    painter.text(rect.left_center(), egui::Align2::LEFT_CENTER, label, egui::FontId::proportional(13.0), TEXT);
    let switch = egui::Rect::from_center_size(egui::pos2(rect.right() - 17.0, rect.center().y), egui::vec2(34.0, 20.0));
    let track = if *on { egui::Color32::from_rgb(0x2f, 0x6d, 0x52) } else { egui::Color32::from_rgb(0x2a, 0x2a, 0x2a) };
    painter.rect_filled(switch, 10.0, track);
    let knob_x = if *on { switch.right() - 10.0 } else { switch.left() + 10.0 };
    let knob = if *on { egui::Color32::WHITE } else { egui::Color32::from_rgb(0x9a, 0x9a, 0x9a) };
    painter.circle_filled(egui::pos2(knob_x, switch.center().y), 7.0, knob);
    response
}

fn disclosure(ui: &mut egui::Ui, label: &str, flag: Option<&str>) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 26.0), egui::Sense::click());
    let painter = ui.painter();
    painter.text(rect.left_center(), egui::Align2::LEFT_CENTER, label, egui::FontId::proportional(13.0), TEXT);
    if let Some(flag) = flag {
        painter.text(rect.right_center(), egui::Align2::RIGHT_CENTER, flag, egui::FontId::proportional(12.0), WARN);
    }
    response
}

fn labeled_slider(ui: &mut egui::Ui, label: &str, value: &mut f32, range: std::ops::RangeInclusive<f32>, integer: bool) -> bool {
    let mut slider = egui::Slider::new(value, range).text(label);
    if integer {
        slider = slider.integer();
    }
    ui.add(slider).changed()
}

fn action_button(ui: &mut egui::Ui, text: &str, kind: Action) -> egui::Response {
    let (fill, color, stroke) = match kind {
        Action::Primary => (egui::Color32::from_rgb(0xf2, 0xf2, 0xf2), INK, egui::Stroke::NONE),
        Action::Quiet => (egui::Color32::TRANSPARENT, TEXT, egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(36))),
        Action::Danger => (REC, egui::Color32::WHITE, egui::Stroke::NONE),
    };
    ui.add(
        egui::Button::new(egui::RichText::new(text).color(color).strong())
            .fill(fill)
            .stroke(stroke)
            .corner_radius(9.0)
            .min_size(egui::vec2(ui.available_width(), 34.0)),
    )
}

fn paint_badge(ui: &egui::Ui, anchor: egui::Pos2, align: egui::Align2, text: &str, color: egui::Color32) {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), egui::FontId::proportional(11.0), color);
    let pad = egui::vec2(8.0, 4.0);
    let rect = align.anchor_size(anchor, galley.size() + pad * 2.0);
    ui.painter().rect_filled(rect, 10.0, egui::Color32::from_black_alpha(150));
    ui.painter().galley(rect.min + pad, galley, color);
}

fn color_adjusted(color: &ColorParams) -> bool {
    color.brightness.abs() > 0.001 || color.contrast.abs() > 0.001 || color.saturation.abs() > 0.001 || color.temperature.abs() > 0.001
}

fn status_color(status: &str) -> egui::Color32 {
    if status.starts_with("En vivo") {
        LIVE
    } else if status == "Conectando" {
        INFO
    } else if status == "Ocupado" || status == "Teléfono no autorizado" {
        WARN
    } else if status == "Versión incompatible" {
        DANGER
    } else {
        TEXT
    }
}

fn notice_color(text: &str) -> egui::Color32 {
    let lower = text.to_lowercase();
    if lower.contains("incompatible") || lower.contains("error") || lower.contains("no hay") {
        DANGER
    } else if lower.contains("último cuadro") || lower.contains("ultimo cuadro") {
        WARN
    } else if lower.contains("sin cámara") {
        INFO
    } else if lower.contains("guardad") || lower.contains("grabando") {
        LIVE
    } else if lower.contains("falta")
        || lower.contains("no es válida")
        || lower.contains("ocup")
        || lower.contains("autoriz")
        || lower.contains("nvidia")
        || lower.contains("runtime")
        || lower.contains("gpu")
    {
        WARN
    } else {
        INFO
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
