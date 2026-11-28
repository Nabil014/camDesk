//! FFI al runtime NVIDIA Video Effects 0.7 que instaló el usuario.
//! `Engine`: `GreenScreen` escribe la máscara y `BackgroundBlur` la aplica.
//! `Cleanup`: `ArtifactReduction` quita los bloques de H.264 y `Denoising` el ruido.
//! Las imágenes de GPU viven en un `Box` para que el puntero que guarda el efecto no se mueva.

use std::ffi::{CStr, CString};
use std::mem;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, SetDllDirectoryW, LOAD_WITH_ALTERED_SEARCH_PATH,
};
use windows::core::{PCSTR, PCWSTR};

use super::{runtime_dll, FxError};

type Handle = *mut std::ffi::c_void;

const NVCV_A: i32 = 2;
const NVCV_BGR: i32 = 5;
const NVCV_BGRA: i32 = 7;
const NVCV_U8: i32 = 1;
const NVCV_F32: i32 = 7;
const NVCV_CHUNKY: u32 = 0;
const NVCV_PLANAR: u32 = 1;
const NVCV_CPU: u32 = 0;
const NVCV_GPU: u32 = 1;
const MODE_QUALITY: u32 = 0;
const MODE_PERFORMANCE: u32 = 1;

const FRAME_BUDGET: Duration = Duration::from_millis(34);
const SLOW_FRAMES: u32 = 12;

#[repr(C)]
struct NvImage {
    width: u32,
    height: u32,
    pitch: i32,
    pixel_format: i32,
    component_type: i32,
    pixel_bytes: u8,
    component_bytes: u8,
    num_components: u8,
    planar: u8,
    gpu_mem: u8,
    colorspace: u8,
    reserved: [u8; 2],
    _pad: u32,
    pixels: *mut std::ffi::c_void,
    delete_ptr: *mut std::ffi::c_void,
    delete_proc: *mut std::ffi::c_void,
    buffer_bytes: u64,
}

const _: () = assert!(std::mem::size_of::<NvImage>() == 64);

impl NvImage {
    fn zero() -> Self {
        Self {
            width: 0,
            height: 0,
            pitch: 0,
            pixel_format: 0,
            component_type: 0,
            pixel_bytes: 0,
            component_bytes: 0,
            num_components: 0,
            planar: 0,
            gpu_mem: 0,
            colorspace: 0,
            reserved: [0; 2],
            _pad: 0,
            pixels: ptr::null_mut(),
            delete_ptr: ptr::null_mut(),
            delete_proc: ptr::null_mut(),
            buffer_bytes: 0,
        }
    }
}

type CreateEffect = unsafe extern "C" fn(*const i8, *mut Handle) -> i32;
type DestroyEffect = unsafe extern "C" fn(Handle) -> i32;
type SetU32 = unsafe extern "C" fn(Handle, *const i8, u32) -> i32;
type GetU32 = unsafe extern "C" fn(Handle, *const i8, *mut u32) -> i32;
type SetF32 = unsafe extern "C" fn(Handle, *const i8, f32) -> i32;
type SetImage = unsafe extern "C" fn(Handle, *const i8, *mut NvImage) -> i32;
type SetString = unsafe extern "C" fn(Handle, *const i8, *const i8) -> i32;
type SetStream = unsafe extern "C" fn(Handle, *const i8, Handle) -> i32;
type SetObject = unsafe extern "C" fn(Handle, *const i8, *mut std::ffi::c_void) -> i32;
type LoadFx = unsafe extern "C" fn(Handle) -> i32;
type RunFx = unsafe extern "C" fn(Handle, i32) -> i32;
type StreamCreate = unsafe extern "C" fn(*mut Handle) -> i32;
type StreamDestroy = unsafe extern "C" fn(Handle) -> i32;
type StreamSync = unsafe extern "C" fn(Handle) -> i32;
type InitImage = unsafe extern "C" fn(
    *mut NvImage,
    u32,
    u32,
    i32,
    *mut std::ffi::c_void,
    i32,
    i32,
    u32,
    u32,
) -> i32;
type AllocImage = unsafe extern "C" fn(*mut NvImage, u32, u32, i32, i32, u32, u32, u32) -> i32;
type DeallocImage = unsafe extern "C" fn(*mut NvImage) -> i32;
type Transfer = unsafe extern "C" fn(
    *const NvImage,
    *mut NvImage,
    f32,
    *mut std::ffi::c_void,
    *mut NvImage,
) -> i32;
type ErrorString = unsafe extern "C" fn(i32) -> *const i8;
type CudaMalloc = unsafe extern "C" fn(*mut *mut std::ffi::c_void, usize) -> i32;
type CudaMemset = unsafe extern "C" fn(*mut std::ffi::c_void, i32, usize) -> i32;
type CudaFree = unsafe extern "C" fn(*mut std::ffi::c_void) -> i32;

