//! FFI al runtime NVIDIA Video Effects 0.7 que instaló el usuario.
//! `GreenScreen` escribe la máscara y `BackgroundBlur` la aplica.
//! Las imágenes de GPU viven en un `Box` para que el puntero que guarda el efecto no se mueva.

use std::ffi::{CStr, CString};
use std::mem;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, SetDllDirectoryW, LOAD_WITH_ALTERED_SEARCH_PATH,
};
use windows::core::{PCSTR, PCWSTR};

use super::{runtime_dll, FxError};

const NVCV_A: i32 = 2;
const NVCV_BGR: i32 = 5;
const NVCV_BGRA: i32 = 7;
const NVCV_U8: i32 = 1;
const NVCV_CHUNKY: u32 = 0;
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

type CreateEffect = unsafe extern "C" fn(*const i8, *mut *mut std::ffi::c_void) -> i32;
type DestroyEffect = unsafe extern "C" fn(*mut std::ffi::c_void) -> i32;
type SetU32 = unsafe extern "C" fn(*mut std::ffi::c_void, *const i8, u32) -> i32;
type SetF32 = unsafe extern "C" fn(*mut std::ffi::c_void, *const i8, f32) -> i32;
type SetImage = unsafe extern "C" fn(*mut std::ffi::c_void, *const i8, *mut NvImage) -> i32;
type SetString = unsafe extern "C" fn(*mut std::ffi::c_void, *const i8, *const i8) -> i32;
type SetStream = unsafe extern "C" fn(*mut std::ffi::c_void, *const i8, *mut std::ffi::c_void) -> i32;
type LoadFx = unsafe extern "C" fn(*mut std::ffi::c_void) -> i32;
type RunFx = unsafe extern "C" fn(*mut std::ffi::c_void, i32) -> i32;
type StreamCreate = unsafe extern "C" fn(*mut *mut std::ffi::c_void) -> i32;
type StreamDestroy = unsafe extern "C" fn(*mut std::ffi::c_void) -> i32;
type StreamSync = unsafe extern "C" fn(*mut std::ffi::c_void) -> i32;
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

#[derive(Clone, Copy)]
struct Api {
    create: CreateEffect,
    destroy: DestroyEffect,
    set_u32: SetU32,
    set_f32: SetF32,
    set_image: SetImage,
    set_string: SetString,
    set_stream: SetStream,
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

pub(crate) struct Engine {
    cv: HMODULE,
    vfx: HMODULE,
    api: Api,
    green: *mut std::ffi::c_void,
    blur: *mut std::ffi::c_void,
    stream: *mut std::ffi::c_void,
    gpu_src: Box<NvImage>,
    gpu_mask: Box<NvImage>,
    gpu_dst: Box<NvImage>,
    cpu: NvImage,
    model_dir: CString,
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
        let _dll_dir = DllDir::set(dir)?;
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
        let mut engine = Self {
            cv,
            vfx,
            api,
            green: ptr::null_mut(),
            blur: ptr::null_mut(),
            stream: ptr::null_mut(),
            gpu_src: Box::new(NvImage::zero()),
            gpu_mask: Box::new(NvImage::zero()),
            gpu_dst: Box::new(NvImage::zero()),
            cpu: NvImage::zero(),
            model_dir,
            width: 0,
            height: 0,
            strength: 0.5,
            mode: MODE_QUALITY,
            frames: 0,
            slow: 0,
            notice: None,
        };
        // DllDir se suelta acá: las DLL ya quedaron cargadas y el directorio
        // sigue haciendo falta para las cargas tardías de TensorRT. Lo dejamos
        // puesto hasta Drop.
        mem::forget(_dll_dir);
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
        if width == 0 || height == 0 || width > u32::MAX as usize || height > u32::MAX as usize {
            return Err(FxError { message: "el cuadro está vacío".into() });
        }
        let row = width.saturating_mul(4);
        let needed = stride.saturating_mul(height.saturating_sub(1)).saturating_add(row);
        if stride < row || bgra.len() < needed {
            return Err(FxError { message: "el cuadro no entra en el buffer".into() });
        }
        let w = width as u32;
        let h = height as u32;
        if w != self.width || h != self.height {
            self.configure(w, h)?;
        }
        let started = Instant::now();
        unsafe { self.run_frame(bgra, w, h, stride as i32)? };
        self.watch_pace(started.elapsed(), w, h)?;
        Ok(())
    }

