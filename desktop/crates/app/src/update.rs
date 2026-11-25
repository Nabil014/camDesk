//! Actualizaciones desde los releases de GitHub.
//!
//! Cada release trae `Camdesk-Setup.exe`, un manifiesto con la versión y el SHA-256 del
//! instalador, y la firma Ed25519 del manifiesto. Solo se instala lo que firmó la clave
//! privada de quien publica: cambiar los archivos del release no alcanza. El manifiesto
//! firmado lleva la versión, así que tampoco se puede ofrecer una más vieja como nueva.
//!
//! La red corre en hilos propios. Nada de esto se ejecuta en el hilo de la UI.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use ed25519_dalek::{Signature, VerifyingKey};
use eframe::egui;
use sha2::{Digest, Sha256};

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

const RELEASES: &str = "https://github.com/Nabil014/camDesk/releases";
const MANIFEST: &str = "camdesk-update.txt";
const SETUP: &str = "Camdesk-Setup.exe";
/// Mitad pública de la clave de publicación. `installer/publish.ps1` no publica si no coincide.
const PUBLIC_KEY: &str = "7dc5867a05fa2cdec4d3ca788cb76fb1ae6f3d075158763f3528e755e64e53c1";
const MAX_SETUP: u64 = 500 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    sha256: [u8; 32],
}

pub enum State {
    /// Sin novedades, o todavía sin consultar. Un fallo al consultar no molesta: queda en el log.
    Quiet,
    Checking(Receiver<Result<Option<Release>, String>>),
    Available(Release),
    Downloading(Release, Receiver<Result<PathBuf, String>>),
    /// Descargado y verificado, a la espera de poder cerrar la app.
    Ready(Release, PathBuf),
    Failed(Release, String),
}

pub struct Updater {
    pub state: State,
    checked: bool,
}

impl Updater {
    pub fn new() -> Self {
        Self { state: State::Quiet, checked: false }
    }

    /// Consulta una sola vez por ejecución. Una compilación de depuración no consulta,
    /// salvo que `CAMDESK_UPDATE_URL` apunte a un servidor de prueba.
    pub fn check_once(&mut self, ctx: &egui::Context) {
        if self.checked {
            return;
        }
        self.checked = true;
        if cfg!(debug_assertions) && std::env::var_os("CAMDESK_UPDATE_URL").is_none() {
            return;
        }
        if let Some(receiver) = background(ctx, || check(&base(), PUBLIC_KEY, CURRENT)) {
            self.state = State::Checking(receiver);
        }
    }

    /// Recoge lo que terminó. Devuelve el error de la consulta o de la descarga para el log.
    pub fn poll(&mut self) -> Option<String> {
        match &self.state {
            State::Checking(receiver) => match receiver.try_recv() {
                Err(TryRecvError::Empty) => None,
                Ok(Ok(Some(release))) => {
                    self.state = State::Available(release);
                    None
                }
                Ok(Ok(None)) => {
                    self.state = State::Quiet;
                    None
                }
                Ok(Err(error)) => {
                    self.state = State::Quiet;
                    Some(format!("buscar actualización: {error}"))
                }
                Err(TryRecvError::Disconnected) => {
                    self.state = State::Quiet;
                    None
                }
            },
            State::Downloading(release, receiver) => {
                let release = release.clone();
                match receiver.try_recv() {
                    Err(TryRecvError::Empty) => None,
                    Ok(Ok(path)) => {
                        self.state = State::Ready(release, path);
                        None
                    }
                    Ok(Err(error)) => {
                        self.state = State::Failed(release, error.clone());
                        Some(format!("descargar actualización: {error}"))
                    }
                    Err(TryRecvError::Disconnected) => {
                        self.state = State::Failed(release, "se cortó la descarga".into());
                        None
                    }
                }
            }
            _ => None,
        }
    }