#[derive(Clone, Copy)]
struct Api {
    create: CreateEffect,
    destroy: DestroyEffect,
    set_u32: SetU32,
    get_u32: GetU32,
    set_f32: SetF32,
    set_image: SetImage,
    set_string: SetString,
    set_stream: SetStream,
    set_object: SetObject,
    load: LoadFx,
    run: RunFx,
    stream_create: StreamCreate,
    stream_destroy: StreamDestroy,
    stream_sync: StreamSync,
    init: InitImage,
    alloc: AllocImage,
    dealloc: DeallocImage,
    transfer: Transfer,
    err: ErrorString,
}

/// Las DLL del runtime, ya cargadas. Cada efecto tiene la suya; `LoadLibrary` cuenta referencias.
struct Runtime {
    cv: HMODULE,
    vfx: HMODULE,
    api: Api,
    model_dir: CString,
    dir: std::path::PathBuf,
    _dll_dir: DllDir,
}

impl Runtime {
    fn open() -> Result<Self, FxError> {
        let dll = runtime_dll().ok_or_else(|| FxError {
            message: "falta el runtime de NVIDIA Video Effects".into(),
        })?;
        let dir = dll.parent().ok_or_else(|| FxError {
            message: "el runtime de NVIDIA Video Effects no tiene carpeta".into(),
        })?;
        let models = dir.join("models");
        if !models.is_dir() {
            return Err(FxError { message: "no está la carpeta de modelos de NVIDIA Video Effects".into() });
        }
        let model_dir = CString::new(models.to_string_lossy().as_bytes()).map_err(|_| FxError {
            message: "la ruta de los modelos no se puede pasar al runtime".into(),
        })?;
        // Queda puesto mientras viva el runtime: TensorRT carga DLL tarde.
        let dll_dir = DllDir::set(dir)?;
        let cv = load_library(&dir.join("NVCVImage.dll"))?;
        let vfx = match load_library(&dll) {
            Ok(module) => module,
            Err(error) => {
                unsafe { let _ = FreeLibrary(cv); }
                return Err(error);
            }
        };
        let api = match resolve(cv, vfx) {
            Ok(api) => api,
            Err(error) => {
                unsafe {
                    let _ = FreeLibrary(vfx);
                    let _ = FreeLibrary(cv);
                }
                return Err(error);
            }
        };
        Ok(Self { cv, vfx, api, model_dir, dir: dir.to_path_buf(), _dll_dir: dll_dir })
    }

    fn create(&self, name: &CStr, label: &str) -> Result<Handle, FxError> {
        let mut handle = ptr::null_mut();
        self.check(
            unsafe { (self.api.create)(name.as_ptr(), &mut handle) },
            &format!("{label} no se creó"),
        )?;
        if handle.is_null() {
            return Err(FxError { message: format!("{label} no se creó") });
        }
        Ok(handle)
    }

