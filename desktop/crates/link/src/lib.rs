//! TCP, adb y reintento. No decodifica ni pinta.
//! `adb` corre en el hilo de la sesión, nunca en el de la UI.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use protocol::{Frame, FrameParser, HandshakeRequest, HandshakeResponse, ProtocolError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Auto,
    Usb,
    Wifi(Ipv4Addr),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    P720,
    P1080,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Camera {
    Back,
    Front,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionConfig {
    pub mode: Mode,
    pub resolution: Resolution,
    pub camera: Camera,
    /// IP de respaldo cuando el modo es automático y no hay un teléfono por USB.
    pub wifi: Option<Ipv4Addr>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Conectando,
    EnVivo { width: u32, height: u32, camera: Camera },
    Desconectado,
    Ocupado,
    NoAutorizado,
    Incompatible,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Status(Status),
    Frame(Frame),
}

pub struct Session {
    stop: Arc<AtomicBool>,
    keyframe: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl Session {
    pub fn start(cfg: SessionConfig) -> std::io::Result<(Self, Receiver<Event>)> {
        start_at(cfg, None)
    }

    pub fn request_keyframe(&self) {
        self.keyframe.store(true, Ordering::Release);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

pub(crate) fn start_at(cfg: SessionConfig, port: Option<u16>) -> std::io::Result<(Session, Receiver<Event>)> {
    let (tx, rx) = mpsc::sync_channel(64);
    let stop = Arc::new(AtomicBool::new(false));
    let keyframe = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let key_thread = keyframe.clone();
    let join = std::thread::Builder::new()
        .name("camdesk-link".into())
        .spawn(move || drive(cfg, port.unwrap_or(protocol::PORT), stop_thread, key_thread, tx))?;
    Ok((Session { stop, keyframe, join: Some(join) }, rx))
}

fn drive(cfg: SessionConfig, port: u16, stop: Arc<AtomicBool>, keyframe: Arc<AtomicBool>, tx: SyncSender<Event>) {
    while !stop.load(Ordering::Acquire) {
        if tx.send(Event::Status(Status::Conectando)).is_err() {
            break;
        }
        let status = match attempt(&cfg, port, &stop, &keyframe, &tx) {
            Attempt::Stop => break,
            Attempt::Again(status) => status,
        };
        if tx.send(Event::Status(status)).is_err() || !sleep_interruptible(&stop, Duration::from_secs(2)) {
            break;
        }
    }
}

enum Attempt {
    Stop,
    Again(Status),
}

fn attempt(
    cfg: &SessionConfig,
    port: u16,
    stop: &AtomicBool,
    keyframe: &AtomicBool,
    tx: &SyncSender<Event>,
) -> Attempt {
    let target = match resolve_target(cfg, port) {
        Target::Again(status) => return Attempt::Again(status),
        Target::Ready { addr, forward } => (addr, forward),
    };
    let (addr, forward) = target;
    let stream = match TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
        Ok(stream) => stream,
        Err(_) => {
            clear_forward(&forward);
            return Attempt::Again(Status::Desconectado);
        }
    };
    if stop.load(Ordering::Acquire) {
        clear_forward(&forward);
        return Attempt::Stop;
    }
    let _ = stream.set_nodelay(true);
    let outcome = talk(cfg, stream, stop, keyframe, tx);
    clear_forward(&forward);
    outcome
}

enum Target {
    Again(Status),
    Ready { addr: SocketAddr, forward: Option<Forward> },
}

struct Forward {
    bin: PathBuf,
    serial: String,
}

fn resolve_target(cfg: &SessionConfig, port: u16) -> Target {
    match cfg.mode {
        Mode::Wifi(ip) => Target::Ready { addr: SocketAddr::from((ip, port)), forward: None },
        Mode::Usb => match usb_target(port) {
            Usb::Ready(ready) => ready,
            Usb::Status(status) => Target::Again(status),
        },
        Mode::Auto => match usb_target(port) {
            Usb::Ready(ready) => ready,
            Usb::Status(Status::NoAutorizado) => Target::Again(Status::NoAutorizado),
            Usb::Status(_) => match cfg.wifi {
                Some(ip) => Target::Ready { addr: SocketAddr::from((ip, port)), forward: None },
                None => Target::Again(Status::Desconectado),
            },
        },
    }
}

enum Usb {
    Ready(Target),
    Status(Status),
}

fn usb_target(port: u16) -> Usb {
    let Some(bin) = find_adb() else {
        return Usb::Status(Status::Desconectado);
    };
    let output = match Command::new(&bin).arg("devices").output() {
        Ok(output) => String::from_utf8_lossy(&output.stdout).into_owned(),
        Err(_) => return Usb::Status(Status::Desconectado),
    };
    match pick_device(&output) {
        DevicePick::Unauthorized => Usb::Status(Status::NoAutorizado),
        DevicePick::None => Usb::Status(Status::Desconectado),
        DevicePick::Ready(serial) => {
            let listed = Command::new(&bin)
                .args(["-s", &serial, "forward", &format!("tcp:{port}"), &format!("tcp:{port}")])
                .output();
            if listed.map(|out| out.status.success()).unwrap_or(false) {
                Usb::Ready(Target::Ready {
                    addr: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
                    forward: Some(Forward { bin, serial }),
                })
            } else {
                Usb::Status(Status::Desconectado)
            }
        }
    }
}

fn clear_forward(forward: &Option<Forward>) {
    let Some(forward) = forward else {
        return;
    };
    let args = remove_forward_args(&forward.serial);
    let _ = Command::new(&forward.bin).args(&args).output();
}

pub(crate) fn remove_forward_args(serial: &str) -> Vec<String> {
    ["-s", serial, "forward", "--remove", "tcp:4747"]
        .into_iter()
        .map(str::to_string)
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DevicePick {
    Ready(String),
    Unauthorized,
    None,
}

pub(crate) fn pick_device(text: &str) -> DevicePick {
    let mut unauthorized = false;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(serial) = parts.next() else { continue };
        match parts.next() {
            Some("device") => return DevicePick::Ready(serial.to_string()),
            Some("unauthorized") => unauthorized = true,
            _ => {}
        }
    }
    if unauthorized {
        DevicePick::Unauthorized
    } else {
        DevicePick::None
    }
}

fn find_adb() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let name = if cfg!(windows) { "adb.exe" } else { "adb" };
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    #[cfg(windows)]
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let candidate = Path::new(&local).join("Android").join("Sdk").join("platform-tools").join("adb.exe");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn talk(
    cfg: &SessionConfig,
    mut stream: TcpStream,
    stop: &AtomicBool,
    keyframe: &AtomicBool,
    tx: &SyncSender<Event>,
) -> Attempt {
    let (width, height) = match cfg.resolution {
        Resolution::P720 => (1280, 720),
        Resolution::P1080 => (1920, 1080),
    };
    let request = HandshakeRequest::video(width, height, 30, cfg.camera == Camera::Front);
    if stream.write_all(&request.to_bytes()).is_err() {
        return Attempt::Again(Status::Desconectado);
    }
    let mut response = [0u8; 16];
    if read_exact(&mut stream, &mut response, Duration::from_secs(5), stop).is_err() {
        return Attempt::Again(Status::Desconectado);
    }
    let parsed = match HandshakeResponse::parse(&response) {
        Ok(parsed) => parsed,
        Err(_) => return Attempt::Again(Status::Desconectado),
    };
    let status = match parsed.status {
        protocol::HandshakeStatus::Ok | protocol::HandshakeStatus::Adjusted => {
            let camera = if parsed.camera == 1 { Camera::Front } else { Camera::Back };
            Status::EnVivo { width: parsed.width, height: parsed.height, camera }
        }
        protocol::HandshakeStatus::Version | protocol::HandshakeStatus::Codec => Status::Incompatible,
        protocol::HandshakeStatus::Camera => Status::Ocupado,
        protocol::HandshakeStatus::Other(_) => Status::Desconectado,
    };
    if tx.send(Event::Status(status.clone())).is_err() {
        return Attempt::Stop;
    }
    if !matches!(status, Status::EnVivo { .. }) {
        return Attempt::Again(status);
    }
    let _ = stream.set_read_timeout(Some(Duration::from_millis(400)));
    let mut parser = FrameParser::new();
    let mut buf = [0u8; 8192];
    let mut skip_until_key = false;
    // Si el Wi-Fi se corta, el socket no siempre se cierra: las lecturas solo
    // vencen. Sin bytes durante este rato el enlace está muerto y se reintenta.
    // El último cuadro queda en quien ya lo tenía.
    let silence = Duration::from_secs(2);
    let mut last_byte = Instant::now();
    loop {
        if stop.load(Ordering::Acquire) {
            return Attempt::Stop;
        }
        if keyframe.swap(false, Ordering::AcqRel) {
            if stream.write_all(&protocol::request_keyframe()).is_err() {
                return Attempt::Again(Status::Desconectado);
            }
        }
        match stream.read(&mut buf) {
            Ok(0) => return Attempt::Again(Status::Desconectado),
            Ok(n) => {
                last_byte = Instant::now();
                parser.push(&buf[..n]);
            }
            Err(err) if err.kind() == std::io::ErrorKind::TimedOut || err.kind() == std::io::ErrorKind::WouldBlock => {
                if last_byte.elapsed() >= silence {
                    return Attempt::Again(Status::Desconectado);
                }
            }
            Err(_) => return Attempt::Again(Status::Desconectado),
        }
        while let Some(item) = parser.next() {
            match item {
                Err(ProtocolError::Desync) => {
                    skip_until_key = true;
                    keyframe.store(true, Ordering::Release);
                }
                Err(ProtocolError::BadMagic) => skip_until_key = true,
                Ok(frame) => {
                    if skip_until_key && !frame.header.keyframe {
                        continue;
                    }
                    skip_until_key = false;
                    match tx.try_send(Event::Frame(frame)) {
                        Ok(()) => {}
                        Err(mpsc::TrySendError::Disconnected(_)) => return Attempt::Stop,
                        Err(mpsc::TrySendError::Full(_)) => {}
                    }
                }
            }
        }
    }
}

fn read_exact(stream: &mut TcpStream, buf: &mut [u8], budget: Duration, stop: &AtomicBool) -> std::io::Result<()> {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let deadline = Instant::now() + budget;
    let mut got = 0;
    while got < buf.len() {
        if stop.load(Ordering::Acquire) {
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "stop"));
        }
        if Instant::now() > deadline {
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "handshake"));
        }
        match stream.read(&mut buf[got..]) {
            Ok(0) => return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "cerrado")),
            Ok(n) => got += n,
            Err(err) if err.kind() == std::io::ErrorKind::TimedOut || err.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