    pub fn download(&mut self, release: Release, ctx: &egui::Context) {
        let wanted = release.clone();
        match background(ctx, move || download(&base(), &wanted, &downloads_dir())) {
            Some(receiver) => self.state = State::Downloading(release, receiver),
            None => self.state = State::Failed(release, "no se pudo crear el hilo".into()),
        }
    }
}

/// Instala sin preguntas y vuelve a abrir Camdesk. Quien llama cierra la ventana después.
pub fn install(setup: &Path) -> std::io::Result<()> {
    std::process::Command::new(setup)
        .args(["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/RELAUNCH=1"])
        .spawn()
        .map(|_| ())
}

fn background<T: Send + 'static>(
    ctx: &egui::Context,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<Receiver<T>> {
    let (sender, receiver) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::Builder::new()
        .name("camdesk-update".into())
        .spawn(move || {
            let _ = sender.send(work());
            ctx.request_repaint();
        })
        .ok()
        .map(|_| receiver)
}

/// La variable de entorno sirve para probar contra un servidor local. La firma se exige igual.
fn base() -> String {
    std::env::var("CAMDESK_UPDATE_URL").unwrap_or_else(|_| RELEASES.into())
}

fn downloads_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Camdesk")
        .join("updates")
}

fn open(url: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(20)))
        .user_agent("Camdesk")
        .build()
        .into();
    agent.get(url).call().map_err(|error| error.to_string())
}

fn fetch(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    open(url)?
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|error| error.to_string())
}

fn check(base: &str, key: &str, current: &str) -> Result<Option<Release>, String> {
    let manifest = fetch(&format!("{base}/latest/download/{MANIFEST}"), 4096)?;
    let signature = fetch(&format!("{base}/latest/download/{MANIFEST}.sig"), 256)?;
    if !signed(&manifest, &signature, key) {
        return Err("la firma del manifiesto no es válida".into());
    }
    let release = parse_manifest(&String::from_utf8_lossy(&manifest)).ok_or("manifiesto mal formado")?;
    Ok(is_newer(&release.version, current).then_some(release))
}

fn download(base: &str, release: &Release, dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let target = dir.join(format!("Camdesk-Setup-{}.exe", release.version));
    let partial = target.with_extension("part");
    // La versión va en la ruta: si se publica otra mientras tanto, no se mezclan.
    let mut response = open(&format!("{base}/download/v{}/{SETUP}", release.version))?;
    let mut reader = response.body_mut().with_config().limit(MAX_SETUP).reader();
    let mut file = File::create(&partial).map_err(|error| error.to_string())?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
        file.write_all(&buffer[..read]).map_err(|error| error.to_string())?;
    }
    drop(file);
    if hash.finalize()[..] != release.sha256 {
        let _ = std::fs::remove_file(&partial);
        return Err("el instalador descargado no coincide con el publicado".into());
    }
    let _ = std::fs::remove_file(&target);
    std::fs::rename(&partial, &target).map_err(|error| error.to_string())?;
    Ok(target)
}

fn signed(message: &[u8], signature: &[u8], key: &str) -> bool {
    let Some(key) = hex32(key).and_then(|bytes| VerifyingKey::from_bytes(&bytes).ok()) else {
        return false;
    };
    Signature::from_slice(signature).is_ok_and(|signature| key.verify_strict(message, &signature).is_ok())
}

/// `version=1.2.3` y `sha256=<64 hex>`, una clave por línea.
fn parse_manifest(text: &str) -> Option<Release> {
    let mut version = None;
    let mut sha256 = None;
    for line in text.lines() {
        match line.trim().split_once('=') {
            Some(("version", value)) => version = parse_version(value).map(|_| value.to_string()),
            Some(("sha256", value)) => sha256 = hex32(value),
            _ => {}
        }
    }
    Some(Release { version: version?, sha256: sha256? })
}

/// Solo tres números: la versión termina en una ruta y en un nombre de archivo.
fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.split('.').map(|part| {
        (!part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())).then(|| part.parse::<u32>().ok()).flatten()
    });
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