    fn check(&self, code: i32, que: &str) -> Result<(), FxError> {
        if code == 0 {
            Ok(())
        } else {
            Err(explain(self.api, code, que))
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        unsafe {
            let _ = FreeLibrary(self.vfx);
            let _ = FreeLibrary(self.cv);
        }
    }
}

pub(crate) struct Engine {
    rt: Runtime,
    green: Handle,
    blur: Handle,
    stream: Handle,
    gpu_src: Box<NvImage>,
    gpu_mask: Box<NvImage>,
    gpu_dst: Box<NvImage>,
    cpu: NvImage,
    width: u32,
    height: u32,
    strength: f32,
    mode: u32,
    frames: u32,
    slow: u32,
    notice: Option<String>,
}

impl Engine {
    pub(crate) fn start() -> Result<Self, FxError> {
        let mut engine = Self {
            rt: Runtime::open()?,
            green: ptr::null_mut(),
            blur: ptr::null_mut(),
            stream: ptr::null_mut(),
            gpu_src: Box::new(NvImage::zero()),
            gpu_mask: Box::new(NvImage::zero()),
            gpu_dst: Box::new(NvImage::zero()),
            cpu: NvImage::zero(),
            width: 0,
            height: 0,
            strength: 0.5,
            mode: MODE_QUALITY,
            frames: 0,
            slow: 0,
            notice: None,
        };
        engine.open_effects()?;
        // El tamaño real llega con el primer cuadro. Esta pasada comprueba
        // driver, GPU y modelos antes de devolver el efecto encendido.
        engine.configure(640, 360)?;
        Ok(engine)
    }

    pub(crate) fn set_strength(&mut self, strength: f32) {
        self.strength = strength;
    }

    pub(crate) fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub(crate) fn apply(&mut self, bgra: &mut [u8], width: usize, height: usize, stride: usize) -> Result<(), FxError> {
        let (w, h) = frame_size(bgra, width, height, stride)?;
        if w != self.width || h != self.height {
            self.configure(w, h)?;
        }
        let started = Instant::now();
        unsafe { self.run_frame(bgra, w, h, stride as i32)? };
        self.watch_pace(started.elapsed(), w, h)?;
        Ok(())
    }

    fn open_effects(&mut self) -> Result<(), FxError> {
        let rt = &self.rt;
        self.green = rt.create(c"GreenScreen", "GreenScreen")?;
        self.blur = rt.create(c"BackgroundBlur", "BackgroundBlur")?;
        rt.check(
            unsafe { (rt.api.set_string)(self.green, c"ModelDir".as_ptr(), rt.model_dir.as_ptr()) },
            "no se encontró el modelo de GreenScreen",
        )?;
        rt.check(
            unsafe { (rt.api.set_u32)(self.green, c"Mode".as_ptr(), self.mode) },
            "GreenScreen no aceptó el modo",
        )?;
        let code = unsafe { (rt.api.stream_create)(&mut self.stream) };
        rt.check(code, "no se pudo crear el stream de CUDA")?;
        rt.check(
            unsafe { (rt.api.set_stream)(self.green, c"CudaStream".as_ptr(), self.stream) },
            "GreenScreen no aceptó el stream de CUDA",
        )?;
        rt.check(
            unsafe { (rt.api.set_stream)(self.blur, c"CudaStream".as_ptr(), self.stream) },
            "BackgroundBlur no aceptó el stream de CUDA",
        )?;
        Ok(())
    }