fn sleep_interruptible(stop: &AtomicBool, total: Duration) -> bool {
    let deadline = Instant::now() + total;
    while Instant::now() < deadline {
        if stop.load(Ordering::Acquire) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    fn recv_until(rx: &Receiver<Event>, pred: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = rx.recv_timeout(left).expect("sin evento");
            if pred(&event) {
                return event;
            }
            if Instant::now() > deadline {
                panic!("no llegó el evento esperado");
            }
        }
    }

    fn handshake_ok(width: u16, height: u16) -> [u8; 16] {
        HandshakeResponse {
            status: protocol::HandshakeStatus::Adjusted,
            version: 1,
            codec: 0,
            width: width as u32,
            height: height as u32,
            fps: 30,
            camera: 0,
            flags: protocol::HS_FLAG_REQUEST_KEYFRAME,
        }
        .to_bytes()
    }

    fn video_frame(payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&protocol::MAGIC_FRAME.to_be_bytes());
        bytes.push(0);
        bytes.push(protocol::FRAME_FLAG_KEYFRAME);
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes.extend_from_slice(&10i64.to_be_bytes());
        bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    fn read_handshake(stream: &mut TcpStream) -> [u8; 16] {
        stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut buf = [0u8; 16];
        stream.read_exact(&mut buf).unwrap();
        buf
    }

    struct Server {
        port: u16,
    }

    impl Server {
        fn spawn(handler: impl FnOnce(TcpStream) + Send + 'static) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            thread::spawn(move || {
                if let Ok((stream, _)) = listener.accept() {
                    handler(stream);
                }
            });
            Self { port }
        }
    }

    fn session(port: u16) -> (Session, Receiver<Event>) {
        start_at(
            SessionConfig {
                mode: Mode::Wifi(Ipv4Addr::LOCALHOST),
                resolution: Resolution::P720,
                camera: Camera::Back,
                wifi: None,
            },
            Some(port),
        )
        .unwrap()
    }

    #[test]
    fn handshake_entrega_el_tamano_ajustado_y_el_frame() {
        let server = Server::spawn(|mut stream| {
            let req = read_handshake(&mut stream);
            assert_eq!(req[0..4], [0x43, 0x43, 0x41, 0x4D]);
            stream.write_all(&handshake_ok(640, 480)).unwrap();
            stream.write_all(&video_frame(&[1, 2, 3])).unwrap();
            thread::sleep(Duration::from_millis(200));
        });
        let (_session, rx) = session(server.port);
        let event = recv_until(&rx, |ev| matches!(ev, Event::Status(Status::EnVivo { .. })));
        assert_eq!(
            event,
            Event::Status(Status::EnVivo { width: 640, height: 480, camera: Camera::Back })
        );
        let event = recv_until(&rx, |ev| matches!(ev, Event::Frame(_)));
        let Event::Frame(frame) = event else { unreachable!() };
        assert_eq!(frame.payload, vec![1, 2, 3]);
        assert!(frame.header.keyframe);
    }

    #[test]
    fn la_camara_aplicada_viene_en_el_estado() {
        let server = Server::spawn(|mut stream| {
            let req = read_handshake(&mut stream);
            assert_eq!(req[11], 1, "el pedido tiene que pedir la frontal");
            stream.write_all(&handshake_ok(1280, 720)).unwrap();
        });
        let (_session, rx) = start_at(
            SessionConfig {
                mode: Mode::Wifi(Ipv4Addr::LOCALHOST),
                resolution: Resolution::P720,
                camera: Camera::Front,
                wifi: None,
            },
            Some(server.port),
        )
        .unwrap();
        let event = recv_until(&rx, |ev| matches!(ev, Event::Status(Status::EnVivo { .. })));
        assert_eq!(
            event,
            Event::Status(Status::EnVivo { width: 1280, height: 720, camera: Camera::Back })
        );
    }

    #[test]
    fn silencio_prolongado_desconecta() {
        let server = Server::spawn(|mut stream| {
            let _ = read_handshake(&mut stream);
            stream.write_all(&handshake_ok(1280, 720)).unwrap();
            stream.write_all(&video_frame(&[7])).unwrap();
            thread::sleep(Duration::from_secs(5));
        });
        let (_session, rx) = session(server.port);
        recv_until(&rx, |ev| matches!(ev, Event::Status(Status::EnVivo { .. })));
        recv_until(&rx, |ev| matches!(ev, Event::Frame(_)));
        let started = Instant::now();
        recv_until(&rx, |ev| matches!(ev, Event::Status(Status::Desconectado)));
        assert!(started.elapsed() < Duration::from_secs(3), "no notó el silencio");
    }

    #[test]
    fn corte_a_mitad_de_frame_desconecta() {
        let server = Server::spawn(|mut stream| {
            let _ = read_handshake(&mut stream);
            stream.write_all(&handshake_ok(1280, 720)).unwrap();
            stream.write_all(&protocol::MAGIC_FRAME.to_be_bytes()[..]).unwrap();
            stream.write_all(&[0, 1, 0, 0, 0, 0]).unwrap();
        });
        let (_session, rx) = session(server.port);
        recv_until(&rx, |ev| matches!(ev, Event::Status(Status::EnVivo { .. })));
        recv_until(&rx, |ev| matches!(ev, Event::Status(Status::Desconectado)));
    }

    #[test]
    fn magic_roto_reengancha_el_frame_siguiente() {
        let server = Server::spawn(|mut stream| {
            let _ = read_handshake(&mut stream);
            stream.write_all(&handshake_ok(1280, 720)).unwrap();
            let mut bytes = vec![0, 1, 2, 3, 4, 5];
            bytes.extend(video_frame(&[9, 9]));
            stream.write_all(&bytes).unwrap();
            thread::sleep(Duration::from_millis(200));
        });
        let (_session, rx) = session(server.port);
        let event = recv_until(&rx, |ev| matches!(ev, Event::Frame(frame) if frame.payload.as_slice() == [9, 9]));
        let Event::Frame(frame) = event else { unreachable!() };
        assert_eq!(frame.payload, vec![9, 9]);
    }

    #[test]
    fn frame_de_mas_de_4_mib_no_se_lee_entero() {
        let server = Server::spawn(|mut stream| {
            let _ = read_handshake(&mut stream);
            stream.write_all(&handshake_ok(1280, 720)).unwrap();
            let mut header = [0u8; 20];
            header[0..4].copy_from_slice(&protocol::MAGIC_FRAME.to_be_bytes());
            header[16..20].copy_from_slice(&(protocol::MAX_PAYLOAD + 1).to_be_bytes());
            stream.write_all(&header).unwrap();
            stream.write_all(&video_frame(&[4, 4, 4, 4])).unwrap();
            thread::sleep(Duration::from_millis(200));
        });
        let started = Instant::now();
        let (_session, rx) = session(server.port);
        let event = recv_until(&rx, |ev| matches!(ev, Event::Frame(frame) if frame.payload.as_slice() == [4, 4, 4, 4]));
        assert!(started.elapsed() < Duration::from_secs(2), "leyó un payload enorme");
        let Event::Frame(frame) = event else { unreachable!() };
        assert_eq!(frame.header.len, 4);
    }

    #[test]
    fn rechazo_por_otro_cliente() {
        let server = Server::spawn(|mut stream| {
            let _ = read_handshake(&mut stream);
            let mut bytes = handshake_ok(1280, 720);
            bytes[13] = 4;
            stream.write_all(&bytes).unwrap();
        });
        let (_session, rx) = session(server.port);
        recv_until(&rx, |ev| matches!(ev, Event::Status(Status::Ocupado)));
    }

    #[test]
    fn quitar_el_forward_no_usa_remove_all() {
        let args = remove_forward_args("ABC123");
        assert_eq!(args, ["-s", "ABC123", "forward", "--remove", "tcp:4747"]);
        assert!(!args.iter().any(|arg| arg == "--remove-all"));
    }

    #[test]
    fn elige_el_primer_telefono_autorizado() {
        let text = "List of devices attached\nZZZ\tunauthorized\nABC\tdevice\n";
        assert_eq!(pick_device(text), DevicePick::Ready("ABC".into()));
        assert_eq!(pick_device("List of devices attached\nZZZ\tunauthorized\n"), DevicePick::Unauthorized);
        assert_eq!(pick_device("List of devices attached\n"), DevicePick::None);
    }
}
