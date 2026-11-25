//! Grabación de un monitor a 30 fps, con loopback y micrófono, en MP4 fragmentado.
//! No conoce cellCam. WASAPI y la captura se abren en `Recording::start`, no al cargar el crate.

mod mux;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use wasapi::{
    DeviceCollection, Direction, Role, SampleType, StreamMode, WaveFormat, get_default_device,
    get_default_device_for_role, initialize_mta,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Shell::{FOLDERID_Videos, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::monitor::Monitor;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

use crate::mux::{Mux, probe_video_encoder};

const FPS: u64 = 30;
const RATE: usize = 48_000;
const SAMPLES_PER_TICK: usize = RATE / FPS as usize;
/// La vista previa no necesita más que esto, y así no frena la grabación.
const PREVIEW_WIDTH: u32 = 960;
const PREVIEW_EVERY: u64 = 3;
/// Cuánto cae un medidor por segundo. La barra entera son 48 dB: medio son 24 dB por segundo.
const METER_FALL: f32 = 0.5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorId(pub String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicId(pub String);

/// Salida de reproducción cuyo sonido se puede grabar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputId(pub String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorInfo {
    pub id: MonitorId,
    /// Único en la lista: lleva un número delante aunque dos pantallas sean del mismo modelo.
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub primary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputInfo {
    pub id: OutputId,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicInfo {
    pub id: MicId,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct RecordConfig {
    pub monitor: MonitorId,
    pub system_audio: bool,
    /// Salida que se graba. `None` es la predeterminada de Windows, y la sigue si cambia.
    pub system_device: Option<OutputId>,
    pub mic: Option<MicId>,
    pub output: PathBuf,
}

#[derive(Debug)]
pub struct RecordError {
    pub message: String,
}

impl std::fmt::Display for RecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for RecordError {}

fn err(message: impl Into<String>) -> RecordError {
    RecordError { message: message.into() }
}

pub fn monitors() -> Vec<MonitorInfo> {
    let list = match Monitor::enumerate() {
        Ok(list) => list,
        Err(_) => return Vec::new(),
    };
    let primary = Monitor::primary().ok();
    let mut found: Vec<(usize, Monitor)> = list
        .into_iter()
        .filter_map(|monitor| Some((monitor.index().ok()?, monitor)))
        .collect();
    found.sort_by_key(|(index, _)| *index);
    found
        .into_iter()
        .enumerate()
        .map(|(position, (index, monitor))| MonitorInfo {
            id: MonitorId(index.to_string()),
            name: monitor_name(position + 1, monitor.name().ok()),
            width: monitor.width().unwrap_or(0),
            height: monitor.height().unwrap_or(0),
            primary: primary == Some(monitor),
        })
        .collect()
}

/// Windows da el nombre del modelo: dos pantallas iguales quedarían idénticas sin el número.
fn monitor_name(number: usize, model: Option<String>) -> String {
    match model.as_deref().map(str::trim) {
        Some(model) if !model.is_empty() => format!("{number} · {model}"),
        _ => format!("{number} · Monitor"),
    }
}

/// Salidas de reproducción activas. La predeterminada va primero.
pub fn outputs() -> Vec<OutputInfo> {
    let _ = initialize_mta();
    let collection = match DeviceCollection::new(&Direction::Render) {
        Ok(collection) => collection,
        Err(_) => return Vec::new(),
    };
    let preferred = default_render_id();
    let count = collection.get_nbr_devices().unwrap_or(0);
    let mut list = Vec::new();
    for index in 0..count {
        let Ok(device) = collection.get_device_at_index(index) else { continue };
        let Ok(id) = device.get_id() else { continue };
        let name = device.get_friendlyname().unwrap_or_else(|_| id.clone());
        list.push(OutputInfo { id: OutputId(id), name });
    }
    if let Some(preferred) = preferred {
        if let Some(pos) = list.iter().position(|item| item.id.0 == preferred) {
            let item = list.remove(pos);
            list.insert(0, item);
        }
    }
    list
}

pub fn microphones() -> Vec<MicInfo> {
    let _ = initialize_mta();
    let collection = match DeviceCollection::new(&Direction::Capture) {
        Ok(collection) => collection,
        Err(_) => return Vec::new(),
    };
    let preferred = get_default_device_for_role(&Direction::Capture, &Role::Communications)
        .ok()
        .and_then(|device| device.get_id().ok());
    let count = collection.get_nbr_devices().unwrap_or(0);
    let mut list = Vec::new();
    for index in 0..count {
        let Ok(device) = collection.get_device_at_index(index) else { continue };
        let Ok(id) = device.get_id() else { continue };
        let name = device.get_friendlyname().unwrap_or_else(|_| id.clone());
        list.push(MicInfo { id: MicId(id), name });
    }
    if let Some(preferred) = preferred {
        if let Some(pos) = list.iter().position(|mic| mic.id.0 == preferred) {
            let mic = list.remove(pos);
            list.insert(0, mic);
        }
    }
    list
}

pub fn default_output() -> PathBuf {
    let dir = videos_dir().join("Camdesk");
    dir.join(format!("grabacion-{}.mp4", local_stamp()))
}

/// Abre el primer encoder disponible y lo cierra. Lo usa el arranque de la grabación.
pub fn video_encoder_name() -> Result<String, RecordError> {
    probe_video_encoder().map_err(err)
}

struct SharedAudio {
    loopback: VecDeque<f32>,
    mic: VecDeque<f32>,
}

struct Levels {
    system: AtomicU32,
    mic: AtomicU32,
}

impl Levels {
    fn new() -> Self {
        Self { system: AtomicU32::new(0), mic: AtomicU32::new(0) }
    }

    fn pair(&self) -> (f32, f32) {
        (
            f32::from_bits(self.system.load(Ordering::Relaxed)),
            f32::from_bits(self.mic.load(Ordering::Relaxed)),
        )
    }
}

/// Cuadro reducido de lo que se está grabando, en RGBA, para mostrarlo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenPreview {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

type PreviewSlot = Arc<Mutex<Option<ScreenPreview>>>;

/// Muestra un monitor sin grabarlo: no abre audio ni encoder ni escribe nada.
/// Soltarlo cierra la captura; no espera, así que se puede soltar desde la UI.
pub struct ScreenWatch {
    monitor: MonitorId,
    preview: PreviewSlot,
    stop: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
}

impl ScreenWatch {
    pub fn start(monitor: MonitorId) -> Result<Self, RecordError> {
        if !monitors().iter().any(|item| item.id == monitor) {
            return Err(err("no está ese monitor"));
        }
        let preview = PreviewSlot::default();
        let stop = Arc::new(AtomicBool::new(false));
        let failure = Arc::new(Mutex::new(None));
        let (id, slot, stop_thread, failed) = (monitor.0.clone(), preview.clone(), stop.clone(), failure.clone());
        std::thread::Builder::new()
            .name("camdesk-watch".into())
            .spawn(move || {
                if let Err(error) = watch(&id, &stop_thread, &slot) {
                    *failed.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(error);
                }
            })
            .map_err(|error| err(error.to_string()))?;
        Ok(Self { monitor, preview, stop, failure })
    }

    pub fn monitor(&self) -> &MonitorId {
        &self.monitor
    }

    /// El último cuadro desde la llamada anterior. Con la pantalla quieta no hay uno nuevo.
    pub fn take_preview(&self) -> Option<ScreenPreview> {
        self.preview.lock().unwrap_or_else(|poison| poison.into_inner()).take()
    }

    pub fn error(&self) -> Option<String> {
        self.failure.lock().ok().and_then(|slot| slot.clone())
    }
}

impl Drop for ScreenWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

pub struct Recording {
    preview: PreviewSlot,
    stop: Arc<AtomicBool>,
    started: Instant,
    join: Option<JoinHandle<Result<(), String>>>,
    path: PathBuf,
    encoder: String,
    failure: Arc<Mutex<Option<String>>>,
    levels: Arc<Levels>,
}

impl Recording {
    pub fn start(cfg: RecordConfig) -> Result<Self, RecordError> {
        let monitor = monitors()
            .into_iter()
            .find(|item| item.id == cfg.monitor)
            .ok_or_else(|| err("no está ese monitor"))?;
        if cfg.system_audio {
            if let Some(device) = &cfg.system_device {
                if !outputs().iter().any(|item| item.id == *device) {
                    return Err(err("la salida de audio elegida no está conectada"));
                }
            }
        }
        if let Some(parent) = cfg.output.parent() {
            std::fs::create_dir_all(parent).map_err(|error| err(error.to_string()))?;
        }
        let width = (monitor.width & !1).max(2);
        let height = (monitor.height & !1).max(2);
        let mux = Mux::open(&cfg.output, width, height).map_err(err)?;
        let encoder = mux.encoder().to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let failure = Arc::new(Mutex::new(None));
        let levels = Arc::new(Levels::new());
        let stop_thread = stop.clone();
        let failure_thread = failure.clone();
        let levels_thread = levels.clone();
        let preview = PreviewSlot::default();
        let preview_thread = preview.clone();
        let system_audio = cfg.system_audio;
        let system_device = cfg.system_device.map(|id| id.0);
        let mic = cfg.mic.clone();
        let monitor_id = cfg.monitor.0.clone();
        let join = std::thread::Builder::new()
            .name("camdesk-record".into())
            .spawn(move || {
                let sound = Sound { system: system_audio, system_device, mic: mic.map(|id| id.0) };
                let shared = Shared { levels: levels_thread, preview: preview_thread };
                if let Err(error) = run(mux, stop_thread, &monitor_id, width, height, sound, shared) {
                    *failure_thread.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(error.clone());
                    return Err(error);
                }
                Ok(())
            })
            .map_err(|error| err(error.to_string()))?;
        Ok(Self {
            preview,
            stop,
            started: Instant::now(),
            join: Some(join),
            path: cfg.output,
            encoder,
            failure,
            levels,
        })
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Nivel reciente de 0 a 1: audio del sistema y micrófono. 0 es silencio.
    pub fn levels(&self) -> (f32, f32) {
        self.levels.pair()
    }

    /// El último cuadro de la pantalla desde la llamada anterior, unas 10 veces por segundo.
    pub fn take_preview(&self) -> Option<ScreenPreview> {
        self.preview.lock().unwrap_or_else(|poison| poison.into_inner()).take()
    }

    pub fn encoder(&self) -> &str {
        &self.encoder
    }

    pub fn error(&self) -> Option<String> {
        self.failure.lock().ok().and_then(|slot| slot.clone())
    }

    pub fn stop(mut self) -> Result<PathBuf, RecordError> {
        self.stop.store(true, Ordering::Release);
        let joined = self.join.take().map(|join| join.join());
        match joined {
            Some(Ok(Ok(()))) => Ok(self.path.clone()),
            Some(Ok(Err(message))) => Err(err(message)),
            Some(Err(_)) => Err(err("el hilo de grabación se cortó")),
            None => Ok(self.path.clone()),
        }
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

struct Captured {
    width: u32,
    height: u32,
    bgra: Vec<u8>,
}

struct Screen {
    latest: Arc<Mutex<Option<Captured>>>,
}

impl GraphicsCaptureApiHandler for Screen {
    type Flags = Arc<Mutex<Option<Captured>>>;
    type Error = String;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self { latest: ctx.flags })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        _capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let mut buffer = frame.buffer().map_err(|error| error.to_string())?;
        let width = buffer.width();
        let height = buffer.height();
        let pitch = buffer.row_pitch() as usize;
        let raw = buffer.as_raw_buffer();
        let row = width as usize * 4;
        let mut bgra = vec![0u8; row * height as usize];
        for y in 0..height as usize {
            let from = y * pitch;
            let to = y * row;
            if from + row <= raw.len() {
                bgra[to..to + row].copy_from_slice(&raw[from..from + row]);
            }
        }
        *self.latest.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(Captured { width, height, bgra });
        Ok(())
    }
}

/// Qué fuentes de audio entran en la grabación.
struct Sound {
    system: bool,
    system_device: Option<String>,
    mic: Option<String>,
}

/// Lo que el hilo de grabación deja a la vista de quien lo arrancó.
struct Shared {
    levels: Arc<Levels>,
    preview: PreviewSlot,
}

fn run(
    mut mux: Mux,
    stop: Arc<AtomicBool>,
    monitor_id: &str,
    width: u32,
    height: u32,
    sound: Sound,
    shared: Shared,
) -> Result<(), String> {
    let Shared { levels, preview } = shared;
    let latest = Arc::new(Mutex::new(None));
    let monitor = find_monitor(monitor_id)?;
    let capture = Screen::start_free_threaded(capture_settings(monitor, Duration::from_millis(16), latest.clone()))
        .map_err(|error| error.to_string())?;
    let audio = Arc::new(Mutex::new(SharedAudio {
        loopback: VecDeque::new(),
        mic: VecDeque::new(),
    }));
    let audio_stop = stop.clone();
    let audio_slot = audio.clone();
    let audio_thread = std::thread::Builder::new()
        .name("camdesk-audio".into())
        .spawn(move || audio_loop(audio_stop, audio_slot, sound, levels))
        .map_err(|error| error.to_string())?;

    // La vista previa se reduce en otro hilo: si se atrasa, se saltea; la grabación no espera.
    let (to_preview, previews) = mpsc::sync_channel::<Vec<u8>>(1);
    std::thread::Builder::new()
        .name("camdesk-preview".into())
        .spawn(move || {
            for frame in previews {
                let small = shrink_to_rgba(&frame, width, height, PREVIEW_WIDTH);
                *preview.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(small);
            }
        })
        .map_err(|error| error.to_string())?;

    let freq = qpc_freq();
    let clock = qpc_now();
    let mut frame_index = 0u64;
    let mut held: Option<Captured> = None;
    let mut canvas = vec![0u8; width as usize * height as usize * 4];
    let mut pending = Vec::<f32>::new();
    let frame_samples = mux.audio_frame_size() * 2;

    while !stop.load(Ordering::Acquire) {
        let target = clock + frame_index as i64 * freq / FPS as i64;
        let now = qpc_now();
        if target > now {
            let nanos = (target - now) as u128 * 1_000_000_000 / freq as u128;
            std::thread::sleep(Duration::from_nanos(nanos.min(u64::MAX as u128) as u64));
        }
        if let Some(frame) = latest.lock().unwrap_or_else(|poison| poison.into_inner()).clone_fresh() {
            held = Some(frame);
        }
        if let Some(frame) = &held {
            fit(&frame.bgra, frame.width, frame.height, &mut canvas, width, height);
        } else {
            canvas.fill(0);
        }
        mux.write_video(&canvas)?;
        // Sale del mismo lienzo que va al MP4: se ve lo que se graba.
        if held.is_some() && frame_index % PREVIEW_EVERY == 0 {
            let _ = to_preview.try_send(canvas.clone());
        }
        pull_audio(&audio, &mut pending);
        let usable = pending.len() - pending.len() % frame_samples;
        if usable > 0 {
            mux.write_audio(&pending[..usable])?;
            pending.drain(..usable);
        }
        frame_index += 1;
        if capture.is_finished() {
            return Err("la captura de pantalla se cerró".into());
        }
    }
    let _ = capture.stop();
    let _ = audio_thread.join();
    mux.finish()
}

fn find_monitor(monitor_id: &str) -> Result<Monitor, String> {
    let index: usize = monitor_id.parse().map_err(|_| "el monitor no tiene índice".to_string())?;
    // `from_index` toma la posición en la enumeración, que no tiene por qué ser el número de pantalla.
    Monitor::enumerate()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|monitor| monitor.index().ok() == Some(index))
        .ok_or_else(|| "no está ese monitor".to_string())
}

fn capture_settings(
    monitor: Monitor,
    interval: Duration,
    latest: Arc<Mutex<Option<Captured>>>,
) -> Settings<Arc<Mutex<Option<Captured>>>, Monitor> {
    Settings::new(
        monitor,
        CursorCaptureSettings::WithCursor,
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Custom(interval),
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        latest,
    )
}

/// La captura de `ScreenWatch`: unos 10 cuadros por segundo, reducidos.
fn watch(monitor_id: &str, stop: &AtomicBool, preview: &PreviewSlot) -> Result<(), String> {
    let interval = Duration::from_millis(100);
    let latest = Arc::new(Mutex::new(None));
    let settings = capture_settings(find_monitor(monitor_id)?, interval, latest.clone());
    let capture = Screen::start_free_threaded(settings).map_err(|error| error.to_string())?;
    while !stop.load(Ordering::Acquire) {
        let frame = latest.lock().unwrap_or_else(|poison| poison.into_inner()).take();
        if let Some(frame) = frame {
            let small = shrink_to_rgba(&frame.bgra, frame.width, frame.height, PREVIEW_WIDTH);
            *preview.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(small);
        }
        if capture.is_finished() {
            return Err("la captura de pantalla se cerró".into());
        }
        std::thread::sleep(interval);
    }
    capture.stop().map_err(|error| error.to_string())
}

trait CloneFresh {
    fn clone_fresh(&self) -> Option<Captured>;
}

impl CloneFresh for Option<Captured> {
    fn clone_fresh(&self) -> Option<Captured> {
        self.as_ref().map(|frame| Captured {
            width: frame.width,
            height: frame.height,
            bgra: frame.bgra.clone(),
        })
    }
}

fn pull_audio(slot: &Mutex<SharedAudio>, pending: &mut Vec<f32>) {
    let mut audio = slot.lock().unwrap_or_else(|poison| poison.into_inner());
    trim(&mut audio.loopback);
    trim(&mut audio.mic);
    let mut mixed = vec![0f32; SAMPLES_PER_TICK * 2];
    take_into(&mut audio.loopback, &mut mixed);
    take_add(&mut audio.mic, &mut mixed);
    for sample in &mut mixed {
        *sample = sample.clamp(-1.0, 1.0);
    }
    pending.extend_from_slice(&mixed);
}

fn trim(queue: &mut VecDeque<f32>) {
    let cap = (SAMPLES_PER_TICK + RATE / 50) * 2;
    while queue.len() > cap {
        queue.pop_front();
    }
}

fn take_into(queue: &mut VecDeque<f32>, dest: &mut [f32]) {
    for sample in dest.iter_mut() {
        *sample = queue.pop_front().unwrap_or(0.0);
    }
}

fn take_add(queue: &mut VecDeque<f32>, dest: &mut [f32]) {
    for sample in dest.iter_mut() {
        *sample += queue.pop_front().unwrap_or(0.0);
    }
}

/// Reduce un BGRA a `max_width` como mucho y lo pasa a RGBA opaco. Promedia cuatro muestras
/// por píxel: tomando una sola, el texto de la pantalla queda ilegible.
fn shrink_to_rgba(bgra: &[u8], width: u32, height: u32, max_width: u32) -> ScreenPreview {
    let (sw, sh) = (width as usize, height as usize);
    let dw = width.min(max_width).max(1) as usize;
    let dh = (sh * dw / sw.max(1)).max(1);
    let mut rgba = vec![0u8; dw * dh * 4];
    if sw == 0 || sh == 0 || bgra.len() < sw * sh * 4 {
        return ScreenPreview { width: dw as u32, height: dh as u32, rgba };
    }
    for (y, row) in rgba.chunks_exact_mut(dw * 4).enumerate() {
        let rows = [(4 * y + 1) * sh / (4 * dh), (4 * y + 3) * sh / (4 * dh)];
        for (x, out) in row.chunks_exact_mut(4).enumerate() {
            let columns = [(4 * x + 1) * sw / (4 * dw), (4 * x + 3) * sw / (4 * dw)];
            let mut sum = [0u32; 3];
            for sy in rows {
                for sx in columns {
                    let from = (sy * sw + sx) * 4;
                    for (channel, total) in sum.iter_mut().enumerate() {
                        *total += u32::from(bgra[from + channel]);
                    }
                }
            }
            out.copy_from_slice(&[(sum[2] / 4) as u8, (sum[1] / 4) as u8, (sum[0] / 4) as u8, 255]);
        }
    }
    ScreenPreview { width: dw as u32, height: dh as u32, rgba }
}

fn fit(src: &[u8], sw: u32, sh: u32, dst: &mut [u8], dw: u32, dh: u32) {
    if sw == 0 || sh == 0 {
        dst.fill(0);
        return;
    }
    if sw == dw && sh == dh && src.len() == dst.len() {
        dst.copy_from_slice(src);
        return;
    }
    let sw = sw as usize;
    let sh = sh as usize;
    let dw = dw as usize;
    let dh = dh as usize;
    for y in 0..dh {
        let sy = y * sh / dh;
        for x in 0..dw {
            let sx = x * sw / dw;
            let from = (sy * sw + sx) * 4;
            let to = (y * dw + x) * 4;
            if from + 4 <= src.len() && to + 4 <= dst.len() {
                dst[to..to + 4].copy_from_slice(&src[from..from + 4]);
            }
        }
    }
}

struct OpenCapture {
    client: wasapi::AudioClient,
    capture: wasapi::AudioCaptureClient,
    id: String,
}

fn audio_loop(stop: Arc<AtomicBool>, slot: Arc<Mutex<SharedAudio>>, sound: Sound, levels: Arc<Levels>) {
    let Sound { system: system_audio, system_device, mic } = sound;
    let _ = initialize_mta();
    // Una salida elegida que se desconecta no se busca en cada vuelta: enumerar cuesta.
    let mut retry_at = Instant::now();
    let mut system_meter = Meter::new();
    let mut mic_meter = Meter::new();
    let mut loopback: Option<OpenCapture> = None;
    let mut microphone: Option<OpenCapture> = None;
    let mut scratch = vec![0u8; 16_384];
    while !stop.load(Ordering::Acquire) {
        if system_audio {
            if loopback.is_none() {
                if Instant::now() >= retry_at {
                    loopback = open_loopback(system_device.as_deref()).ok();
                    retry_at = Instant::now() + Duration::from_millis(500);
                }
            } else if let (None, Some(current)) = (&system_device, loopback.as_ref()) {
                // Solo la predeterminada se sigue: una salida elegida a mano no cambia sola.
                if let Some(default) = default_render_id() {
                    if default != current.id {
                        loopback = None;
                        retry_at = Instant::now();
                        continue;
                    }
                }
            }
        }
        if let Some(id) = mic.as_ref() {
            if microphone.as_ref().map(|item| &item.id) != Some(id) {
                microphone = open_mic(id).ok();
            }
        }
        let mut read_any = false;
        if let Some(source) = loopback.as_ref() {
            match read_floats(&source.capture, &mut scratch) {
                Ok(samples) if !samples.is_empty() => {
                    system_meter.feed(&levels.system, &samples);
                    slot.lock().unwrap_or_else(|poison| poison.into_inner()).loopback.extend(samples);
                    read_any = true;
                }
                Ok(_) => system_meter.feed(&levels.system, &[]),
                Err(_) => {
                    system_meter.feed(&levels.system, &[]);
                    loopback = None;
                }
            }
        } else if system_audio {
            system_meter.feed(&levels.system, &[]);
        }
        if let Some(source) = microphone.as_ref() {
            match read_floats(&source.capture, &mut scratch) {
                Ok(samples) if !samples.is_empty() => {
                    mic_meter.feed(&levels.mic, &samples);
                    slot.lock().unwrap_or_else(|poison| poison.into_inner()).mic.extend(samples);
                    read_any = true;
                }
                Ok(_) => mic_meter.feed(&levels.mic, &[]),
                Err(_) => {
                    mic_meter.feed(&levels.mic, &[]);
                    microphone = None;
                }
            }
        } else if mic.is_some() {
            mic_meter.feed(&levels.mic, &[]);
        }
        if !read_any {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    if let Some(source) = loopback.as_ref() {
        let _ = source.client.stop_stream();
    }
    if let Some(source) = microphone.as_ref() {
        let _ = source.client.stop_stream();
    }
}

/// Nivel de una pista: sube al instante y baja a ritmo constante en el tiempo, no por vuelta
/// del bucle, que no tiene un ritmo fijo.
struct Meter {
    value: f32,
    at: Instant,
}

impl Meter {
    fn new() -> Self {
        Self { value: 0.0, at: Instant::now() }
    }

    fn feed(&mut self, slot: &AtomicU32, samples: &[f32]) {
        let now = Instant::now();
        self.value = envelope(self.value, peak_level(samples), now.duration_since(self.at).as_secs_f32());
        self.at = now;
        slot.store(self.value.to_bits(), Ordering::Relaxed);
    }
}

/// Pico de 0 a 1, lineal en decibeles entre −48 dB y 0 dB.
fn peak_level(samples: &[f32]) -> f32 {
    let peak = samples.iter().fold(0f32, |peak, sample| peak.max(sample.abs()));
    if peak <= 1e-7 {
        0.0
    } else {
        ((20.0 * peak.log10() + 48.0) / 48.0).clamp(0.0, 1.0)
    }
}

fn envelope(previous: f32, target: f32, seconds: f32) -> f32 {
    target.max(previous - METER_FALL * seconds).max(0.0)
}

fn open_loopback(chosen: Option<&str>) -> Result<OpenCapture, wasapi::WasapiError> {
    let device = match chosen {
        Some(id) => find_device(&Direction::Render, id)?,
        None => get_default_device(&Direction::Render)?,
    };
    let id = device.get_id()?;
    let capture = start_capture(&device, &Direction::Capture)?;
    Ok(OpenCapture { client: capture.0, capture: capture.1, id })
}

fn open_mic(id: &str) -> Result<OpenCapture, wasapi::WasapiError> {
    let device = find_device(&Direction::Capture, id)?;
    let capture = start_capture(&device, &Direction::Capture)?;
    Ok(OpenCapture { client: capture.0, capture: capture.1, id: id.to_string() })
}

fn find_device(direction: &Direction, id: &str) -> Result<wasapi::Device, wasapi::WasapiError> {
    let collection = DeviceCollection::new(direction)?;
    let count = collection.get_nbr_devices()?;
    for index in 0..count {
        let device = collection.get_device_at_index(index)?;
        if device.get_id()? == id {
            return Ok(device);
        }
    }
    Err(wasapi::WasapiError::DeviceNotFound(id.to_string()))
}

fn start_capture(
    device: &wasapi::Device,
    direction: &Direction,
) -> Result<(wasapi::AudioClient, wasapi::AudioCaptureClient), wasapi::WasapiError> {
    let mut client = device.get_iaudioclient()?;
    let format = WaveFormat::new(32, 32, &SampleType::Float, RATE, 2, None);
    let mode = StreamMode::PollingShared {
        autoconvert: true,
        buffer_duration_hns: 200_000,
    };
    client.initialize_client(&format, direction, &mode)?;
    let capture = client.get_audiocaptureclient()?;
    client.start_stream()?;
    Ok((client, capture))
}

fn default_render_id() -> Option<String> {
    get_default_device(&Direction::Render).ok().and_then(|device| device.get_id().ok())
}

fn read_floats(capture: &wasapi::AudioCaptureClient, scratch: &mut Vec<u8>) -> Result<Vec<f32>, wasapi::WasapiError> {
    let Some(frames) = capture.get_next_packet_size()? else {
        return Ok(Vec::new());
    };
    if frames == 0 {
        return Ok(Vec::new());
    }
    let bytes = frames as usize * 8;
    if scratch.len() < bytes {
        scratch.resize(bytes, 0);
    }
    let (got, info) = capture.read_from_device(&mut scratch[..bytes])?;
    let count = got as usize * 2;
    if info.flags.silent {
        return Ok(vec![0f32; count]);
    }
    let mut out = Vec::with_capacity(count);
    for chunk in scratch[..got as usize * 8].chunks_exact(4) {
        out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(out)
}

fn videos_dir() -> PathBuf {
    unsafe {
        match SHGetKnownFolderPath(&FOLDERID_Videos, KF_FLAG_DEFAULT, None) {
            Ok(ptr) => {
                let path = ptr.to_string().unwrap_or_else(|_| ".".into());
                CoTaskMemFree(Some(ptr.as_ptr() as *const _));
                PathBuf::from(path)
            }
            Err(_) => std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")).join("Videos"),
        }
    }
}

fn local_stamp() -> String {
    let time = unsafe { GetLocalTime() };
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond
    )
}

fn qpc_freq() -> i64 {
    let mut freq = 0i64;
    let _ = unsafe { QueryPerformanceFrequency(&mut freq) };
    freq.max(1)
}

fn qpc_now() -> i64 {
    let mut now = 0i64;
    let _ = unsafe { QueryPerformanceCounter(&mut now) };
    now
}

#[test]
#[ignore]
fn graba_un_segundo() {
    let bin = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../third_party/ffmpeg-n8.1-latest-win64-lgpl-shared-8.1/bin");
    std::env::set_var("CAMDESK_FFMPEG_DIR", &bin);
    let monitor = monitors().into_iter().next().expect("hace falta un monitor");
    let output = std::env::temp_dir().join(format!("camdesk-smoke-{}.mp4", std::process::id()));
    let _ = std::fs::remove_file(&output);
    let recording = Recording::start(RecordConfig {
        monitor: monitor.id,
        system_audio: true,
        system_device: None,
        mic: None,
        output: output.clone(),
    })
    .expect("la grabación tiene que arrancar");
    let encoder = recording.encoder().to_string();
    std::thread::sleep(Duration::from_millis(1200));
    let preview = recording.take_preview().expect("tiene que haber vista previa");
    assert!(preview.width <= PREVIEW_WIDTH && preview.rgba.len() == (preview.width * preview.height * 4) as usize);
    recording.stop().expect("la grabación tiene que cerrar");
    let bytes = std::fs::read(&output).expect("el MP4 tiene que existir");
    assert!(bytes.len() > 1024, "mp4 corto: {}", bytes.len());
    assert!(bytes.windows(4).any(|chunk| chunk == b"ftyp"), "sin ftyp");
    let probe = std::process::Command::new(bin.join("ffprobe.exe"))
        .args(["-v", "error", "-show_entries", "stream=codec_type,duration,avg_frame_rate", "-of", "csv=p=0", output.to_str().unwrap()])
        .output()
        .expect("ffprobe");
    let text = String::from_utf8_lossy(&probe.stdout);
    eprintln!("smoke encoder={encoder} bytes={} {}", bytes.len(), text.trim());
    let mut video_s = 0.0f64;
    let mut audio_s = 0.0f64;
    for line in text.lines() {
        let parts: Vec<_> = line.split(',').collect();
        if parts.len() < 3 {
            continue;
        }
        let duration: f64 = parts[2].parse().unwrap_or(0.0);
        if parts[0] == "video" {
            video_s = duration;
        } else if parts[0] == "audio" {
            audio_s = duration;
        }
    }
    assert!(audio_s > 0.8 && video_s > 0.8, "duración video={video_s} audio={audio_s}");
    assert!((video_s - audio_s).abs() < 0.5, "el video no acompaña al audio: {video_s} vs {audio_s}");
    let _ = std::fs::remove_file(&output);
}

/// Captura la pantalla de verdad: no forma parte de `cargo test`.
#[test]
#[ignore]
fn muestra_un_monitor_sin_grabar() {
    for monitor in monitors() {
        let watch = ScreenWatch::start(monitor.id.clone()).expect("tiene que arrancar");
        std::thread::sleep(Duration::from_millis(1500));
        let preview = watch.take_preview().unwrap_or_else(|| panic!("sin cuadro: {:?}", watch.error()));
        eprintln!("{} {}x{} -> vista {}x{}", monitor.name, monitor.width, monitor.height, preview.width, preview.height);
        assert_eq!(preview.width, monitor.width.min(PREVIEW_WIDTH));
        assert!(preview.rgba.chunks_exact(4).any(|pixel| pixel[..3] != [0, 0, 0]), "todo negro");
        assert_eq!(watch.error(), None);
    }
    assert!(ScreenWatch::start(MonitorId("99".into())).is_err());
}

#[test]
fn el_medidor_sube_al_instante_y_baja_segun_el_tiempo() {
    assert_eq!(envelope(0.1, 0.8, 0.001), 0.8);
    // En 16 ms baja menos del 1 %, sin importar cuántas vueltas dé el bucle.
    let once = envelope(0.8, 0.0, 0.016);
    let in_pieces = (0..4).fold(0.8, |value, _| envelope(value, 0.0, 0.004));
    assert!((0.8 - once) < 0.01 && (once - in_pieces).abs() < 1e-5, "{once} {in_pieces}");
    assert_eq!(envelope(0.2, 0.0, 10.0), 0.0);
    assert_eq!(peak_level(&[]), 0.0);
    assert_eq!(peak_level(&[0.0, -1.0]), 1.0);
}

#[test]
fn la_vista_previa_se_reduce_y_queda_en_rgba() {
    // Mitad izquierda azul, mitad derecha roja, en BGRA.
    let mut bgra = Vec::new();
    for _ in 0..8 {
        for x in 0..16 {
            bgra.extend_from_slice(if x < 8 { &[255, 0, 0, 0] } else { &[0, 0, 255, 0] });
        }
    }
    let small = shrink_to_rgba(&bgra, 16, 8, 4);
    assert_eq!((small.width, small.height, small.rgba.len()), (4, 2, 4 * 2 * 4));
    assert_eq!(&small.rgba[0..4], &[0, 0, 255, 255], "izquierda azul");
    assert_eq!(&small.rgba[12..16], &[255, 0, 0, 255], "derecha roja");
    // Una pantalla más chica que el máximo no se agranda.
    let same = shrink_to_rgba(&bgra, 16, 8, 960);
    assert_eq!((same.width, same.height), (16, 8));
}

#[test]
fn dos_pantallas_del_mismo_modelo_tienen_nombres_distintos() {
    let first = monitor_name(1, Some("18.5 monitor".into()));
    let second = monitor_name(2, Some("18.5 monitor".into()));
    assert_eq!(first, "1 · 18.5 monitor");
    assert_ne!(first, second);
    assert_eq!(monitor_name(3, Some("  ".into())), "3 · Monitor");
    assert_eq!(monitor_name(1, None), "1 · Monitor");
}

#[test]
fn el_encoder_no_es_libx264() {
    let bin = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../third_party/ffmpeg-n8.1-latest-win64-lgpl-shared-8.1/bin");
    if !bin.join("avcodec-62.dll").is_file() {
        panic!("falta el FFmpeg LGPL en {}", bin.display());
    }
    std::env::set_var("CAMDESK_FFMPEG_DIR", &bin);
    let name = video_encoder_name().expect("tiene que abrir NVENC, AMF, Quick Sync o h264_mf");
    assert!(!name.contains("libx264"), "{name}");
    assert!(
        ["h264_nvenc", "h264_amf", "h264_qsv", "h264_mf"].contains(&name.as_str()),
        "{name}"
    );
    eprintln!("encoder={name}");
}
