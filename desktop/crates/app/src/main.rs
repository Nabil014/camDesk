//! Ventana de Camdesk. Al abrir no carga FFmpeg, no registra la cámara virtual,
//! no crea el archivo mapeado, no corre adb, no abre WASAPI y no captura la pantalla.

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use eframe::egui;
use fx::{BackgroundBlur, BlurAvailability, ColorParams, blur_availability};
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
    mic: Option<MicId>,
    status: String,
    notice: String,
    session: Option<Session>,
    events: Option<Receiver<Event>>,
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
            mic: None,
            status: "Desconectado".into(),
            notice: String::new(),
            session: None,
            events: None,
            recording: None,
            dirty: false,
        };
        app.read_config();
        app
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        self.poll();
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
            ui.vertical(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(640.0, 360.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 4.0, egui::Color32::from_rgb(18, 18, 18));
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "La imagen aparece al conectar",
                    egui::FontId::proportional(16.0),
                    egui::Color32::from_rgb(160, 160, 160),
                );
            });
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
        let monitor_label = self
            .monitors
            .iter()
            .find(|item| Some(&item.id) == self.monitor.as_ref())
            .map(|item| item.name.clone())
            .unwrap_or_else(|| "Elegir monitor".into());
        egui::ComboBox::from_id_salt("monitor")
            .selected_text(monitor_label)
            .show_ui(ui, |ui| {
                if self.monitors.is_empty() {
                    self.monitors = monitors();
                    if self.monitor.is_none() {
                        self.monitor = self.monitors.first().map(|item| item.id.clone());
                    }
                }
                for item in &self.monitors {
                    let selected = self.monitor.as_ref() == Some(&item.id);
                    if ui.selectable_label(selected, &item.name).clicked() {
                        self.monitor = Some(item.id.clone());
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
                self.notice.clear();
            }
            Err(error) => self.notice = error.to_string(),
        }
    }

    fn poll(&mut self) {
        let Some(events) = self.events.as_ref() else { return };
        while let Ok(event) = events.try_recv() {
            if let Event::Status(status) = event {
                self.status = status_text(status);
            }
        }
    }

    fn start_recording(&mut self) {
        if self.monitors.is_empty() {
            self.monitors = monitors();
        }
        let Some(monitor) = self.monitor.clone().or_else(|| self.monitors.first().map(|item| item.id.clone())) else {
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
            "mode={mode}\nip={}\nresolution={}\ncamera={}\nbrightness={}\ncontrast={}\nsaturation={}\ntemperature={}\nblur={}\nsystem_audio={}\nmic={}\n",
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
        );
        let _ = std::fs::write(path, text);
    }
}

impl eframe::App for CamdeskApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| self.ui(ui));
        if self.session.is_some() || self.recording.is_some() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }
}

fn status_text(status: Status) -> String {
    match status {
        Status::Conectando => "Conectando".into(),
        Status::EnVivo { width, height } => format!("En vivo {width}x{height}"),
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
