//! MP4 fragmentado con el FFmpeg LGPL cargado en runtime.
//! Offsets de FFmpeg 8.1 (avcodec 62, avformat 62) en Windows x64.

use std::ffi::{CString, c_void};
use std::path::Path;
use std::ptr;

use libloading::Library;

const EAGAIN: i32 = -11;
const EOF: i32 = -541_478_725;
const NV12: i32 = 23;
const YUV420P: i32 = 0;
const BGRA: i32 = 28;
const FLTP: i32 = 8;
const SWS_BILINEAR: i32 = 2;
const GLOBAL_HEADER: i32 = 1 << 22;
const AVIO_FLAG_WRITE: i32 = 2;
const NOPTS: i64 = i64::MIN;

const OFF_BITRATE: usize = 56;
const OFF_FLAGS: usize = 64;
const OFF_TIME_BASE: usize = 84;
const OFF_FRAMERATE: usize = 100;
const OFF_WIDTH: usize = 112;
const OFF_HEIGHT: usize = 116;
const OFF_PIX_FMT: usize = 136;
const OFF_GOP: usize = 332;
const OFF_SAMPLE_RATE: usize = 344;
const OFF_SAMPLE_FMT: usize = 348;
const OFF_CH_LAYOUT: usize = 352;
const OFF_FRAME_SIZE: usize = 376;

const OFF_FRAME_WIDTH: usize = 104;
const OFF_FRAME_HEIGHT: usize = 108;
const OFF_FRAME_FORMAT: usize = 116;
const OFF_FRAME_PTS: usize = 136;
const OFF_FRAME_CH: usize = 384;
const OFF_PKT_PTS: usize = 8;
const OFF_PKT_DTS: usize = 16;
const OFF_PKT_STREAM: usize = 36;
const OFF_PKT_DURATION: usize = 64;
const OFF_STREAM_CODECPAR: usize = 16;
const OFF_STREAM_TIME_BASE: usize = 32;
const OFF_FMT_PB: usize = 32;

const ENCODERS: &[&str] = &["h264_nvenc", "h264_amf", "h264_qsv", "h264_mf"];

type FnPtr = unsafe extern "C" fn() -> *mut c_void;

struct Ff {
    _avutil: Library,
    _swresample: Library,
    _swscale: Library,
    _avcodec: Library,
    _avformat: Library,
    find_encoder: unsafe extern "C" fn(*const i8) -> *mut c_void,
    alloc_context: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    open: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> i32,
    free_context: unsafe extern "C" fn(*mut *mut c_void),
    send_frame: unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32,
    receive_packet: unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32,
    params_from_context: unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32,
    packet_alloc: FnPtr,
    packet_free: unsafe extern "C" fn(*mut *mut c_void),
    packet_unref: unsafe extern "C" fn(*mut c_void),
    frame_alloc: FnPtr,
    frame_free: unsafe extern "C" fn(*mut *mut c_void),
    frame_get_buffer: unsafe extern "C" fn(*mut c_void, i32) -> i32,
    channel_layout: unsafe extern "C" fn(*mut c_void, i32),
    sws_get: unsafe extern "C" fn(i32, i32, i32, i32, i32, i32, i32, *mut c_void, *mut c_void, *const f64) -> *mut c_void,
    sws_scale: unsafe extern "C" fn(*mut c_void, *const *const u8, *const i32, i32, i32, *mut *mut u8, *const i32) -> i32,
    sws_free: unsafe extern "C" fn(*mut c_void),
    alloc_output: unsafe extern "C" fn(*mut *mut c_void, *mut c_void, *const i8, *const i8) -> i32,
    new_stream: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void,
    avio_open: unsafe extern "C" fn(*mut *mut c_void, *const i8, i32) -> i32,
    avio_closep: unsafe extern "C" fn(*mut *mut c_void) -> i32,
    write_header: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> i32,
    write_frame: unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32,
    write_trailer: unsafe extern "C" fn(*mut c_void) -> i32,
    free_context_fmt: unsafe extern "C" fn(*mut c_void),
    dict_set: unsafe extern "C" fn(*mut *mut c_void, *const i8, *const i8, i32) -> i32,
    dict_free: unsafe extern "C" fn(*mut *mut c_void),
}

