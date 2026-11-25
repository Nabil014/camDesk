//! H.264 Annex-B a BGRA ya rotado, y el lienzo NV12 1920x1080.
//! FFmpeg se carga en `Decoder::new()`. Este crate no pinta, no graba y no abre sockets.

use std::path::PathBuf;

use libloading::Library;
use protocol::{Frame, FrameKind, Rotation};

pub const CANVAS_W: u32 = 1920;
pub const CANVAS_H: u32 = 1080;

const AV_CODEC_ID_H264: i32 = 27;
const AV_PIX_FMT_NV12: i32 = 23;
const AV_PIX_FMT_BGRA: i32 = 28;
const SWS_CS_ITU709: i32 = 1;
const SWS_BICUBIC: i32 = 4;
const SWS_FULL_CHR_H_INP: i32 = 1 << 14;
const SWS_PLANE_PADDING: usize = 64;
const AVERROR_EAGAIN: i32 = -11;
const AVERROR_EOF: i32 = -541_478_725;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
    pub pts_us: i64,
}

#[derive(Debug)]
pub enum DecodeError {
    NeedKeyframe,
    Message(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::NeedKeyframe => write!(f, "falta un keyframe"),
            DecodeError::Message(text) => write!(f, "{text}"),
        }
    }
}

impl std::error::Error for DecodeError {}

type FnFindDecoder = unsafe extern "C" fn(i32) -> *mut std::ffi::c_void;
type FnAllocContext = unsafe extern "C" fn(*mut std::ffi::c_void) -> *mut std::ffi::c_void;
type FnOpen = unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void, *mut *mut std::ffi::c_void) -> i32;
type FnSend = unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void) -> i32;
type FnReceive = unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void) -> i32;
type FnFreeContext = unsafe extern "C" fn(*mut *mut std::ffi::c_void);
type FnPacketAlloc = unsafe extern "C" fn() -> *mut std::ffi::c_void;
type FnPacketFree = unsafe extern "C" fn(*mut *mut std::ffi::c_void);
type FnNewPacket = unsafe extern "C" fn(*mut std::ffi::c_void, i32) -> i32;
type FnFlush = unsafe extern "C" fn(*mut std::ffi::c_void);
type FnFrameAlloc = unsafe extern "C" fn() -> *mut std::ffi::c_void;
type FnFrameFree = unsafe extern "C" fn(*mut *mut std::ffi::c_void);
type FnFrameUnref = unsafe extern "C" fn(*mut std::ffi::c_void);
type FnSwsAlloc = unsafe extern "C" fn() -> *mut std::ffi::c_void;
type FnSwsScaleFrame = unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void, *const std::ffi::c_void) -> i32;
type FnSwsCoefficients = unsafe extern "C" fn(i32) -> *const i32;
type FnSwsSetColorspace = unsafe extern "C" fn(*mut std::ffi::c_void, *const i32, i32, *const i32, i32, i32, i32, i32) -> i32;
type FnSwsGetCached = unsafe extern "C" fn(
    *mut std::ffi::c_void, i32, i32, i32, i32, i32, i32, i32,
    *mut std::ffi::c_void, *mut std::ffi::c_void, *const f64,
) -> *mut std::ffi::c_void;
type FnSwsScale = unsafe extern "C" fn(*mut std::ffi::c_void, *const *const u8, *const i32, i32, i32, *mut *mut u8, *const i32) -> i32;
type FnSwsFree = unsafe extern "C" fn(*mut std::ffi::c_void);

