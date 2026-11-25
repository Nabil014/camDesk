//! Hilo de video. Es dueño de la sesión con el teléfono, del decoder, de los
//! efectos y del escritor del archivo mapeado. La cámara virtual no depende de
//! que egui redibuje, y el decode no compite con la UI. La UI manda órdenes y
//! recibe estados y el último cuadro para la vista previa.
//!
//! Soltar la sesión espera al hilo de `link`, que puede estar corriendo adb o
//! leyendo el handshake. Eso pasa acá, nunca en el hilo de la UI. Como las
//! órdenes se atienden en orden, el `forward --remove` de una sesión vieja
//! termina antes de que arranque la siguiente.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui;
use fx::{BackgroundBlur, ColorCorrector, ColorParams};
use link::{Event, Session, SessionConfig, Status};
use protocol::Rotation;

/// Pedir un keyframe más seguido no lo trae antes: el teléfono codifica un IDR por pedido.
const KEYFRAME_GAP: Duration = Duration::from_millis(500);

pub enum Command {
    /// Lo que la UI pudo crear al conectar. Un `None` no borra lo que ya hay.
    Pipeline { decoder: Option<decode::Decoder>, writer: Option<vcam_shm::Writer> },
    Start(SessionConfig),
    Stop,
    Rotation(Option<Rotation>),
    Mirror(bool),
    Color(ColorParams),
    Blur { on: bool, strength: f32 },
    Quit,
}

pub enum Report {
    Status(Status),
    /// `Session::start` no pudo crear el hilo de conexión.
    StartFailed(String),
    BlurReady,
    /// `BackgroundBlur::new` falló: el efecto no llegó a prenderse.
    BlurFailed(String),
    /// El efecto falló con la sesión en marcha y se apagó.
    BlurStopped(String),
    /// Aviso del efecto que no lo apaga, por ejemplo el modo rendimiento.
    BlurNotice(String),
    DecodeError(String),
}

pub struct Video {
    commands: Sender<Command>,
    reports: Receiver<Report>,
    preview: Arc<Mutex<Option<egui::ColorImage>>>,
    join: Option<JoinHandle<()>>,
}

impl Video {
    pub fn spawn(ctx: egui::Context) -> std::io::Result<Self> {
        let (commands, command_rx) = mpsc::channel();
        let (report_tx, reports) = mpsc::channel();
        let preview = Arc::new(Mutex::new(None));
        let slot = preview.clone();
        let join = std::thread::Builder::new()
            .name("camdesk-video".into())
            .spawn(move || Worker::new(ctx, report_tx, slot).run(command_rx))?;
        Ok(Self { commands, reports, preview, join: Some(join) })
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    pub fn try_report(&self) -> Option<Report> {
        self.reports.try_recv().ok()
    }

    /// El último cuadro, ya rotado y con efectos. Los anteriores que la UI no llegó a pintar se pierden: solo son vista previa.
    pub fn take_preview(&self) -> Option<egui::ColorImage> {
        self.preview.lock().unwrap_or_else(|poison| poison.into_inner()).take()
    }
}

impl Drop for Video {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Quit);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

struct Worker {
    ctx: egui::Context,
    reports: Sender<Report>,
    preview: Arc<Mutex<Option<egui::ColorImage>>>,
    session: Option<Session>,
    events: Option<Receiver<Event>>,
    decoder: Option<decode::Decoder>,
    writer: Option<vcam_shm::Writer>,
    color: ColorCorrector,
    rotation: Option<Rotation>,
    mirror: bool,
    blur: Option<BackgroundBlur>,
    blur_notice: Option<String>,
    last_keyframe_request: Option<Instant>,
    rgba: Vec<u8>,
    canvas_y: Vec<u8>,
    canvas_uv: Vec<u8>,
}

impl Worker {
    fn new(ctx: egui::Context, reports: Sender<Report>, preview: Arc<Mutex<Option<egui::ColorImage>>>) -> Self {
        Self {
            ctx,
            reports,
            preview,
            session: None,
            events: None,
            decoder: None,
            writer: None,
            color: ColorCorrector::new(),
            rotation: None,
            mirror: false,
            blur: None,
            blur_notice: None,
            last_keyframe_request: None,
            rgba: Vec::new(),
            canvas_y: Vec::new(),
            canvas_uv: Vec::new(),
        }
    }