    fn configure(&mut self, width: u32, height: u32) -> Result<(), FxError> {
        let api = self.rt.api;
        alloc_into(api, &mut self.gpu_src, width, height, NVCV_BGR, NVCV_U8, NVCV_CHUNKY)?;
        alloc_into(api, &mut self.gpu_mask, width, height, NVCV_A, NVCV_U8, NVCV_CHUNKY)?;
        alloc_into(api, &mut self.gpu_dst, width, height, NVCV_BGR, NVCV_U8, NVCV_CHUNKY)?;
        unsafe {
            let _ = (api.set_u32)(self.green, c"MaxInputWidth".as_ptr(), width);
            let _ = (api.set_u32)(self.green, c"MaxInputHeight".as_ptr(), height);
            let _ = (api.set_u32)(self.green, c"Mode".as_ptr(), self.mode);
        }
        let rt = &self.rt;
        let (green, blur) = (self.green, self.blur);
        let code = unsafe { (api.set_image)(green, c"SrcImage0".as_ptr(), &mut *self.gpu_src) };
        rt.check(code, "GreenScreen no aceptó la entrada")?;
        let code = unsafe { (api.set_image)(green, c"DstImage0".as_ptr(), &mut *self.gpu_mask) };
        rt.check(code, "GreenScreen no aceptó la máscara")?;
        let code = unsafe { (api.set_image)(blur, c"SrcImage0".as_ptr(), &mut *self.gpu_src) };
        rt.check(code, "BackgroundBlur no aceptó la entrada")?;
        let code = unsafe { (api.set_image)(blur, c"SrcImage1".as_ptr(), &mut *self.gpu_mask) };
        rt.check(code, "BackgroundBlur no aceptó la máscara")?;
        let code = unsafe { (api.set_image)(blur, c"DstImage0".as_ptr(), &mut *self.gpu_dst) };
        rt.check(code, "BackgroundBlur no aceptó la salida")?;
        rt.check(
            unsafe { (api.set_f32)(blur, c"Strength".as_ptr(), self.strength) },
            "BackgroundBlur no aceptó la fuerza",
        )?;
        rt.check(unsafe { (api.load)(green) }, "GreenScreen no cargó")?;
        rt.check(unsafe { (api.load)(blur) }, "BackgroundBlur no cargó")?;
        self.width = width;
        self.height = height;
        Ok(())
    }

    unsafe fn run_frame(&mut self, bgra: &mut [u8], width: u32, height: u32, pitch: i32) -> Result<(), FxError> {
        let api = self.rt.api;
        let rt = &self.rt;
        let code = (api.init)(
            &mut self.cpu,
            width,
            height,
            pitch,
            bgra.as_mut_ptr().cast(),
            NVCV_BGRA,
            NVCV_U8,
            NVCV_CHUNKY,
            NVCV_CPU,
        );
        rt.check(code, "no se pudo describir el cuadro")?;
        let code = (api.transfer)(&self.cpu, &mut *self.gpu_src, 1.0, self.stream, ptr::null_mut());
        rt.check(code, "no se pudo subir el cuadro a la GPU")?;
        rt.check((api.stream_sync)(self.stream), "CUDA no terminó de subir el cuadro")?;
        rt.check(
            (api.set_f32)(self.blur, c"Strength".as_ptr(), self.strength),
            "BackgroundBlur no aceptó la fuerza",
        )?;
        rt.check((api.run)(self.green, 0), "GreenScreen falló")?;
        rt.check((api.run)(self.blur, 0), "BackgroundBlur falló")?;
        let code = (api.transfer)(&*self.gpu_dst, &mut self.cpu, 1.0, self.stream, ptr::null_mut());
        rt.check(code, "no se pudo bajar el cuadro de la GPU")?;
        rt.check((api.stream_sync)(self.stream), "CUDA no terminó de bajar el cuadro")?;
        Ok(())
    }