struct Ffmpeg {
    _avutil: Library,
    _swresample: Library,
    _swscale: Library,
    _avcodec: Library,
    find_decoder: FnFindDecoder,
    alloc_context: FnAllocContext,
    open: FnOpen,
    send: FnSend,
    receive: FnReceive,
    free_context: FnFreeContext,
    packet_alloc: FnPacketAlloc,
    packet_free: FnPacketFree,
    new_packet: FnNewPacket,
    flush: FnFlush,
    frame_alloc: FnFrameAlloc,
    frame_free: FnFrameFree,
    frame_unref: FnFrameUnref,
    sws_alloc: FnSwsAlloc,
    sws_scale_frame: FnSwsScaleFrame,
    sws_coefficients: FnSwsCoefficients,
    sws_colorspace: FnSwsSetColorspace,
    sws_cached: FnSwsGetCached,
    sws_scale: FnSwsScale,
    sws_free: FnSwsFree,
}

fn sym<T: Copy>(lib: &Library, name: &[u8]) -> Result<T, DecodeError> {
    unsafe {
        lib.get(name)
            .map(|symbol| *symbol)
            .map_err(|err| DecodeError::Message(format!("{}: {err}", String::from_utf8_lossy(name))))
    }
}

fn ffmpeg_dir() -> Result<PathBuf, DecodeError> {
    if let Some(dir) = std::env::var_os("CAMDESK_FFMPEG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if dir.join("avcodec-62.dll").is_file() {
                return Ok(dir.to_path_buf());
            }
        }
    }
    Err(DecodeError::Message(
        "no están las DLL LGPL de FFmpeg (CAMDESK_FFMPEG_DIR o junto al exe)".into(),
    ))
}

fn load_ffmpeg() -> Result<Ffmpeg, DecodeError> {
    let dir = ffmpeg_dir()?;
    let dll = |name: &str| -> Result<Library, DecodeError> {
        let path = dir.join(name);
        unsafe { Library::new(&path) }.map_err(|err| DecodeError::Message(format!("{}: {err}", path.display())))
    };
    let avutil = dll("avutil-60.dll")?;
    let swresample = dll("swresample-6.dll")?;
    let swscale = dll("swscale-9.dll")?;
    let avcodec = dll("avcodec-62.dll")?;
    Ok(Ffmpeg {
        find_decoder: sym(&avcodec, b"avcodec_find_decoder")?,
        alloc_context: sym(&avcodec, b"avcodec_alloc_context3")?,
        open: sym(&avcodec, b"avcodec_open2")?,
        send: sym(&avcodec, b"avcodec_send_packet")?,
        receive: sym(&avcodec, b"avcodec_receive_frame")?,
        free_context: sym(&avcodec, b"avcodec_free_context")?,
        packet_alloc: sym(&avcodec, b"av_packet_alloc")?,
        packet_free: sym(&avcodec, b"av_packet_free")?,
        new_packet: sym(&avcodec, b"av_new_packet")?,
        flush: sym(&avcodec, b"avcodec_flush_buffers")?,
        frame_alloc: sym(&avutil, b"av_frame_alloc")?,
        frame_free: sym(&avutil, b"av_frame_free")?,
        frame_unref: sym(&avutil, b"av_frame_unref")?,
        sws_alloc: sym(&swscale, b"sws_alloc_context")?,
        sws_scale_frame: sym(&swscale, b"sws_scale_frame")?,
        sws_coefficients: sym(&swscale, b"sws_getCoefficients")?,
        sws_colorspace: sym(&swscale, b"sws_setColorspaceDetails")?,
        sws_cached: sym(&swscale, b"sws_getCachedContext")?,
        sws_scale: sym(&swscale, b"sws_scale")?,
        sws_free: sym(&swscale, b"sws_freeContext")?,
        _avutil: avutil,
        _swresample: swresample,
        _swscale: swscale,
        _avcodec: avcodec,
    })
}

pub struct Decoder {
    ff: Ffmpeg,
    ctx: *mut std::ffi::c_void,
    frame: *mut std::ffi::c_void,
    sws: *mut std::ffi::c_void,
    bgra_frame: *mut std::ffi::c_void,
    canvas_sws: *mut std::ffi::c_void,
    canvas_input: Vec<u8>,
    canvas_output: Vec<u8>,
    seen_idr: bool,
    codec_config: Vec<u8>,
}

unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new() -> Result<Self, DecodeError> {
        let ff = load_ffmpeg()?;
        let codec = unsafe { (ff.find_decoder)(AV_CODEC_ID_H264) };
        if codec.is_null() {
            return Err(DecodeError::Message("FFmpeg no tiene el decoder H.264".into()));
        }
        let ctx = unsafe { (ff.alloc_context)(codec) };
        if ctx.is_null() {
            return Err(DecodeError::Message("no se pudo crear el contexto de decode".into()));
        }
        let rc = unsafe { (ff.open)(ctx, codec, std::ptr::null_mut()) };
        if rc < 0 {
            let mut ctx = ctx;
            unsafe { (ff.free_context)(&mut ctx) };
            return Err(DecodeError::Message(format!("avcodec_open2 devolvió {rc}")));
        }
        let frame = unsafe { (ff.frame_alloc)() };
        if frame.is_null() {
            let mut ctx = ctx;
            unsafe { (ff.free_context)(&mut ctx) };
            return Err(DecodeError::Message("no se pudo crear el frame".into()));
        }
        Ok(Self {
            ff,
            ctx,
            frame,
            sws: std::ptr::null_mut(),
            bgra_frame: std::ptr::null_mut(),
            canvas_sws: std::ptr::null_mut(),
            canvas_input: Vec::new(),
            canvas_output: Vec::new(),
            seen_idr: false,
            codec_config: Vec::new(),
        })
    }

    pub fn reset(&mut self) {
        unsafe { (self.ff.flush)(self.ctx) };
        self.seen_idr = false;
    }

    pub fn push(&mut self, frame: &Frame) -> Result<Option<Decoded>, DecodeError> {
        match frame.header.kind {
            FrameKind::Audio | FrameKind::Control | FrameKind::Other(_) => return Ok(None),
            FrameKind::CodecConfig => {
                if !frame.payload.is_empty() {
                    self.codec_config = frame.payload.clone();
                }
                return Ok(None);
            }
            FrameKind::Video => {}
        }
        let idr = frame.header.keyframe || annexb_has_idr(&frame.payload);
        if !self.seen_idr && !idr {
            return Err(DecodeError::NeedKeyframe);
        }
        let combined;
        let bytes = if !annexb_has_nal(&frame.payload, 7) && !self.codec_config.is_empty() {
            combined = [self.codec_config.as_slice(), frame.payload.as_slice()].concat();
            combined.as_slice()
        } else {
            frame.payload.as_slice()
        };
        self.feed(bytes, frame.header.pts_us)?;
        let picture = self.pull()?;
        if picture.is_none() {
            if !self.seen_idr {
                return Err(DecodeError::NeedKeyframe);
            }
            return Ok(None);
        }
        self.seen_idr = true;
        let (w, h, bgra) = picture.unwrap();
        let (bgra, w, h) = rotate_bgra(&bgra, w, h, frame.header.rotation);
        Ok(Some(Decoded {
            width: w,
            height: h,
            bgra,
            pts_us: frame.header.pts_us,
        }))
    }

    /// Escala el BGRA ya rotado a 1920x1080 NV12 con barras (Y=16, UV=128).
    pub fn to_canvas_nv12(&mut self, decoded: &Decoded, y: &mut [u8], uv: &mut [u8], stride: usize) {
        let rows = CANVAS_H as usize;
        let uv_rows = rows / 2;
        if stride < CANVAS_W as usize || stride > y.len() / rows || stride > uv.len() / uv_rows {
            return;
        }
        for row in 0..rows {
            y[row * stride..row * stride + CANVAS_W as usize].fill(16);
        }
        for row in 0..uv_rows {
            uv[row * stride..row * stride + CANVAS_W as usize].fill(128);
        }
        let Some((x0, y0, dst_w, dst_h)) = fit_canvas(decoded.width, decoded.height) else {
            return;
        };
        let Some(src_stride) = i32::try_from(decoded.width).ok().and_then(|width| width.checked_mul(4)) else {
            return;
        };
        let Ok(src_height) = i32::try_from(decoded.height) else { return };
        let Some(src_len) = (src_stride as usize).checked_mul(src_height as usize) else { return };
        let Some(pixels) = decoded.bgra.get(..src_len) else { return };
        let Some(padded_len) = src_len.checked_add(SWS_PLANE_PADDING) else { return };

        self.canvas_sws = unsafe {
            (self.ff.sws_cached)(
                self.canvas_sws,
                src_stride / 4,
                src_height,
                AV_PIX_FMT_BGRA,
                dst_w as i32,
                dst_h as i32,
                AV_PIX_FMT_NV12,
                SWS_BICUBIC | SWS_FULL_CHR_H_INP,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if self.canvas_sws.is_null() {
            return;
        }

        let color_status = unsafe {
            let bt709 = (self.ff.sws_coefficients)(SWS_CS_ITU709);
            // BGRA de rango completo a NV12 BT.709 limitado. Contraste y saturación 1.0 en 16.16.
            (self.ff.sws_colorspace)(self.canvas_sws, bt709, 1, bt709, 0, 0, 1 << 16, 1 << 16)
        };
        if color_status < 0 {
            return;
        }

        // Mantener padding para libswscale y copiar solo el rectángulo visible al lienzo.
        self.canvas_input.resize(padded_len, 0);
        self.canvas_input[..src_len].copy_from_slice(pixels);
        self.canvas_input[src_len..].fill(0);
        let dst_w = dst_w as usize;
        let dst_h = dst_h as usize;
        let output_stride = dst_w.next_multiple_of(64);
        let output_y_len = output_stride * dst_h;
        let uv_offset = output_y_len + SWS_PLANE_PADDING;
        self.canvas_output.resize(uv_offset + output_y_len / 2 + SWS_PLANE_PADDING, 0);
        let mut output = [
            self.canvas_output.as_mut_ptr(),
            self.canvas_output[uv_offset..].as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ];
        let input = [self.canvas_input.as_ptr(), std::ptr::null(), std::ptr::null(), std::ptr::null()];
        let output_stride_i32 = output_stride as i32;
        let scaled = unsafe {
            (self.ff.sws_scale)(
                self.canvas_sws,
                input.as_ptr(),
                [src_stride, 0, 0, 0].as_ptr(),
                0,
                src_height,
                output.as_mut_ptr(),
                [output_stride_i32, output_stride_i32, 0, 0].as_ptr(),
            )
        };
        if scaled != dst_h as i32 {
            return;
        }
        for (plane, top, offset, height) in [
            (y, y0 as usize, 0, dst_h),
            (uv, y0 as usize / 2, uv_offset, dst_h / 2),
        ] {
            for row in 0..height {
                let from = offset + row * output_stride;
                let to = (top + row) * stride + x0 as usize;
                plane[to..to + dst_w].copy_from_slice(&self.canvas_output[from..from + dst_w]);
            }
        }
    }

    fn feed(&mut self, bytes: &[u8], pts: i64) -> Result<(), DecodeError> {
        if bytes.is_empty() {
            return Ok(());
        }
        let packet = unsafe { (self.ff.packet_alloc)() };
        if packet.is_null() {
            return Err(DecodeError::Message("no se pudo crear el paquete".into()));
        }
        let rc = unsafe { (self.ff.new_packet)(packet, bytes.len() as i32) };
        if rc < 0 {
            let mut packet = packet;
            unsafe { (self.ff.packet_free)(&mut packet) };
            return Err(DecodeError::Message(format!("av_new_packet devolvió {rc}")));
        }
        unsafe {
            let data = read_ptr(packet, 24);
            let size = read_i32(packet, 32);
            if size != bytes.len() as i32 || data.is_null() {
                let mut packet = packet;
                (self.ff.packet_free)(&mut packet);
                return Err(DecodeError::Message(format!(
                    "el paquete no quedó usable (size={size}, data nulo={})",
                    data.is_null()
                )));
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
            write_i64(packet, 8, pts);
            let send = (self.ff.send)(self.ctx, packet);
            let mut packet = packet;
            (self.ff.packet_free)(&mut packet);
            if send < 0 && send != AVERROR_EAGAIN {
                return Err(DecodeError::Message(format!("avcodec_send_packet devolvió {send}")));
            }
        }
        Ok(())
    }

    fn pull(&mut self) -> Result<Option<(u32, u32, Vec<u8>)>, DecodeError> {
        let rc = unsafe { (self.ff.receive)(self.ctx, self.frame) };
        if rc == AVERROR_EAGAIN || rc == AVERROR_EOF {
            return Ok(None);
        }
        if rc < 0 {
            return Err(DecodeError::Message(format!("avcodec_receive_frame devolvió {rc}")));
        }
        let width = unsafe { read_i32(self.frame, 104) };
        let height = unsafe { read_i32(self.frame, 108) };
        if width <= 0 || height <= 0 {
            unsafe { (self.ff.frame_unref)(self.frame) };
            return Ok(None);
        }
        if self.sws.is_null() {
            self.sws = unsafe { (self.ff.sws_alloc)() };
        }
        if self.bgra_frame.is_null() {
            self.bgra_frame = unsafe { (self.ff.frame_alloc)() };
        }
        if self.sws.is_null() || self.bgra_frame.is_null() {
            return Err(DecodeError::Message("no se pudo preparar la conversión a BGRA".into()));
        }

        // libswscale usa los metadatos de color del AVFrame de entrada en cada conversión.
        let scaled = unsafe {
            (self.ff.frame_unref)(self.bgra_frame);
            write_i32(self.bgra_frame, 104, width);
            write_i32(self.bgra_frame, 108, height);
            write_i32(self.bgra_frame, 116, AV_PIX_FMT_BGRA);
            (self.ff.sws_scale_frame)(self.sws, self.bgra_frame, self.frame)
        };
        unsafe { (self.ff.frame_unref)(self.frame) };
        if scaled < 0 {
            return Err(DecodeError::Message(format!("sws_scale_frame devolvió {scaled}")));
        }

        let plane = unsafe { read_ptr(self.bgra_frame, 0) };
        let stride = unsafe { read_i32(self.bgra_frame, 64) } as isize;
        let row_bytes = width as usize * 4;
        let mut bgra = Vec::with_capacity(row_bytes * height as usize);
        for row in 0..height as isize {
            let pixels = unsafe { std::slice::from_raw_parts(plane.offset(row * stride), row_bytes) };
            bgra.extend_from_slice(pixels);
        }
        Ok(Some((width as u32, height as u32, bgra)))
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        if !self.sws.is_null() {
            unsafe { (self.ff.sws_free)(self.sws) };
        }
        if !self.canvas_sws.is_null() {
            unsafe { (self.ff.sws_free)(self.canvas_sws) };
        }
        if !self.bgra_frame.is_null() {
            unsafe { (self.ff.frame_free)(&mut self.bgra_frame) };
        }
        if !self.frame.is_null() {
            let mut frame = self.frame;
            unsafe { (self.ff.frame_free)(&mut frame) };
        }
        if !self.ctx.is_null() {
            let mut ctx = self.ctx;
            unsafe { (self.ff.free_context)(&mut ctx) };
        }
    }
}

fn rotate_bgra(src: &[u8], w: u32, h: u32, rotation: Rotation) -> (Vec<u8>, u32, u32) {
    let w = w as usize;
    let h = h as usize;
    if src.len() < w * h * 4 {
        return (src.to_vec(), w as u32, h as u32);
    }
    match rotation {
        Rotation::R0 => (src.to_vec(), w as u32, h as u32),
        Rotation::R90 => {
            let mut dst = vec![0u8; src.len().max(w * h * 4)];
            for y in 0..h {
                for x in 0..w {
                    let dx = h - 1 - y;
                    let dy = x;
                    copy_px(src, w, x, y, &mut dst, h, dx, dy);
                }
            }
            (dst, h as u32, w as u32)
        }
        Rotation::R180 => {
            let mut dst = vec![0u8; w * h * 4];
            for y in 0..h {
                for x in 0..w {
                    copy_px(src, w, x, y, &mut dst, w, w - 1 - x, h - 1 - y);
                }
            }
            (dst, w as u32, h as u32)
        }
        Rotation::R270 => {
            let mut dst = vec![0u8; w * h * 4];
            for y in 0..h {
                for x in 0..w {
                    copy_px(src, w, x, y, &mut dst, h, y, w - 1 - x);
                }
            }
            (dst, h as u32, w as u32)
        }
    }
}

fn copy_px(src: &[u8], src_w: usize, x: usize, y: usize, dst: &mut [u8], dst_w: usize, dx: usize, dy: usize) {
    let from = (y * src_w + x) * 4;
    let to = (dy * dst_w + dx) * 4;
    dst[to..to + 4].copy_from_slice(&src[from..from + 4]);
}

/// Devuelve (x, y, ancho, alto) pares para que la imagen entre completa en 1920x1080.
fn fit_canvas(src_w: u32, src_h: u32) -> Option<(u32, u32, u32, u32)> {
    if src_w == 0 || src_h == 0 {
        return None;
    }
    let scale = (CANVAS_W as f32 / src_w as f32).min(CANVAS_H as f32 / src_h as f32);
    let dst_w = ((src_w as f32 * scale).round() as u32).min(CANVAS_W) & !1;
    let dst_h = ((src_h as f32 * scale).round() as u32).min(CANVAS_H) & !1;
    if dst_w < 2 || dst_h < 2 {
        return None;
    }
    let x0 = ((CANVAS_W - dst_w) / 2) & !1;
    let y0 = ((CANVAS_H - dst_h) / 2) & !1;
    Some((x0, y0, dst_w, dst_h))
}

fn annexb_has_idr(bytes: &[u8]) -> bool {
    annexb_has_nal(bytes, 5)
}

fn annexb_has_nal(bytes: &[u8], nal_type: u8) -> bool {
    nal_types(bytes).contains(&nal_type)
}

fn nal_types(bytes: &[u8]) -> Vec<u8> {
    let mut types = Vec::new();
    let mut i = 0;
    while i + 3 < bytes.len() {
        let start = if bytes[i..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if bytes[i..].starts_with(&[0, 0, 1]) {
            3
        } else {
            i += 1;
            continue;
        };
        if i + start < bytes.len() {
            types.push(bytes[i + start] & 0x1f);
        }
        i += start;
    }
    types
}

#[cfg(test)]
fn split_annexb(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if bytes[i..].starts_with(&[0, 0, 0, 1]) || bytes[i..].starts_with(&[0, 0, 1]) {
            starts.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut nals = Vec::new();
    for (n, start) in starts.iter().enumerate() {
        let end = starts.get(n + 1).copied().unwrap_or(bytes.len());
        nals.push(bytes[*start..end].to_vec());
    }
    nals
}

unsafe fn read_ptr(base: *mut std::ffi::c_void, offset: usize) -> *mut u8 {
    *base.cast::<u8>().add(offset).cast::<*mut u8>()
}

unsafe fn read_i32(base: *mut std::ffi::c_void, offset: usize) -> i32 {
    *base.cast::<u8>().add(offset).cast::<i32>()
}

unsafe fn write_i32(base: *mut std::ffi::c_void, offset: usize, value: i32) {
    *base.cast::<u8>().add(offset).cast::<i32>() = value;
}

unsafe fn write_i64(base: *mut std::ffi::c_void, offset: usize, value: i64) {
    *base.cast::<u8>().add(offset).cast::<i64>() = value;
}

#[cfg(test)]
fn ffmpeg_bin() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../third_party/ffmpeg-n8.1-latest-win64-lgpl-shared-8.1/bin")
}

#[cfg(test)]
fn video_frame(payload: Vec<u8>, rotation: Rotation, keyframe: bool) -> Frame {
    Frame {
        header: protocol::FrameHeader {
            kind: FrameKind::Video,
            keyframe,
            rotation,
            pts_us: 0,
            len: payload.len() as u32,
        },
        payload,
    }
}

#[test]
fn el_720p_llena_el_lienzo_y_el_vertical_deja_barras() {
    assert_eq!(fit_canvas(1280, 720), Some((0, 0, 1920, 1080)));
    assert_eq!(fit_canvas(1920, 1080), Some((0, 0, 1920, 1080)));
    let (x, y, w, h) = fit_canvas(720, 1280).expect("vertical");
    assert_eq!((y, h), (0, 1080));
    assert!((576..=640).contains(&w), "ancho vertical {w}");
    assert!(x > 0 && x + w <= CANVAS_W);
    assert_eq!(fit_canvas(0, 720), None);
}

#[test]
fn annexb_rota_y_deja_barras() {
    let bin = ffmpeg_bin();
    if !bin.join("avcodec-62.dll").is_file() {
        panic!("falta el FFmpeg LGPL en {}", bin.display());
    }
    std::env::set_var("CAMDESK_FFMPEG_DIR", &bin);
    let annexb = include_bytes!("../fixtures/red-32x16.annexb");
    let nals = split_annexb(annexb);
    assert!(nals.len() >= 3, "el fixture no trae SPS, PPS e IDR");
    let mut decoder = Decoder::new().unwrap();
    let config = video_frame(nals[..nals.len() - 1].concat(), Rotation::R0, false);
    let mut config = config;
    config.header.kind = FrameKind::CodecConfig;
    assert!(decoder.push(&config).unwrap().is_none());
    let early = video_frame(vec![0, 0, 0, 1, 0x41, 0x88, 0x84], Rotation::R0, false);
    assert!(matches!(decoder.push(&early), Err(DecodeError::NeedKeyframe)));

    for (rotation, w, h) in [
        (Rotation::R0, 32, 16),
        (Rotation::R90, 16, 32),
        (Rotation::R180, 32, 16),
        (Rotation::R270, 16, 32),
    ] {
        decoder.reset();
        let picture = decoder
            .push(&video_frame(annexb.to_vec(), rotation, true))
            .unwrap()
            .expect("el IDR tiene que salir");
        assert_eq!((picture.width, picture.height), (w, h));
        let mid = ((h as usize / 2) * w as usize + (w as usize / 2)) * 4;
        let b = picture.bgra[mid];
        let g = picture.bgra[mid + 1];
        let r = picture.bgra[mid + 2];
        assert!(r > 180 && g < 80 && b < 80, "no es rojo: {b},{g},{r} rot={rotation:?}");
        let mut y = vec![0u8; CANVAS_W as usize * CANVAS_H as usize];
        let mut uv = vec![0u8; CANVAS_W as usize * CANVAS_H as usize / 2];
        decoder.to_canvas_nv12(&picture, &mut y, &mut uv, CANVAS_W as usize);
        assert_eq!(y[0], 16, "la barra tiene que ser Y=16");
        let center = (CANVAS_H as usize / 2) * CANVAS_W as usize + CANVAS_W as usize / 2;
        assert_ne!(y[center], 16, "el centro no puede ser la barra");
    }
    decoder.reset();
    let only_idr = decoder
        .push(&video_frame(nals.last().unwrap().clone(), Rotation::R0, true))
        .unwrap()
        .expect("el IDR usa el SPS guardado");
    assert_eq!((only_idr.width, only_idr.height), (32, 16));

    let mut edge_row = [0, 0, 0, 255].repeat(640);
    edge_row.extend_from_slice(&[255, 255, 255, 255].repeat(640));
    let contrast_edge = Decoded {
        width: 1280,
        height: 720,
        bgra: edge_row.repeat(720),
        pts_us: 0,
    };
    let stride = CANVAS_W as usize + 16;
    let mut y = vec![0; stride * CANVAS_H as usize];
    let mut uv = vec![0; stride * CANVAS_H as usize / 2];
    decoder.to_canvas_nv12(&contrast_edge, &mut y, &mut uv, stride);
    let center = stride * (CANVAS_H as usize / 2) + CANVAS_W as usize / 2;
    assert!((17..235).contains(&y[center]), "el borde escalado debe tener luminancia intermedia: {}", y[center]);
    assert_eq!(y[0], 16, "el negro mantiene el rango limitado");
    assert_eq!(uv[0], 128);
    assert_eq!(y[CANVAS_W as usize], 0, "el padding queda fuera del lienzo");
}

#[test]
fn respeta_bt709_en_preview_y_lienzo() {
    std::env::set_var("CAMDESK_FFMPEG_DIR", ffmpeg_bin());
    let mut decoder = Decoder::new().unwrap();
    let frame = video_frame(include_bytes!("../fixtures/red-bt709-32x16.annexb").to_vec(), Rotation::R0, true);
    let image = decoder.push(&frame).unwrap().unwrap();
    let center = (8 * 32 + 16) * 4;
    let bgra = &image.bgra[center..center + 4];
    // El mismo H.264 interpretado como BT.601 da verde 0 en vez de 23.
    assert!(bgra[0] <= 2 && bgra[1].abs_diff(23) <= 2 && bgra[2] >= 253);

    let stride = CANVAS_W as usize;
    let mut y = vec![0; stride * CANVAS_H as usize];
    let mut uv = vec![0; y.len() / 2];
    decoder.to_canvas_nv12(&image, &mut y, &mut uv, stride);
    let luma = 540 * stride + 960;
    let chroma = 270 * stride + 960;
    assert!(y[luma].abs_diff(77) <= 2);
    assert!(uv[chroma].abs_diff(95) <= 2);
    assert!(uv[chroma + 1].abs_diff(231) <= 2);
}

#[test]
fn la_rotacion_es_horaria_y_no_espeja() {
    // 3x2, cada píxel con su propio azul:  A B C
    //                                      D E F
    let ids = [1u8, 2, 3, 4, 5, 6];
    let src: Vec<u8> = ids.iter().flat_map(|id| [*id, 0, 0, 255]).collect();
    let ids_of = |bgra: &[u8]| bgra.chunks_exact(4).map(|px| px[0]).collect::<Vec<u8>>();

    let (out, w, h) = rotate_bgra(&src, 3, 2, Rotation::R0);
    assert_eq!((w, h, ids_of(&out)), (3, 2, vec![1, 2, 3, 4, 5, 6]));
    // 90° horario: la columna izquierda (A, D) pasa a ser la fila de arriba, leída de abajo hacia arriba.
    let (out, w, h) = rotate_bgra(&src, 3, 2, Rotation::R90);
    assert_eq!((w, h, ids_of(&out)), (2, 3, vec![4, 1, 5, 2, 6, 3]));
    let (out, w, h) = rotate_bgra(&src, 3, 2, Rotation::R180);
    assert_eq!((w, h, ids_of(&out)), (3, 2, vec![6, 5, 4, 3, 2, 1]));
    // 270° horario = 90° antihorario: la columna derecha (C, F) pasa a ser la fila de arriba.
    let (out, w, h) = rotate_bgra(&src, 3, 2, Rotation::R270);
    assert_eq!((w, h, ids_of(&out)), (2, 3, vec![3, 6, 2, 5, 1, 4]));
}