    fn run(mut self, commands: Receiver<Command>) {
        loop {
            // Sin sesión no hay nada que leer: se espera la próxima orden.
            let first = if self.events.is_some() {
                match commands.try_recv() {
                    Ok(command) => Some(command),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => break,
                }
            } else {
                match commands.recv_timeout(Duration::from_millis(250)) {
                    Ok(command) => Some(command),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            };
            let mut quit = false;
            let mut next = first;
            while let Some(command) = next {
                if !self.handle(command) {
                    quit = true;
                    break;
                }
                next = commands.try_recv().ok();
            }
            if quit {
                break;
            }
            self.pump();
        }
        // La sesión se suelta antes que el escritor: el último cuadro queda en el archivo.
        self.session = None;
        self.events = None;
    }

    fn handle(&mut self, command: Command) -> bool {
        match command {
            Command::Pipeline { decoder, writer } => {
                if decoder.is_some() {
                    self.decoder = decoder;
                }
                if writer.is_some() {
                    self.writer = writer;
                }
            }
            Command::Start(cfg) => {
                self.close_session();
                match Session::start(cfg) {
                    Ok((session, events)) => {
                        self.session = Some(session);
                        self.events = Some(events);
                    }
                    Err(error) => self.report(Report::StartFailed(error.to_string())),
                }
            }
            Command::Stop => self.close_session(),
            Command::Rotation(rotation) => self.rotation = rotation,
            Command::Mirror(on) => self.mirror = on,
            Command::Color(params) => self.color.set(params),
            Command::Blur { on: false, .. } => {
                self.blur = None;
                self.blur_notice = None;
            }
            Command::Blur { on: true, strength } => match self.blur.as_mut() {
                Some(fx) => fx.set_strength(strength),
                None => match BackgroundBlur::new() {
                    Ok(mut fx) => {
                        fx.set_strength(strength);
                        self.blur = Some(fx);
                        self.blur_notice = None;
                        self.report(Report::BlurReady);
                    }
                    Err(error) => self.report(Report::BlurFailed(error.message)),
                },
            },
            Command::Quit => return false,
        }
        true
    }

    fn close_session(&mut self) {
        self.session = None;
        self.events = None;
        self.last_keyframe_request = None;
        if let Some(decoder) = self.decoder.as_mut() {
            decoder.reset();
        }
    }

    /// Lee lo que llegó del teléfono. Decodifica todos los frames, porque H.264
    /// no admite saltear uno, pero los efectos y el lienzo van solo sobre el último.
    fn pump(&mut self) {
        let Some(events) = self.events.as_ref() else { return };
        let mut batch = Vec::new();
        match events.recv_timeout(Duration::from_millis(10)) {
            Ok(event) => batch.push(event),
            Err(RecvTimeoutError::Timeout) => return,
            Err(RecvTimeoutError::Disconnected) => {
                // El hilo de link terminó solo (Ocupado, Incompatible): ya avisó el estado.
                self.close_session();
                return;
            }
        }
        while let Ok(event) = events.try_recv() {
            batch.push(event);
        }
        let mut latest = None;
        let mut session_over = false;
        for event in batch {
            match event {
                Event::Status(status) => {
                    if matches!(status, Status::Conectando) {
                        if let Some(decoder) = self.decoder.as_mut() {
                            decoder.reset();
                        }
                    }
                    session_over |= matches!(status, Status::Ocupado | Status::Incompatible);
                    self.report(Report::Status(status));
                }
                Event::Frame(frame) => {
                    if let Some(image) = self.decode(frame) {
                        latest = Some(image);
                    }
                }
            }
        }
        if let Some(image) = latest {
            self.publish(image);
        }
        if session_over {
            self.close_session();
        }
    }

    fn decode(&mut self, mut frame: protocol::Frame) -> Option<decode::Decoded> {
        let decoder = self.decoder.as_mut()?;
        if let Some(rotation) = self.rotation {
            frame.header.rotation = rotation;
        }
        match decoder.push(&frame) {
            Ok(image) => image,
            Err(decode::DecodeError::NeedKeyframe) => {
                let due = self.last_keyframe_request.map_or(true, |at| at.elapsed() >= KEYFRAME_GAP);
                if due {
                    if let Some(session) = &self.session {
                        session.request_keyframe();
                        self.last_keyframe_request = Some(Instant::now());
                    }
                }
                None
            }
            Err(decode::DecodeError::Message(text)) => {
                self.report(Report::DecodeError(text));
                None
            }
        }
    }

    /// Rotación (ya hecha por decode) → desenfoque → color → espejo → vista previa y lienzo.
    fn publish(&mut self, mut decoded: decode::Decoded) {
        let width = decoded.width as usize;
        let height = decoded.height as usize;
        let stride = width.saturating_mul(4);
        if let Some(fx) = self.blur.as_mut() {
            match fx.apply(&mut decoded.bgra, width, height, stride) {
                Err(error) => {
                    self.blur = None;
                    self.blur_notice = None;
                    self.report(Report::BlurStopped(error.message));
                }
                Ok(()) => {
                    let notice = fx.notice().map(str::to_string);
                    if notice.is_some() && notice != self.blur_notice {
                        self.blur_notice = notice.clone();
                        self.report(Report::BlurNotice(notice.unwrap_or_default()));
                    }
                }
            }
        }
        self.color.apply(&mut decoded.bgra, width, height, stride);
        // Al final: el modelo del desenfoque ve la imagen tal como salió de la cámara.
        if self.mirror {
            mirror_bgra(&mut decoded.bgra, width);
        }
        self.write_canvas(&decoded);
        self.store_preview(&decoded);
    }

    fn write_canvas(&mut self, decoded: &decode::Decoded) {
        let (Some(decoder), Some(writer)) = (self.decoder.as_mut(), self.writer.as_mut()) else { return };
        self.canvas_y.resize(vcam_shm::Y_BYTES, 16);
        self.canvas_uv.resize(vcam_shm::UV_BYTES, 128);
        decoder.to_canvas_nv12(decoded, &mut self.canvas_y, &mut self.canvas_uv, vcam_shm::STRIDE as usize);
        let (y, uv) = (&self.canvas_y, &self.canvas_uv);
        let _ = writer.write(decoded.pts_us.saturating_mul(10), |dst_y, dst_uv| {
            if dst_y.len() == y.len() && dst_uv.len() == uv.len() {
                dst_y.copy_from_slice(y);
                dst_uv.copy_from_slice(uv);
            }
        });
    }

    fn store_preview(&mut self, decoded: &decode::Decoded) {
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
        *self.preview.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(image);
        self.ctx.request_repaint();
    }

    fn report(&self, report: Report) {
        let _ = self.reports.send(report);
        self.ctx.request_repaint();
    }
}

/// Invierte izquierda y derecha, fila por fila, sin copiar el cuadro.
fn mirror_bgra(bgra: &mut [u8], width: usize) {
    if width == 0 {
        return;
    }
    for row in bgra.chunks_exact_mut(width * 4) {
        for x in 0..width / 2 {
            for channel in 0..4 {
                row.swap(x * 4 + channel, (width - 1 - x) * 4 + channel);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_espejo_invierte_cada_fila_sin_mezclar_canales() {
        // 3x2: cada píxel lleva su número en los cuatro canales, más el canal.
        let pixel = |id: u8| [id * 10, id * 10 + 1, id * 10 + 2, id * 10 + 3];
        let image = |ids: [u8; 6]| ids.iter().flat_map(|id| pixel(*id)).collect::<Vec<u8>>();
        let mut frame = image([1, 2, 3, 4, 5, 6]);
        mirror_bgra(&mut frame, 3);
        assert_eq!(frame, image([3, 2, 1, 6, 5, 4]));
        mirror_bgra(&mut frame, 3);
        assert_eq!(frame, image([1, 2, 3, 4, 5, 6]), "dos veces deja todo igual");
        mirror_bgra(&mut frame, 0);
        assert_eq!(frame, image([1, 2, 3, 4, 5, 6]));
    }
}