    fn watch_pace(&mut self, elapsed: Duration, width: u32, height: u32) -> Result<(), FxError> {
        self.frames = self.frames.saturating_add(1);
        if self.mode != MODE_QUALITY || self.frames <= SLOW_FRAMES {
            return Ok(());
        }
        if elapsed > FRAME_BUDGET {
            self.slow = self.slow.saturating_add(1);
            if self.slow >= SLOW_FRAMES {
                self.mode = MODE_PERFORMANCE;
                self.slow = 0;
                self.notice = Some(
                    "el desenfoque pasó a modo rendimiento para sostener 30 fps".into(),
                );
                self.configure(width, height)?;
            }
        } else {
            self.slow = 0;
        }
        Ok(())
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let api = self.rt.api;
        unsafe {
            if !self.green.is_null() {
                (api.destroy)(self.green);
            }
            if !self.blur.is_null() {
                (api.destroy)(self.blur);
            }
            for image in [&mut *self.gpu_src, &mut *self.gpu_mask, &mut *self.gpu_dst] {
                if !image.pixels.is_null() {
                    (api.dealloc)(image);
                }
            }
            if !self.stream.is_null() {
                (api.stream_destroy)(self.stream);
            }
        }
        // `rt` se suelta después: descarga las DLL y la carpeta del runtime.
    }
}

/// `cudart` del runtime de NVIDIA, para la memoria de estado de `Denoising`.
struct Cuda {
    module: HMODULE,
    malloc: CudaMalloc,
    memset: CudaMemset,
    free: CudaFree,
}

impl Cuda {
    fn open(dir: &Path) -> Result<Self, FxError> {
        let module = load_library(&dir.join("cudart64_12.dll"))?;
        let resolved = (|| {
            Ok::<_, FxError>((sym(module, c"cudaMalloc")?, sym(module, c"cudaMemset")?, sym(module, c"cudaFree")?))
        })();
        match resolved {
            Ok((malloc, memset, free)) => Ok(Self { module, malloc, memset, free }),
            Err(error) => {
                unsafe { let _ = FreeLibrary(module); }
                Err(error)
            }
        }
    }
}

impl Drop for Cuda {
    fn drop(&mut self) {
        unsafe { let _ = FreeLibrary(self.module); }
    }
}

/// `ArtifactReduction` y después `Denoising`, sobre BGR planar en float, en la GPU.
/// Si los dos no sostienen 30 fps, `Denoising` se apaga y queda solo la compresión.
pub(crate) struct Cleanup {
    rt: Runtime,
    cuda: Cuda,
    artifacts: Handle,
    denoise: Handle,
    stream: Handle,
    gpu_src: Box<NvImage>,
    gpu_mid: Box<NvImage>,
    gpu_dst: Box<NvImage>,
    staging: Box<NvImage>,
    cpu: NvImage,
    state: *mut std::ffi::c_void,
    /// `Denoising` guarda este arreglo, no una copia: tiene que quedarse en su lugar.
    state_list: Box<[*mut std::ffi::c_void; 1]>,
    width: u32,
    height: u32,
    strong: bool,
    configured_strong: bool,
    denoise_on: bool,
    frames: u32,
    slow: u32,
    notice: Option<String>,
}

impl Cleanup {
    pub(crate) fn start(strong: bool) -> Result<Self, FxError> {
        let rt = Runtime::open()?;
        let cuda = Cuda::open(&rt.dir)?;
        let mut cleanup = Self {
            rt,
            cuda,
            artifacts: ptr::null_mut(),
            denoise: ptr::null_mut(),
            stream: ptr::null_mut(),
            gpu_src: Box::new(NvImage::zero()),
            gpu_mid: Box::new(NvImage::zero()),
            gpu_dst: Box::new(NvImage::zero()),
            staging: Box::new(NvImage::zero()),
            cpu: NvImage::zero(),
            state: ptr::null_mut(),
            state_list: Box::new([ptr::null_mut()]),
            width: 0,
            height: 0,
            strong,
            configured_strong: strong,
            denoise_on: true,
            frames: 0,
            slow: 0,
            notice: None,
        };
        cleanup.open_effects()?;
        cleanup.configure(640, 360)?;
        Ok(cleanup)
    }

    pub(crate) fn set_strong(&mut self, strong: bool) {
        self.strong = strong;
    }

    pub(crate) fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub(crate) fn apply(&mut self, bgra: &mut [u8], width: usize, height: usize, stride: usize) -> Result<(), FxError> {
        let (w, h) = frame_size(bgra, width, height, stride)?;
        if w != self.width || h != self.height || self.strong != self.configured_strong {
            self.configure(w, h)?;
        }
        let started = Instant::now();
        unsafe { self.run_frame(bgra, w, h, stride as i32)? };
        self.watch_pace(started.elapsed());
        Ok(())
    }