fn is_newer(candidate: &str, current: &str) -> bool {
    matches!((parse_version(candidate), parse_version(current)), (Some(new), Some(old)) if new > old)
}

fn hex32(text: &str) -> Option<[u8; 32]> {
    let text = text.trim();
    if text.len() != 64 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use std::collections::HashMap;
    use std::net::TcpListener;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// Clave fija de prueba. No es la de publicación.
    fn test_key() -> (SigningKey, String) {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let public = hex(key.verifying_key().as_bytes());
        (key, public)
    }

    /// Servidor HTTP mínimo con las rutas de un release.
    fn serve(files: HashMap<String, Vec<u8>>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut request = [0u8; 2048];
                let read = stream.read(&mut request).unwrap_or(0);
                let path = String::from_utf8_lossy(&request[..read]).split_whitespace().nth(1).unwrap_or("").to_string();
                let (status, body) = match files.get(&path) {
                    Some(body) => ("200 OK", body.clone()),
                    None => ("404 Not Found", Vec::new()),
                };
                let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        base
    }

    fn release_files(version: &str, setup: &[u8], signer: &SigningKey) -> HashMap<String, Vec<u8>> {
        let manifest = format!("version={version}\nsha256={}\n", hex(&Sha256::digest(setup))).into_bytes();
        let signature = signer.sign(&manifest).to_bytes().to_vec();
        HashMap::from([
            (format!("/latest/download/{MANIFEST}"), manifest),
            (format!("/latest/download/{MANIFEST}.sig"), signature),
            (format!("/download/v{version}/{SETUP}"), setup.to_vec()),
        ])
    }

    #[test]
    fn versiones() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.9", "0.2.0"));
        for bad in ["", "1.2", "1.2.3.4", "1.2.x", "v1.2.3", "1.2.+3", "..\\1.2.3"] {
            assert_eq!(parse_version(bad), None, "{bad}");
        }
    }

    #[test]
    fn la_clave_publica_es_valida() {
        let bytes = hex32(PUBLIC_KEY).expect("64 hex");
        VerifyingKey::from_bytes(&bytes).expect("punto de la curva");
    }

    #[test]
    fn ofrece_la_version_nueva_y_descarga_lo_firmado() {
        let (signer, public) = test_key();
        let base = serve(release_files("0.3.0", b"instalador", &signer));
        let release = check(&base, &public, "0.2.0").unwrap().expect("hay una nueva");
        assert_eq!(release.version, "0.3.0");
        assert_eq!(check(&base, &public, "0.3.0").unwrap(), None);
        assert_eq!(check(&base, &public, "0.4.0").unwrap(), None);
        let dir = std::env::temp_dir().join(format!("camdesk-update-ok-{}", std::process::id()));
        let path = download(&base, &release, &dir).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"instalador");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rechaza_otra_firma_y_un_instalador_cambiado() {
        let (signer, public) = test_key();
        let intruder = SigningKey::from_bytes(&[9u8; 32]);
        let base = serve(release_files("0.3.0", b"instalador", &intruder));
        assert!(check(&base, &public, "0.2.0").unwrap_err().contains("firma"));

        let mut files = release_files("0.3.0", b"instalador", &signer);
        files.insert(format!("/download/v0.3.0/{SETUP}"), b"otro programa".to_vec());
        let base = serve(files);
        let release = check(&base, &public, "0.2.0").unwrap().unwrap();
        let dir = std::env::temp_dir().join(format!("camdesk-update-mal-{}", std::process::id()));
        assert!(download(&base, &release, &dir).unwrap_err().contains("no coincide"));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "no tiene que quedar nada ejecutable");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sin_release_publicado_es_un_error_silencioso() {
        let base = serve(HashMap::new());
        assert!(check(&base, &test_key().1, "0.2.0").is_err());
    }
}