    fn open_effects(&mut self) -> Result<(), FxError> {
        self.green = self.create(c"GreenScreen", "GreenScreen")?;
        self.blur = self.create(c"BackgroundBlur", "BackgroundBlur")?;
        self.check(
            unsafe { (self.api.set_string)(self.green, c"ModelDir".as_ptr(), self.model_dir.as_ptr()) },
            "no se encontró el modelo de GreenScreen",
        )?;
        self.check(
            unsafe { (self.api.set_u32)(self.green, c"Mode".as_ptr(), self.mode) },
            "GreenScreen no aceptó el modo",
        )?;
        let code = unsafe { (self.api.stream_create)(&mut self.stream) };
        self.check(code, "no se pudo crear el stream de CUDA")?;
        self.check(
            unsafe { (self.api.set_stream)(self.green, c"CudaStream".as_ptr(), self.stream) },
            "GreenScreen no aceptó el stream de CUDA",
        )?;
        self.check(
            unsafe { (self.api.set_stream)(self.blur, c"CudaStream".as_ptr(), self.stream) },
            "BackgroundBlur no aceptó el stream de CUDA",
        )?;
        Ok(())
    }

    fn create(&self, name: &CStr, label: &str) -> Result<*mut std::ffi::c_void, FxError> {
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

    fn configure(&mut self, width: u32, height: u32) -> Result<(), FxError> {
        let api = self.api;
        alloc_into(api, &mut self.gpu_src, width, height, NVCV_BGR)?;
        alloc_into(api, &mut self.gpu_mask, width, height, NVCV_A)?;
        alloc_into(api, &mut self.gpu_dst, width, height, NVCV_BGR)?;
        unsafe {
            let _ = (self.api.set_u32)(self.green, c"MaxInputWidth".as_ptr(), width);
            let _ = (self.api.set_u32)(self.green, c"MaxInputHeight".as_ptr(), height);
            let _ = (self.api.set_u32)(self.green, c"Mode".as_ptr(), self.mode);
        }
        let set_image = self.api.set_image;
        let green = self.green;
        let blur = self.blur;
        let code = unsafe { set_image(green, c"SrcImage0".as_ptr(), &mut *self.gpu_src) };
        self.check(code, "GreenScreen no aceptó la entrada")?;
        let code = unsafe { set_image(green, c"DstImage0".as_ptr(), &mut *self.gpu_mask) };
        self.check(code, "GreenScreen no aceptó la máscara")?;
        let code = unsafe { set_image(blur, c"SrcImage0".as_ptr(), &mut *self.gpu_src) };
        self.check(code, "BackgroundBlur no aceptó la entrada")?;
        let code = unsafe { set_image(blur, c"SrcImage1".as_ptr(), &mut *self.gpu_mask) };
        self.check(code, "BackgroundBlur no aceptó la máscara")?;
        let code = unsafe { set_image(blur, c"DstImage0".as_ptr(), &mut *self.gpu_dst) };
        self.check(code, "BackgroundBlur no aceptó la salida")?;
        self.check(
            unsafe { (self.api.set_f32)(self.blur, c"Strength".as_ptr(), self.strength) },
            "BackgroundBlur no aceptó la fuerza",
        )?;
        self.check(unsafe { (self.api.load)(self.green) }, "GreenScreen no cargó")?;
        self.check(unsafe { (self.api.load)(self.blur) }, "BackgroundBlur no cargó")?;
        self.width = width;
        self.height = height;
        Ok(())
    }

    unsafe fn run_frame(&mut self, bgra: &mut [u8], width: u32, height: u32, pitch: i32) -> Result<(), FxError> {
        let code = (self.api.init)(
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
        self.check(code, "no se pudo describir el cuadro")?;
        let code = (self.api.transfer)(&self.cpu, &mut *self.gpu_src, 1.0, self.stream, ptr::null_mut());
        self.check(code, "no se pudo subir el cuadro a la GPU")?;
        self.check((self.api.stream_sync)(self.stream), "CUDA no terminó de subir el cuadro")?;
        self.check(
            (self.api.set_f32)(self.blur, c"Strength".as_ptr(), self.strength),
            "BackgroundBlur no aceptó la fuerza",
        )?;
        self.check((self.api.run)(self.green, 0), "GreenScreen falló")?;
        self.check((self.api.run)(self.blur, 0), "BackgroundBlur falló")?;
        let code = (self.api.transfer)(&*self.gpu_dst, &mut self.cpu, 1.0, self.stream, ptr::null_mut());
        self.check(code, "no se pudo bajar el cuadro de la GPU")?;
        self.check((self.api.stream_sync)(self.stream), "CUDA no terminó de bajar el cuadro")?;
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

    fn check(&self, code: i32, que: &str) -> Result<(), FxError> {
        if code == 0 {
            Ok(())
        } else {
            Err(self.explain(code, que))
        }
    }

    fn explain(&self, code: i32, que: &str) -> FxError {
        let detail = unsafe {
            let text = (self.api.err)(code);
            if text.is_null() {
                String::new()
            } else {
                CStr::from_ptr(text).to_string_lossy().into_owned()
            }
        };
        FxError { message: format!("{que}: {detail} ({code})") }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        unsafe {
            if !self.green.is_null() {
                (self.api.destroy)(self.green);
            }
            if !self.blur.is_null() {
                (self.api.destroy)(self.blur);
            }
            if !self.gpu_src.pixels.is_null() {
                (self.api.dealloc)(&mut *self.gpu_src);
            }
            if !self.gpu_mask.pixels.is_null() {
                (self.api.dealloc)(&mut *self.gpu_mask);
            }
            if !self.gpu_dst.pixels.is_null() {
                (self.api.dealloc)(&mut *self.gpu_dst);
            }
            if !self.stream.is_null() {
                (self.api.stream_destroy)(self.stream);
            }
            let _ = FreeLibrary(self.vfx);
            let _ = FreeLibrary(self.cv);
            let _ = SetDllDirectoryW(PCWSTR::null());
        }
    }
}

struct DllDir;

impl DllDir {
    fn set(dir: &Path) -> Result<Self, FxError> {
        let wide = wide(dir);
        unsafe { SetDllDirectoryW(PCWSTR::from_raw(wide.as_ptr())) }.map_err(|error| FxError {
            message: format!("no se pudo usar la carpeta del runtime: {error}"),
        })?;
        Ok(Self)
    }
}

impl Drop for DllDir {
    fn drop(&mut self) {
        unsafe { let _ = SetDllDirectoryW(PCWSTR::null()); }
    }
}

fn alloc_into(api: Api, slot: &mut NvImage, width: u32, height: u32, format: i32) -> Result<(), FxError> {
    if !slot.pixels.is_null() {
        unsafe { (api.dealloc)(slot) };
        *slot = NvImage::zero();
    }
    let code = unsafe { (api.alloc)(slot, width, height, format, NVCV_U8, NVCV_CHUNKY, NVCV_GPU, 1) };
    if code != 0 {
        if !slot.pixels.is_null() {
            unsafe { (api.dealloc)(slot) };
        }
        *slot = NvImage::zero();
        let detail = unsafe {
            let text = (api.err)(code);
            if text.is_null() {
                String::new()
            } else {
                CStr::from_ptr(text).to_string_lossy().into_owned()
            }
        };
        return Err(FxError { message: format!("no se pudo reservar la imagen en la GPU: {detail} ({code})") });
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
        set_f32: sym(vfx, c"NvVFX_SetF32")?,
        set_image: sym(vfx, c"NvVFX_SetImage")?,
        set_string: sym(vfx, c"NvVFX_SetString")?,
        set_stream: sym(vfx, c"NvVFX_SetCudaStream")?,
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