    fn open_effects(&mut self) -> Result<(), FxError> {
        let rt = &self.rt;
        let api = rt.api;
        self.artifacts = rt.create(c"ArtifactReduction", "ArtifactReduction")?;
        self.denoise = rt.create(c"Denoising", "Denoising")?;
        let code = unsafe { (api.stream_create)(&mut self.stream) };
        rt.check(code, "no se pudo crear el stream de CUDA")?;
        for (effect, label) in [(self.artifacts, "ArtifactReduction"), (self.denoise, "Denoising")] {
            rt.check(
                unsafe { (api.set_string)(effect, c"ModelDir".as_ptr(), rt.model_dir.as_ptr()) },
                &format!("no se encontró el modelo de {label}"),
            )?;
            rt.check(
                unsafe { (api.set_stream)(effect, c"CudaStream".as_ptr(), self.stream) },
                &format!("{label} no aceptó el stream de CUDA"),
            )?;
        }
        Ok(())
    }

    fn configure(&mut self, width: u32, height: u32) -> Result<(), FxError> {
        let api = self.rt.api;
        alloc_into(api, &mut self.gpu_src, width, height, NVCV_BGR, NVCV_F32, NVCV_PLANAR)?;
        alloc_into(api, &mut self.gpu_mid, width, height, NVCV_BGR, NVCV_F32, NVCV_PLANAR)?;
        alloc_into(api, &mut self.gpu_dst, width, height, NVCV_BGR, NVCV_F32, NVCV_PLANAR)?;
        self.free_state();
        let rt = &self.rt;
        let (artifacts, denoise) = (self.artifacts, self.denoise);
        let strong = self.strong;
        let code = unsafe { (api.set_image)(artifacts, c"SrcImage0".as_ptr(), &mut *self.gpu_src) };
        rt.check(code, "ArtifactReduction no aceptó la entrada")?;
        let code = unsafe { (api.set_image)(artifacts, c"DstImage0".as_ptr(), &mut *self.gpu_mid) };
        rt.check(code, "ArtifactReduction no aceptó la salida")?;
        // Modo 0 respeta los degradados suaves; el 1 limpia más, para Wi-Fi con poca señal.
        rt.check(
            unsafe { (api.set_u32)(artifacts, c"Mode".as_ptr(), u32::from(strong)) },
            "ArtifactReduction no aceptó el modo",
        )?;
        let code = unsafe { (api.set_image)(denoise, c"SrcImage0".as_ptr(), &mut *self.gpu_mid) };
        rt.check(code, "Denoising no aceptó la entrada")?;
        let code = unsafe { (api.set_image)(denoise, c"DstImage0".as_ptr(), &mut *self.gpu_dst) };
        rt.check(code, "Denoising no aceptó la salida")?;
        rt.check(
            unsafe { (api.set_f32)(denoise, c"Strength".as_ptr(), if strong { 1.0 } else { 0.0 }) },
            "Denoising no aceptó la fuerza",
        )?;
        let mut state_bytes = 0u32;
        rt.check(
            unsafe { (api.get_u32)(denoise, c"StateSize".as_ptr(), &mut state_bytes) },
            "Denoising no dijo cuánta memoria de estado necesita",
        )?;
        if state_bytes > 0 {
            let mut state = ptr::null_mut();
            let code = unsafe { (self.cuda.malloc)(&mut state, state_bytes as usize) };
            if code != 0 || state.is_null() {
                return Err(FxError { message: format!("no se pudo reservar el estado de Denoising en la GPU ({code})") });
            }
            self.state = state;
            let code = unsafe { (self.cuda.memset)(state, 0, state_bytes as usize) };
            if code != 0 {
                return Err(FxError { message: format!("no se pudo limpiar el estado de Denoising ({code})") });
            }
        }
        self.state_list[0] = self.state;
        let rt = &self.rt;
        rt.check(
            unsafe { (api.set_object)(denoise, c"State".as_ptr(), self.state_list.as_mut_ptr().cast()) },
            "Denoising no aceptó el estado",
        )?;
        rt.check(unsafe { (api.load)(artifacts) }, "ArtifactReduction no cargó")?;
        rt.check(unsafe { (api.load)(denoise) }, "Denoising no cargó")?;
        self.width = width;
        self.height = height;
        self.configured_strong = strong;
        Ok(())
    }