fn sym<T: Copy>(lib: &Library, name: &[u8]) -> Result<T, String> {
    unsafe {
        lib.get(name)
            .map(|symbol| *symbol)
            .map_err(|err| format!("{}: {err}", String::from_utf8_lossy(name)))
    }
}

fn ffmpeg_dir() -> Result<std::path::PathBuf, String> {
    if let Some(dir) = std::env::var_os("CAMDESK_FFMPEG_DIR") {
        return Ok(std::path::PathBuf::from(dir));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if dir.join("avcodec-62.dll").is_file() {
                return Ok(dir.to_path_buf());
            }
        }
    }
    Err("no están las DLL LGPL de FFmpeg (CAMDESK_FFMPEG_DIR o junto al exe)".into())
}

fn load() -> Result<Ff, String> {
    let dir = ffmpeg_dir()?;
    let dll = |name: &str| -> Result<Library, String> {
        let path = dir.join(name);
        unsafe { Library::new(&path) }.map_err(|err| format!("{}: {err}", path.display()))
    };
    let avutil = dll("avutil-60.dll")?;
    let swresample = dll("swresample-6.dll")?;
    let swscale = dll("swscale-9.dll")?;
    let avcodec = dll("avcodec-62.dll")?;
    let avformat = dll("avformat-62.dll")?;
    Ok(Ff {
        find_encoder: sym(&avcodec, b"avcodec_find_encoder_by_name")?,
        alloc_context: sym(&avcodec, b"avcodec_alloc_context3")?,
        open: sym(&avcodec, b"avcodec_open2")?,
        free_context: sym(&avcodec, b"avcodec_free_context")?,
        send_frame: sym(&avcodec, b"avcodec_send_frame")?,
        receive_packet: sym(&avcodec, b"avcodec_receive_packet")?,
        params_from_context: sym(&avcodec, b"avcodec_parameters_from_context")?,
        packet_alloc: sym(&avcodec, b"av_packet_alloc")?,
        packet_free: sym(&avcodec, b"av_packet_free")?,
        packet_unref: sym(&avcodec, b"av_packet_unref")?,
        frame_alloc: sym(&avutil, b"av_frame_alloc")?,
        frame_free: sym(&avutil, b"av_frame_free")?,
        frame_get_buffer: sym(&avutil, b"av_frame_get_buffer")?,
        channel_layout: sym(&avutil, b"av_channel_layout_default")?,
        sws_get: sym(&swscale, b"sws_getContext")?,
        sws_scale: sym(&swscale, b"sws_scale")?,
        sws_free: sym(&swscale, b"sws_freeContext")?,
        alloc_output: sym(&avformat, b"avformat_alloc_output_context2")?,
        new_stream: sym(&avformat, b"avformat_new_stream")?,
        avio_open: sym(&avformat, b"avio_open")?,
        avio_closep: sym(&avformat, b"avio_closep")?,
        write_header: sym(&avformat, b"avformat_write_header")?,
        write_frame: sym(&avformat, b"av_interleaved_write_frame")?,
        write_trailer: sym(&avformat, b"av_write_trailer")?,
        free_context_fmt: sym(&avformat, b"avformat_free_context")?,
        dict_set: sym(&avutil, b"av_dict_set")?,
        dict_free: sym(&avutil, b"av_dict_free")?,
        _avutil: avutil,
        _swresample: swresample,
        _swscale: swscale,
        _avcodec: avcodec,
        _avformat: avformat,
    })
}

pub struct Mux {
    ff: Ff,
    encoder: String,
    fmt: *mut c_void,
    video: *mut c_void,
    audio: *mut c_void,
    vframe: *mut c_void,
    aframe: *mut c_void,
    packet: *mut c_void,
    sws: *mut c_void,
    width: i32,
    height: i32,
    audio_frame: i32,
    video_pts: i64,
    audio_pts: i64,
}

