//! Ventana de Camdesk. Al abrir no carga FFmpeg, no carga el runtime de NVIDIA,
//! no registra la cámara virtual, no crea el archivo mapeado, no corre adb,
//! no abre WASAPI y no captura la pantalla.

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use eframe::egui;
use fx::{BackgroundBlur, BlurAvailability, ColorCorrector, ColorParams, blur_availability};
use link::{Camera, Event, Mode, Resolution, Session, SessionConfig, Status};
use record::{MicId, MonitorId, RecordConfig, Recording, default_output, microphones, monitors};

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1080.0, 720.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Camdesk",
        options,
        Box::new(|_cc| Ok(Box::new(CamdeskApp::load()))),
    )
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
    _blur_fx: Option<BackgroundBlur>,
    system_audio: bool,
    mic_on: bool,
    monitors: Vec<record::MonitorInfo>,
    mics: Vec<record::MicInfo>,
    monitor: Option<MonitorId>,
    monitor_size: Option<(u32, u32)>,
    mic: Option<MicId>,
    status: String,
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

    fn ui(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        self.poll(ctx);
        ui.heading("Camdesk");
        ui.label(format!("Estado: {}", self.status));
        if !self.notice.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(220, 160, 60), &self.notice);
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                self.connection(ui);
                ui.separator();
                self.effects(ui);
                ui.separator();
                self.record_ui(ui);
            });
            ui.vertical(|ui| self.preview(ui));
        });
        if self.dirty {
            self.write_config();
            self.dirty = false;
        }
    }

    fn connection(&mut self, ui: &mut egui::Ui) {
        ui.label("Conexión");
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("modo")
                .selected_text(match self.mode {
                    LinkMode::Auto => "Automática",
                    LinkMode::Usb => "Solo USB",
                    LinkMode::Wifi => "Solo Wi-Fi",
                })
                .show_ui(ui, |ui| {
                    self.dirty |= ui.selectable_value(&mut self.mode, LinkMode::Auto, "Automática").changed();
                    self.dirty |= ui.selectable_value(&mut self.mode, LinkMode::Usb, "Solo USB").changed();
                    self.dirty |= ui.selectable_value(&mut self.mode, LinkMode::Wifi, "Solo Wi-Fi").changed();
                });
            let ip = ui.add(egui::TextEdit::singleline(&mut self.ip).hint_text("IP del teléfono").desired_width(140.0));
            self.dirty |= ip.changed();
        });
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("resolucion")
                .selected_text(match self.resolution {
                    Resolution::P720 => "1280x720",
                    Resolution::P1080 => "1920x1080",
                })
                .show_ui(ui, |ui| {
                    self.dirty |= ui.selectable_value(&mut self.resolution, Resolution::P720, "1280x720").changed();
                    self.dirty |= ui.selectable_value(&mut self.resolution, Resolution::P1080, "1920x1080").changed();
                });
            egui::ComboBox::from_id_salt("camara")
                .selected_text(match self.camera {
                    Camera::Back => "Trasera",
                    Camera::Front => "Frontal",
                })
                .show_ui(ui, |ui| {
                    self.dirty |= ui.selectable_value(&mut self.camera, Camera::Back, "Trasera").changed();
                    self.dirty |= ui.selectable_value(&mut self.camera, Camera::Front, "Frontal").changed();
                });
        });
        ui.horizontal(|ui| {
            if self.session.is_none() {
                if ui.button("Conectar").clicked() {
                    self.connect();
                }
            } else if ui.button("Desconectar").clicked() {
                self.session = None;
                self.events = None;
                if let Some(decoder) = self.decoder.as_mut() {
                    decoder.reset();
                }
                self.status = "Desconectado".into();
            }
        });
    }

    fn effects(&mut self, ui: &mut egui::Ui) {
        if self.blur.is_none() {
            self.blur = Some(blur_availability());
            self.blur_note = match self.blur {
                Some(BlurAvailability::Available) => String::new(),
                Some(BlurAvailability::NoNvidiaGpu) => "sin GPU NVIDIA".into(),
                Some(BlurAvailability::NoRuntime) | None => "sin el runtime de NVIDIA Video Effects".into(),
            };
        }
        ui.label("Efectos");
        let available = matches!(self.blur, Some(BlurAvailability::Available));
        ui.add_enabled_ui(available, |ui| {
            if ui.checkbox(&mut self.blur_on, "Desenfocar fondo").changed() {
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
            if ui.add(egui::Slider::new(&mut self.blur_strength, 0.0..=1.0).text("Fuerza")).changed() {
                self.dirty = true;
                if let Some(fx) = self._blur_fx.as_mut() {
                    fx.set_strength(self.blur_strength);
                }
            }
        });
        if !self.blur_note.is_empty() {
            ui.label(&self.blur_note);
        }
        if !available {
            ui.hyperlink_to(
                "Descargar el runtime",
                "https://www.nvidia.com/en-us/geforce/broadcasting/broadcast-sdk/resources/",
            );
        }
        let mut changed = false;
        changed |= ui.add(egui::Slider::new(&mut self.color.brightness, -1.0..=1.0).text("Brillo")).changed();
        changed |= ui.add(egui::Slider::new(&mut self.color.contrast, -1.0..=1.0).text("Contraste")).changed();
        changed |= ui.add(egui::Slider::new(&mut self.color.saturation, -1.0..=1.0).text("Saturación")).changed();
        changed |= ui.add(egui::Slider::new(&mut self.color.temperature, -1.0..=1.0).text("Temperatura")).changed();
        if changed {
            self.dirty = true;
        }
        if ui.button("Restablecer").clicked() {
            self.color = ColorParams::default();
            self.dirty = true;
        }
    }

    fn record_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Grabación");
        self.ensure_monitors();
        let monitor_label = self
            .monitors
            .iter()
            .find(|item| Some(&item.id) == self.monitor.as_ref())
            .map(|item| monitor_text(item))
            .unwrap_or_else(|| "Elegir monitor".into());
        egui::ComboBox::from_id_salt("monitor")
            .width(220.0)
            .selected_text(monitor_label)
            .show_ui(ui, |ui| {
                for item in &self.monitors {
                    let selected = self.monitor.as_ref() == Some(&item.id);
                    if ui.selectable_label(selected, monitor_text(item)).clicked() {
                        self.monitor = Some(item.id.clone());
                        self.monitor_size = Some((item.width, item.height));
                        self.dirty = true;
                    }
                }
            });
        self.dirty |= ui.checkbox(&mut self.system_audio, "Audio del sistema").changed();
        self.dirty |= ui.checkbox(&mut self.mic_on, "Micrófono").changed();
        let mic_label = self
            .mics
            .iter()
            .find(|item| Some(&item.id) == self.mic.as_ref())
            .map(|item| item.name.clone())
            .unwrap_or_else(|| "Elegir micrófono".into());
        ui.add_enabled_ui(self.mic_on, |ui| {
            egui::ComboBox::from_id_salt("microfono")
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
        });
        ui.horizontal(|ui| {
            if self.recording.is_none() {
                if ui.button("Grabar").clicked() {
                    self.start_recording();
                }
            } else {
                let elapsed = self.recording.as_ref().map(|rec| rec.elapsed()).unwrap_or_default();
                ui.label(format_elapsed(elapsed));
                if ui.button("Detener").clicked() {
                    self.stop_recording();
                }
            }
        });
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
                self.notice = warning.unwrap_or_default();
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
                    if let Status::EnVivo { camera, .. } = status {
                        if camera != self.camera {
                            self.camera = camera;
                            self.dirty = true;
                            self.notice = match camera {
                                Camera::Back => "El teléfono se quedó en la cámara trasera".into(),
                                Camera::Front => "El teléfono se quedó en la cámara frontal".into(),
                            };
                        }
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

    fn preview(&self, ui: &mut egui::Ui) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(640.0, 360.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 4.0, egui::Color32::from_rgb(18, 18, 18));
        let Some(texture) = &self.preview else {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "La imagen aparece al conectar",
                egui::FontId::proportional(16.0),
                egui::Color32::from_rgb(160, 160, 160),
            );
            return;
        };
        let size = texture.size_vec2();
        if size.x <= 0.0 || size.y <= 0.0 {
            return;
        }
        let scale = (rect.width() / size.x).min(rect.height() / size.y);
        let fitted = egui::Rect::from_center_size(rect.center(), egui::vec2(size.x * scale, size.y * scale));
        egui::Image::from_texture(texture).paint_at(ui, fitted);
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
                self.notice = format!("Grabando con {} en {}", recording.encoder(), output.display());
                self.recording = Some(recording);
            }
            Err(error) => self.notice = error.message,
        }
    }

    fn stop_recording(&mut self) {
        if let Some(recording) = self.recording.take() {
            match recording.stop() {
                Ok(path) => self.notice = format!("Guardado en {}", path.display()),
                Err(error) => self.notice = error.message,
            }
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
        egui::CentralPanel::default().show(ctx, |ui| self.ui(ctx, ui));
        if self.session.is_some() {
            ctx.request_repaint_after(Duration::from_millis(16));
        } else if self.recording.is_some() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
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
        Status::EnVivo { width, height, .. } => format!("En vivo {width}x{height}"),
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