    unsafe fn run_frame(&mut self, bgra: &mut [u8], width: u32, height: u32, pitch: i32) -> Result<(), FxError> {
        let api = self.rt.api;
        let rt = &self.rt;
        let code = (api.init)(
            &mut self.cpu,
            width,
            height,
            pitch,
            bgra.as_mut_ptr().cast(),
            NVCV_BGRA,
            NVCV_U8,
            NVCV_CHUNKY,
            NVCV_CPU,
        );
        rt.check(code, "no se pudo describir el cuadro")?;
        // Los modelos esperan 0..1: se escala al subir y al bajar.
        let code = (api.transfer)(&self.cpu, &mut *self.gpu_src, 1.0 / 255.0, self.stream, &mut *self.staging);
        rt.check(code, "no se pudo subir el cuadro a la GPU")?;
        rt.check((api.run)(self.artifacts, 0), "ArtifactReduction falló")?;
        let out: *const NvImage = if self.denoise_on {
            rt.check((api.run)(self.denoise, 0), "Denoising falló")?;
            &*self.gpu_dst
        } else {
            &*self.gpu_mid
        };
        let code = (api.transfer)(out, &mut self.cpu, 255.0, self.stream, &mut *self.staging);
        rt.check(code, "no se pudo bajar el cuadro de la GPU")?;
        rt.check((api.stream_sync)(self.stream), "CUDA no terminó el cuadro")?;
        Ok(())
    }

    fn watch_pace(&mut self, elapsed: Duration) {
        self.frames = self.frames.saturating_add(1);
        if !self.denoise_on || self.frames <= SLOW_FRAMES {
            return;
        }
        if elapsed > FRAME_BUDGET {
            self.slow = self.slow.saturating_add(1);
            if self.slow >= SLOW_FRAMES {
                self.denoise_on = false;
                self.notice = Some("la mejora de imagen dejó de quitar ruido para sostener 30 fps".into());
            }
        } else {
            self.slow = 0;
        }
    }