unsafe impl Send for Mux {}

pub fn probe_video_encoder() -> Result<String, String> {
    let ff = load()?;
    let (name, ctx, _) = open_video(&ff, 640, 360)?;
    let mut ctx = ctx;
    unsafe { (ff.free_context)(&mut ctx) };
    Ok(name)
}

impl Mux {
    pub fn open(path: &Path, width: u32, height: u32) -> Result<Self, String> {
        let ff = load()?;
        let width = (width & !1).max(2) as i32;
        let height = (height & !1).max(2) as i32;
        let (encoder, video, pix_fmt) = open_video(&ff, width, height)?;
        let audio = match open_audio(&ff) {
            Ok(ctx) => ctx,
            Err(err) => {
                let mut video = video;
                unsafe { (ff.free_context)(&mut video) };
                return Err(err);
            }
        };
        let audio_frame = unsafe { read_i32(audio, OFF_FRAME_SIZE) }.max(1024);
        let path_c = CString::new(path.to_string_lossy().as_bytes()).map_err(|_| "la ruta del MP4 no es válida".to_string())?;
        let format = CString::new("mp4").unwrap();
        let mut fmt = ptr::null_mut();
        let rc = unsafe { (ff.alloc_output)(&mut fmt, ptr::null_mut(), format.as_ptr(), path_c.as_ptr()) };
        if rc < 0 || fmt.is_null() {
            free_pair(&ff, video, audio);
            return Err(format!("no se pudo crear el MP4 ({rc})"));
        }
        let vstream = unsafe { (ff.new_stream)(fmt, ptr::null_mut()) };
        let astream = unsafe { (ff.new_stream)(fmt, ptr::null_mut()) };
        if vstream.is_null() || astream.is_null() {
            unsafe { (ff.free_context_fmt)(fmt) };
            free_pair(&ff, video, audio);
            return Err("no se pudieron crear las pistas".into());
        }
        if let Err(err) = copy_params(&ff, vstream, video).and_then(|_| copy_params(&ff, astream, audio)) {
            unsafe { (ff.free_context_fmt)(fmt) };
            free_pair(&ff, video, audio);
            return Err(err);
        }
        unsafe {
            write_i32(vstream, OFF_STREAM_TIME_BASE, 1);
            write_i32(vstream, OFF_STREAM_TIME_BASE + 4, 30);
            write_i32(astream, OFF_STREAM_TIME_BASE, 1);
            write_i32(astream, OFF_STREAM_TIME_BASE + 4, 48_000);
        }
        let mut pb = ptr::null_mut();
        let rc = unsafe { (ff.avio_open)(&mut pb, path_c.as_ptr(), AVIO_FLAG_WRITE) };
        if rc < 0 || pb.is_null() {
            unsafe { (ff.free_context_fmt)(fmt) };
            free_pair(&ff, video, audio);
            return Err(format!("no se pudo crear el archivo ({rc})"));
        }
        unsafe { write_ptr(fmt, OFF_FMT_PB, pb) };
        let mut dict = ptr::null_mut();
        let key = CString::new("movflags").unwrap();
        let val = CString::new("frag_keyframe+empty_moov+default_base_moof").unwrap();
        unsafe {
            (ff.dict_set)(&mut dict, key.as_ptr(), val.as_ptr(), 0);
            let rc = (ff.write_header)(fmt, &mut dict);
            (ff.dict_free)(&mut dict);
            if rc < 0 {
                let mut pb = read_ptr(fmt, OFF_FMT_PB);
                (ff.avio_closep)(&mut pb);
                (ff.free_context_fmt)(fmt);
                free_pair(&ff, video, audio);
                return Err(format!("no se pudo escribir la cabecera ({rc})"));
            }
        }
        let vframe = unsafe { (ff.frame_alloc)() };
        let aframe = unsafe { (ff.frame_alloc)() };
        let packet = unsafe { (ff.packet_alloc)() };
        if vframe.is_null() || aframe.is_null() || packet.is_null() {
            return Err("no se pudieron reservar los frames".into());
        }
        unsafe {
            write_i32(vframe, OFF_FRAME_WIDTH, width);
            write_i32(vframe, OFF_FRAME_HEIGHT, height);
            write_i32(vframe, OFF_FRAME_FORMAT, pix_fmt);
            let rc = (ff.frame_get_buffer)(vframe, 32);
            if rc < 0 {
                return Err(format!("no se pudo reservar el frame de video ({rc})"));
            }
            write_i32(aframe, 112, audio_frame);
            write_i32(aframe, 116, FLTP);
            (ff.channel_layout)(aframe.cast::<u8>().add(OFF_FRAME_CH).cast(), 2);
            let rc = (ff.frame_get_buffer)(aframe, 0);
            if rc < 0 {
                return Err(format!("no se pudo reservar el frame de audio ({rc})"));
            }
        }
        let sws = unsafe {
            (ff.sws_get)(
                width,
                height,
                BGRA,
                width,
                height,
                pix_fmt,
                SWS_BILINEAR,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            )
        };
        if sws.is_null() {
            return Err("no se pudo preparar la conversión a NV12".into());
        }
        Ok(Self {
            ff,
            encoder,
            fmt,
            video,
            audio,
            vframe,
            aframe,
            packet,
            sws,
            width,
            height,
            audio_frame,
            video_pts: 0,
            audio_pts: 0,
        })
    }

