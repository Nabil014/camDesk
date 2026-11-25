//! Grabación de un monitor a 30 fps, con loopback y micrófono, en MP4 fragmentado.
//! No conoce cellCam. WASAPI y la captura se abren en `Recording::start`, no al cargar el crate.

mod mux;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorId(pub String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicId(pub String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorInfo {
    pub id: MonitorId,
    pub name: String,
    pub width: u32,
    pub height: u32,
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
    list.into_iter()
        .filter_map(|monitor| {
            let index = monitor.index().ok()?;
            let name = monitor.name().unwrap_or_else(|_| format!("Monitor {index}"));
            let width = monitor.width().unwrap_or(0);
            let height = monitor.height().unwrap_or(0);
            Some(MonitorInfo {
                id: MonitorId(index.to_string()),
                name,
                width,
                height,
            })
        })
        .collect()
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

pub struct Recording {
    stop: Arc<AtomicBool>,
    started: Instant,
    join: Option<JoinHandle<Result<(), String>>>,
    path: PathBuf,
    encoder: String,
    failure: Arc<Mutex<Option<String>>>,
}

impl Recording {
    pub fn start(cfg: RecordConfig) -> Result<Self, RecordError> {
        let monitor = monitors()
            .into_iter()
            .find(|item| item.id == cfg.monitor)
            .ok_or_else(|| err("no está ese monitor"))?;
        if let Some(parent) = cfg.output.parent() {
            std::fs::create_dir_all(parent).map_err(|error| err(error.to_string()))?;
        }
        let width = (monitor.width & !1).max(2);
        let height = (monitor.height & !1).max(2);
        let mux = Mux::open(&cfg.output, width, height).map_err(err)?;
        let encoder = mux.encoder().to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let failure = Arc::new(Mutex::new(None));
        let stop_thread = stop.clone();
        let failure_thread = failure.clone();
        let system_audio = cfg.system_audio;
        let mic = cfg.mic.clone();
        let monitor_id = cfg.monitor.0.clone();
        let join = std::thread::Builder::new()
            .name("camdesk-record".into())
            .spawn(move || {
                if let Err(error) = run(mux, stop_thread, &monitor_id, width, height, system_audio, mic) {
                    *failure_thread.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(error.clone());
                    return Err(error);
                }
                Ok(())
            })
            .map_err(|error| err(error.to_string()))?;
        Ok(Self {
            stop,
            started: Instant::now(),
            join: Some(join),
            path: cfg.output,
            encoder,
            failure,
        })
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
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

fn run(
    mut mux: Mux,
    stop: Arc<AtomicBool>,
    monitor_id: &str,
    width: u32,
    height: u32,
    system_audio: bool,
    mic: Option<MicId>,
) -> Result<(), String> {
    let index: usize = monitor_id.parse().map_err(|_| "el monitor no tiene índice".to_string())?;
    let monitor = Monitor::from_index(index).map_err(|error| error.to_string())?;
    let latest = Arc::new(Mutex::new(None));
    let settings = Settings::new(
        monitor,
        CursorCaptureSettings::WithCursor,
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Custom(Duration::from_millis(16)),
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        latest.clone(),
    );
    let capture = Screen::start_free_threaded(settings).map_err(|error| error.to_string())?;
    let audio = Arc::new(Mutex::new(SharedAudio {
        loopback: VecDeque::new(),
        mic: VecDeque::new(),
    }));
    let audio_stop = stop.clone();
    let audio_slot = audio.clone();
    let mic_id = mic.map(|id| id.0);
    let audio_thread = std::thread::Builder::new()
        .name("camdesk-audio".into())
        .spawn(move || audio_loop(audio_stop, audio_slot, system_audio, mic_id))
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

fn audio_loop(stop: Arc<AtomicBool>, slot: Arc<Mutex<SharedAudio>>, system_audio: bool, mic: Option<String>) {
    let _ = initialize_mta();
    let mut loopback: Option<OpenCapture> = None;
    let mut microphone: Option<OpenCapture> = None;
    let mut scratch = vec![0u8; 16_384];
    while !stop.load(Ordering::Acquire) {
        if system_audio {
            if loopback.is_none() {
                loopback = open_loopback().ok();
            } else if let Some(current) = loopback.as_ref() {
                if let Some(default) = default_render_id() {
                    if default != current.id {
                        loopback = None;
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
                    slot.lock().unwrap_or_else(|poison| poison.into_inner()).loopback.extend(samples);
                    read_any = true;
                }
                Ok(_) => {}
                Err(_) => loopback = None,
            }
        }
        if let Some(source) = microphone.as_ref() {
            match read_floats(&source.capture, &mut scratch) {
                Ok(samples) if !samples.is_empty() => {
                    slot.lock().unwrap_or_else(|poison| poison.into_inner()).mic.extend(samples);
                    read_any = true;
                }
                Ok(_) => {}
                Err(_) => microphone = None,
            }
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

fn open_loopback() -> Result<OpenCapture, wasapi::WasapiError> {
    let device = get_default_device(&Direction::Render)?;
    let id = device.get_id()?;
    let capture = start_capture(&device, &Direction::Capture)?;
    Ok(OpenCapture { client: capture.0, capture: capture.1, id })
}

fn open_mic(id: &str) -> Result<OpenCapture, wasapi::WasapiError> {
    let collection = DeviceCollection::new(&Direction::Capture)?;
    let count = collection.get_nbr_devices()?;
    for index in 0..count {
        let device = collection.get_device_at_index(index)?;
        if device.get_id()? == id {
            let capture = start_capture(&device, &Direction::Capture)?;
            return Ok(OpenCapture {
                client: capture.0,
                capture: capture.1,
                id: id.to_string(),
            });
        }
    }
    Err(wasapi::WasapiError::UnsupportedFormat)
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
        mic: None,
        output: output.clone(),
    })
    .expect("la grabación tiene que arrancar");
    let encoder = recording.encoder().to_string();
    std::thread::sleep(Duration::from_millis(1200));
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