    fn free_state(&mut self) {
        if !self.state.is_null() {
            unsafe { (self.cuda.free)(self.state) };
            self.state = ptr::null_mut();
        }
        self.state_list[0] = ptr::null_mut();
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let api = self.rt.api;
        unsafe {
            if !self.stream.is_null() {
                (api.stream_sync)(self.stream);
            }
            if !self.artifacts.is_null() {
                (api.destroy)(self.artifacts);
            }
            if !self.denoise.is_null() {
                (api.destroy)(self.denoise);
            }
            for image in [&mut *self.gpu_src, &mut *self.gpu_mid, &mut *self.gpu_dst, &mut *self.staging] {
                if !image.pixels.is_null() {
                    (api.dealloc)(image);
                }
            }
        }
        self.free_state();
        unsafe {
            if !self.stream.is_null() {
                (api.stream_destroy)(self.stream);
            }
        }
    }
}

fn frame_size(bgra: &[u8], width: usize, height: usize, stride: usize) -> Result<(u32, u32), FxError> {
    if width == 0 || height == 0 || width > u32::MAX as usize || height > u32::MAX as usize || stride > i32::MAX as usize {
        return Err(FxError { message: "el cuadro está vacío".into() });
    }
    let row = width.saturating_mul(4);
    let needed = stride.saturating_mul(height.saturating_sub(1)).saturating_add(row);
    if stride < row || bgra.len() < needed {
        return Err(FxError { message: "el cuadro no entra en el buffer".into() });
    }
    Ok((width as u32, height as u32))
}

fn explain(api: Api, code: i32, que: &str) -> FxError {
    let detail = unsafe {
        let text = (api.err)(code);
        if text.is_null() {
            String::new()
        } else {
            CStr::from_ptr(text).to_string_lossy().into_owned()
        }
    };
    FxError { message: format!("{que}: {detail} ({code})") }
}

/// Cuántos efectos tienen puesta la carpeta del runtime. La última en soltarse la quita.
static DLL_DIR_USERS: Mutex<usize> = Mutex::new(0);

struct DllDir;

impl DllDir {
    fn set(dir: &Path) -> Result<Self, FxError> {
        let mut users = DLL_DIR_USERS.lock().unwrap_or_else(|poison| poison.into_inner());
        let wide = wide(dir);
        unsafe { SetDllDirectoryW(PCWSTR::from_raw(wide.as_ptr())) }.map_err(|error| FxError {
            message: format!("no se pudo usar la carpeta del runtime: {error}"),
        })?;
        *users += 1;
        Ok(Self)
    }
}

impl Drop for DllDir {
    fn drop(&mut self) {
        let mut users = DLL_DIR_USERS.lock().unwrap_or_else(|poison| poison.into_inner());
        *users = users.saturating_sub(1);
        if *users == 0 {
            unsafe { let _ = SetDllDirectoryW(PCWSTR::null()); }
        }
    }
}

fn alloc_into(
    api: Api,
    slot: &mut NvImage,
    width: u32,
    height: u32,
    format: i32,
    component: i32,
    layout: u32,
) -> Result<(), FxError> {
    if !slot.pixels.is_null() {
        unsafe { (api.dealloc)(slot) };
        *slot = NvImage::zero();
    }
    let code = unsafe { (api.alloc)(slot, width, height, format, component, layout, NVCV_GPU, 1) };
    if code != 0 {
        if !slot.pixels.is_null() {
            unsafe { (api.dealloc)(slot) };
        }
        *slot = NvImage::zero();
        return Err(explain(api, code, "no se pudo reservar la imagen en la GPU"));
    }
    Ok(())
}

fn load_library(path: &Path) -> Result<HMODULE, FxError> {
    let wide = wide(path);
    unsafe { LoadLibraryExW(PCWSTR::from_raw(wide.as_ptr()), None, LOAD_WITH_ALTERED_SEARCH_PATH) }.map_err(|error| {
        FxError { message: format!("no se pudo cargar {}: {error}", path.display()) }
    })
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn resolve(cv: HMODULE, vfx: HMODULE) -> Result<Api, FxError> {
    Ok(Api {
        create: sym(vfx, c"NvVFX_CreateEffect")?,
        destroy: sym(vfx, c"NvVFX_DestroyEffect")?,
        set_u32: sym(vfx, c"NvVFX_SetU32")?,
        get_u32: sym(vfx, c"NvVFX_GetU32")?,
        set_f32: sym(vfx, c"NvVFX_SetF32")?,
        set_image: sym(vfx, c"NvVFX_SetImage")?,
        set_string: sym(vfx, c"NvVFX_SetString")?,
        set_stream: sym(vfx, c"NvVFX_SetCudaStream")?,
        set_object: sym(vfx, c"NvVFX_SetObject")?,
        load: sym(vfx, c"NvVFX_Load")?,
        run: sym(vfx, c"NvVFX_Run")?,
        stream_create: sym(vfx, c"NvVFX_CudaStreamCreate")?,
        stream_destroy: sym(vfx, c"NvVFX_CudaStreamDestroy")?,
        stream_sync: sym(vfx, c"NvVFX_CudaStreamSynchronize")?,
        init: sym(cv, c"NvCVImage_Init")?,
        alloc: sym(cv, c"NvCVImage_Alloc")?,
        dealloc: sym(cv, c"NvCVImage_Dealloc")?,
        transfer: sym(cv, c"NvCVImage_Transfer")?,
        err: sym(cv, c"NvCV_GetErrorStringFromCode")?,
    })
}

fn sym<T>(module: HMODULE, name: &CStr) -> Result<T, FxError> {
    let proc = unsafe { GetProcAddress(module, PCSTR(name.as_ptr().cast())) };
    let Some(proc) = proc else {
        return Err(FxError { message: format!("el runtime no exporta {}", name.to_string_lossy()) });
    };
    Ok(unsafe { mem::transmute_copy(&proc) })
}