    pub fn encoder(&self) -> &str {
        &self.encoder
    }

    pub fn write_video(&mut self, bgra: &[u8]) -> Result<(), String> {
        let src = [bgra.as_ptr()];
        let src_stride = [self.width * 4];
        let mut dst = [ptr::null_mut::<u8>(); 4];
        let mut dst_stride = [0i32; 4];
        unsafe {
            for i in 0..4 {
                dst[i] = read_ptr(self.vframe, i * 8).cast();
                dst_stride[i] = read_i32(self.vframe, 64 + i * 4);
            }
            let rc = (self.ff.sws_scale)(
                self.sws,
                src.as_ptr(),
                src_stride.as_ptr(),
                0,
                self.height,
                dst.as_mut_ptr(),
                dst_stride.as_ptr(),
            );
            if rc < 0 {
                return Err(format!("sws_scale devolvió {rc}"));
            }
            write_i64(self.vframe, OFF_FRAME_PTS, self.video_pts);
        }
        self.video_pts += 1;
        self.encode(self.video, self.vframe, 0, 1)
    }

    pub fn write_audio(&mut self, interleaved: &[f32]) -> Result<(), String> {
        let mut offset = 0;
        let channels = 2usize;
        while offset + self.audio_frame as usize <= interleaved.len() / channels {
            let start = offset * channels;
            unsafe {
                let left = read_ptr(self.aframe, 0).cast::<u8>();
                let right = read_ptr(self.aframe, 8).cast::<u8>();
                for n in 0..self.audio_frame as usize {
                    let sample = start + n * channels;
                    *left.add(n * 4).cast::<f32>() = interleaved[sample];
                    *right.add(n * 4).cast::<f32>() = interleaved[sample + 1];
                }
                write_i64(self.aframe, OFF_FRAME_PTS, self.audio_pts);
            }
            self.audio_pts += self.audio_frame as i64;
            offset += self.audio_frame as usize;
            self.encode(self.audio, self.aframe, 1, self.audio_frame as i64)?;
        }
        let _ = offset;
        Ok(())
    }

    pub fn audio_frame_size(&self) -> usize {
        self.audio_frame as usize
    }

    pub fn finish(mut self) -> Result<(), String> {
        self.encode(self.video, ptr::null_mut(), 0, 1)?;
        self.encode(self.audio, ptr::null_mut(), 1, self.audio_frame as i64)?;
        let rc = unsafe { (self.ff.write_trailer)(self.fmt) };
        self.release();
        if rc < 0 {
            return Err(format!("no se pudo cerrar el MP4 ({rc})"));
        }
        Ok(())
    }

