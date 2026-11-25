//! Efectos sobre un buffer BGRA que le pasan. No conoce cellCam, el decoder ni la UI.
//! El desenfoque no redistribuye DLL ni modelos: los busca en el runtime del usuario.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorParams {
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub temperature: f32,
}

impl Default for ColorParams {
    fn default() -> Self {
        Self { brightness: 0.0, contrast: 0.0, saturation: 0.0, temperature: 0.0 }
    }
}

impl ColorParams {
    pub fn is_neutral(self) -> bool {
        self.brightness == 0.0
            && self.contrast == 0.0
            && self.saturation == 0.0
            && self.temperature == 0.0
    }

    fn clamped(self) -> Self {
        Self {
            brightness: self.brightness.clamp(-1.0, 1.0),
            contrast: self.contrast.clamp(-1.0, 1.0),
            saturation: self.saturation.clamp(-1.0, 1.0),
            temperature: self.temperature.clamp(-1.0, 1.0),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ColorCorrector {
    params: ColorParams,
    lut_r: [u8; 256],
    lut_g: [u8; 256],
    lut_b: [u8; 256],
    sat: f32,
}

impl Default for ColorCorrector {
    fn default() -> Self {
        Self::new()
    }
}

impl ColorCorrector {
    pub fn new() -> Self {
        let mut corrector = Self {
            params: ColorParams::default(),
            lut_r: [0; 256],
            lut_g: [0; 256],
            lut_b: [0; 256],
            sat: 1.0,
        };
        corrector.rebuild();
        corrector
    }

    pub fn set(&mut self, params: ColorParams) {
        let params = params.clamped();
        if params == self.params {
            return;
        }
        self.params = params;
        self.rebuild();
    }

    pub fn apply(&self, bgra: &mut [u8], width: usize, height: usize, stride: usize) {
        if self.params.is_neutral() || width == 0 || height == 0 {
            return;
        }
        let row_bytes = width.saturating_mul(4);
        if stride < row_bytes || bgra.len() < stride.saturating_mul(height.saturating_sub(1)) + row_bytes {
            return;
        }
        for y in 0..height {
            let row = &mut bgra[y * stride..y * stride + row_bytes];
            for px in row.chunks_exact_mut(4) {
                let b = self.lut_b[px[0] as usize] as f32;
                let g = self.lut_g[px[1] as usize] as f32;
                let r = self.lut_r[px[2] as usize] as f32;
                let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                px[0] = clamp_u8(luma + (b - luma) * self.sat);
                px[1] = clamp_u8(luma + (g - luma) * self.sat);
                px[2] = clamp_u8(luma + (r - luma) * self.sat);
            }
        }
    }

    fn rebuild(&mut self) {
        let p = self.params;
        let contrast = 1.0 + p.contrast;
        let bright = p.brightness * 255.0;
        let warm = p.temperature * 64.0;
        for i in 0..256 {
            let base = (i as f32 - 128.0) * contrast + 128.0 + bright;
            self.lut_r[i] = clamp_u8(base + warm);
            self.lut_g[i] = clamp_u8(base);
            self.lut_b[i] = clamp_u8(base - warm);
        }
        self.sat = 1.0 + p.saturation;
    }
}

fn clamp_u8(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlurAvailability {
    Available,
    NoNvidiaGpu,
    NoRuntime,
}

#[derive(Debug)]
pub struct FxError {
    pub message: String,
}

impl fmt::Display for FxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FxError {}

pub fn blur_availability() -> BlurAvailability {
    #[cfg(windows)]
    {
        if runtime_dll().is_none() {
            return BlurAvailability::NoRuntime;
        }
        if !nvidia_adapter_present() {
            return BlurAvailability::NoNvidiaGpu;
        }
        BlurAvailability::Available
    }
    #[cfg(not(windows))]
    {
        BlurAvailability::NoRuntime
    }
}

pub struct BackgroundBlur {
    strength: f32,
}

impl BackgroundBlur {
    pub fn new() -> Result<Self, FxError> {
        match blur_availability() {
            BlurAvailability::NoRuntime => {
                return Err(FxError { message: "falta el runtime de NVIDIA Video Effects".into() });
            }
            BlurAvailability::NoNvidiaGpu => {
                return Err(FxError { message: "no hay una GPU NVIDIA".into() });
            }
            BlurAvailability::Available => {}
        }
        // El chequeo definitivo (RTX, driver, modelos) ocurre al cargar el runtime.
        // Cargarlo y dejarlo a medias corrompe el frame: si Load no está listo, se apaga.
        Err(FxError {
            message: "el runtime está instalado, pero este build todavía no corre GreenScreen".into(),
        })
    }

    pub fn set_strength(&mut self, strength: f32) {
        self.strength = strength.clamp(0.0, 1.0);
    }

    pub fn apply(&mut self, _bgra: &mut [u8], _width: usize, _height: usize, _stride: usize) -> Result<(), FxError> {
        let _ = self.strength;
        Err(FxError { message: "desenfoque apagado".into() })
    }
}

#[cfg(windows)]
fn runtime_dll() -> Option<std::path::PathBuf> {
    if let Some(dir) = std::env::var_os("NV_VIDEO_EFFECTS_PATH") {
        let path = std::path::PathBuf::from(dir).join("NVVideoEffects.dll");
        if path.is_file() {
            return Some(path);
        }
    }
    let path = std::path::PathBuf::from(r"C:\Program Files\NVIDIA Corporation\NVIDIA Video Effects\NVVideoEffects.dll");
    path.is_file().then_some(path)
}

#[cfg(windows)]
fn nvidia_adapter_present() -> bool {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
    unsafe {
        let factory: IDXGIFactory1 = match CreateDXGIFactory1() {
            Ok(factory) => factory,
            Err(_) => return false,
        };
        let mut index = 0;
        loop {
            let adapter = match factory.EnumAdapters1(index) {
                Ok(adapter) => adapter,
                Err(_) => return false,
            };
            if let Ok(desc) = adapter.GetDesc1() {
                if desc.VendorId == 0x10DE {
                    return true;
                }
            }
            index += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutro_no_cambia_el_pixel() {
        let corrector = ColorCorrector::new();
        let original = [10u8, 20, 30, 255, 200, 10, 5, 128];
        let mut pixels = original;
        corrector.apply(&mut pixels, 2, 1, 8);
        assert_eq!(pixels, original);
    }

    #[test]
    fn extremos_no_desbordan() {
        let mut corrector = ColorCorrector::new();
        let mut pixels = [0u8, 0, 0, 255, 255, 255, 255, 255];
        for sign in [-1.0, 1.0] {
            corrector.set(ColorParams {
                brightness: sign,
                contrast: sign,
                saturation: sign,
                temperature: sign,
            });
            corrector.apply(&mut pixels, 2, 1, 8);
            assert_eq!(pixels[3], 255);
            assert_eq!(pixels[7], 255);
        }
    }

    #[test]
    fn el_brillo_alto_aclara_un_gris() {
        let mut corrector = ColorCorrector::new();
        corrector.set(ColorParams { brightness: 1.0, ..ColorParams::default() });
        let mut pixels = [40u8, 40, 40, 255];
        corrector.apply(&mut pixels, 1, 1, 4);
        assert!(pixels[0] > 40 && pixels[1] > 40 && pixels[2] > 40);
    }
}