    fn encode(&mut self, ctx: *mut c_void, frame: *mut c_void, stream: i32, duration: i64) -> Result<(), String> {
        loop {
            let rc = unsafe { (self.ff.send_frame)(ctx, frame) };
            if rc == EAGAIN {
                self.drain(ctx, stream, duration)?;
                continue;
            }
            if rc < 0 {
                return Err(format!("el encoder rechazó un frame ({rc})"));
            }
            break;
        }
        self.drain(ctx, stream, duration)
    }

    fn drain(&mut self, ctx: *mut c_void, stream: i32, duration: i64) -> Result<(), String> {
        loop {
            let rc = unsafe { (self.ff.receive_packet)(ctx, self.packet) };
            if rc == EAGAIN || rc == EOF {
                return Ok(());
            }
            if rc < 0 {
                return Err(format!("el encoder no entregó el paquete ({rc})"));
            }
            unsafe {
                write_i32(self.packet, OFF_PKT_STREAM, stream);
                let pts = read_i64(self.packet, OFF_PKT_PTS);
                if pts == NOPTS {
                    let next = if stream == 0 { self.video_pts - 1 } else { self.audio_pts - duration };
                    write_i64(self.packet, OFF_PKT_PTS, next);
                    write_i64(self.packet, OFF_PKT_DTS, next);
                }
                if read_i64(self.packet, OFF_PKT_DURATION) == 0 {
                    write_i64(self.packet, OFF_PKT_DURATION, duration);
                }
                let wr = (self.ff.write_frame)(self.fmt, self.packet);
                (self.ff.packet_unref)(self.packet);
                if wr < 0 {
                    return Err(format!("no se pudo escribir el MP4 ({wr})"));
                }
            }
        }
    }

    fn release(&mut self) {
        unsafe {
            if !self.sws.is_null() {
                (self.ff.sws_free)(self.sws);
                self.sws = ptr::null_mut();
            }
            if !self.packet.is_null() {
                (self.ff.packet_free)(&mut self.packet);
            }
            if !self.vframe.is_null() {
                (self.ff.frame_free)(&mut self.vframe);
            }
            if !self.aframe.is_null() {
                (self.ff.frame_free)(&mut self.aframe);
            }
            if !self.fmt.is_null() {
                let mut pb = read_ptr(self.fmt, OFF_FMT_PB);
                if !pb.is_null() {
                    (self.ff.avio_closep)(&mut pb);
                }
                (self.ff.free_context_fmt)(self.fmt);
                self.fmt = ptr::null_mut();
            }
            if !self.video.is_null() {
                (self.ff.free_context)(&mut self.video);
            }
            if !self.audio.is_null() {
                (self.ff.free_context)(&mut self.audio);
            }
        }
    }
}

impl Drop for Mux {
    fn drop(&mut self) {
        if !self.fmt.is_null() {
            unsafe { (self.ff.write_trailer)(self.fmt) };
        }
        self.release();
    }
}

fn open_video(ff: &Ff, width: i32, height: i32) -> Result<(String, *mut c_void, i32), String> {
    let mut errors = Vec::new();
    for name in ENCODERS {
        let c_name = CString::new(*name).unwrap();
        let codec = unsafe { (ff.find_encoder)(c_name.as_ptr()) };
        if codec.is_null() {
            errors.push(format!("{name}: no está en este FFmpeg"));
            continue;
        }
        for pix in [NV12, YUV420P] {
            for global in [true, false] {
                let ctx = unsafe { (ff.alloc_context)(codec) };
                if ctx.is_null() {
                    continue;
                }
                unsafe {
                    write_i32(ctx, OFF_WIDTH, width);
                    write_i32(ctx, OFF_HEIGHT, height);
                    write_i32(ctx, OFF_PIX_FMT, pix);
                    write_i32(ctx, OFF_TIME_BASE, 1);
                    write_i32(ctx, OFF_TIME_BASE + 4, 30);
                    write_i32(ctx, OFF_FRAMERATE, 30);
                    write_i32(ctx, OFF_FRAMERATE + 4, 1);
                    write_i32(ctx, OFF_GOP, 30);
                    write_i64(ctx, OFF_BITRATE, 8_000_000);
                    if global {
                        let flags = read_i32(ctx, OFF_FLAGS);
                        write_i32(ctx, OFF_FLAGS, flags | GLOBAL_HEADER);
                    }
                    let rc = (ff.open)(ctx, codec, ptr::null_mut());
                    if rc >= 0 {
                        return Ok(((*name).to_string(), ctx, pix));
                    }
                    let mut ctx = ctx;
                    (ff.free_context)(&mut ctx);
                    errors.push(format!("{name} pix={pix} global={global}: {rc}"));
                }
            }
        }
    }
    Err(format!(
        "ningún encoder de hardware ni h264_mf pudo abrirse ({})",
        errors.join("; ")
    ))
}

fn open_audio(ff: &Ff) -> Result<*mut c_void, String> {
    let name = CString::new("aac").unwrap();
    let codec = unsafe { (ff.find_encoder)(name.as_ptr()) };
    if codec.is_null() {
        return Err("este FFmpeg no tiene el encoder AAC".into());
    }
    let ctx = unsafe { (ff.alloc_context)(codec) };
    if ctx.is_null() {
        return Err("no se pudo crear el encoder de audio".into());
    }
    unsafe {
        write_i32(ctx, OFF_SAMPLE_RATE, 48_000);
        write_i32(ctx, OFF_SAMPLE_FMT, FLTP);
        write_i32(ctx, OFF_TIME_BASE, 1);
        write_i32(ctx, OFF_TIME_BASE + 4, 48_000);
        write_i64(ctx, OFF_BITRATE, 128_000);
        (ff.channel_layout)(ctx.cast::<u8>().add(OFF_CH_LAYOUT).cast(), 2);
        let flags = read_i32(ctx, OFF_FLAGS);
        write_i32(ctx, OFF_FLAGS, flags | GLOBAL_HEADER);
        let rc = (ff.open)(ctx, codec, ptr::null_mut());
        if rc < 0 {
            let mut ctx = ctx;
            (ff.free_context)(&mut ctx);
            return Err(format!("no se pudo abrir el AAC ({rc})"));
        }
    }
    Ok(ctx)
}

fn copy_params(ff: &Ff, stream: *mut c_void, codec: *mut c_void) -> Result<(), String> {
    let par = unsafe { read_ptr(stream, OFF_STREAM_CODECPAR) };
    let rc = unsafe { (ff.params_from_context)(par, codec) };
    if rc < 0 {
        Err(format!("no se pudieron copiar los parámetros ({rc})"))
    } else {
        Ok(())
    }
}

fn free_pair(ff: &Ff, mut video: *mut c_void, mut audio: *mut c_void) {
    unsafe {
        (ff.free_context)(&mut video);
        (ff.free_context)(&mut audio);
    }
}

unsafe fn read_ptr(base: *mut c_void, offset: usize) -> *mut c_void {
    *base.cast::<u8>().add(offset).cast::<*mut c_void>()
}

unsafe fn read_i32(base: *mut c_void, offset: usize) -> i32 {
    *base.cast::<u8>().add(offset).cast::<i32>()
}

unsafe fn read_i64(base: *mut c_void, offset: usize) -> i64 {
    *base.cast::<u8>().add(offset).cast::<i64>()
}

unsafe fn write_ptr(base: *mut c_void, offset: usize, value: *mut c_void) {
    *base.cast::<u8>().add(offset).cast::<*mut c_void>() = value;
}

unsafe fn write_i32(base: *mut c_void, offset: usize, value: i32) {
    *base.cast::<u8>().add(offset).cast::<i32>() = value;
}

unsafe fn write_i64(base: *mut c_void, offset: usize, value: i64) {
    *base.cast::<u8>().add(offset).cast::<i64>() = value;
}
