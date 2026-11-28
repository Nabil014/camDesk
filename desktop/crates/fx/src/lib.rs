//! Efectos sobre un buffer BGRA que le pasan. No conoce cellCam, el decoder ni la UI.
//! El desenfoque no redistribuye DLL ni modelos: los busca en el runtime del usuario.

use std::fmt;

#[cfg(windows)]
mod nvidia;

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

/// Cuánto se acerca el ajuste medido al del cuadro anterior: unos 0,4 s a 30 fps.
const AUTO_SMOOTHING: f32 = 0.08;

/// Ajuste que mide `AutoColor` en cada cuadro.
#[derive(Clone, Copy, Debug, PartialEq)]
struct AutoLevels {
    /// Ganancia por canal del mundo gris, en orden B, G, R.
    gain: [f32; 3],
    /// Punto negro y punto blanco de la luma. La ganancia no mueve la luma media.
    black: f32,
    white: f32,
    /// Exponente de la curva de exposición; 1 = sin cambio.
    gamma: f32,
}

impl AutoLevels {
    const NEUTRAL: Self = Self { gain: [1.0; 3], black: 0.0, white: 255.0, gamma: 1.0 };

    fn toward(self, target: Self, k: f32) -> Self {
        let mix = |a: f32, b: f32| a + (b - a) * k;
        Self {
            gain: [mix(self.gain[0], target.gain[0]), mix(self.gain[1], target.gain[1]), mix(self.gain[2], target.gain[2])],
            black: mix(self.black, target.black),
            white: mix(self.white, target.white),
            gamma: mix(self.gamma, target.gamma),
        }
    }
}

/// Balance de blancos, niveles y exposición automáticos sobre BGRA.
///
/// Mide una muestra del cuadro (mundo gris para el tinte, percentiles de luma para
/// negro y blanco, la media para la exposición), acerca el ajuste de a poco para que
/// la imagen no parpadee y lo aplica con una tabla por canal. Va antes del
/// `ColorCorrector`: los sliders quedan como ajuste fino sobre el resultado.
#[derive(Clone, Debug)]
pub struct AutoColor {
    current: Option<AutoLevels>,
    lut: [[u8; 256]; 3],
}

impl Default for AutoColor {
    fn default() -> Self {
        Self::new()
    }
}

impl AutoColor {
    pub fn new() -> Self {
        Self { current: None, lut: [[0; 256]; 3] }
    }

    /// Olvida lo medido: el próximo cuadro toma su ajuste de una vez, sin transición.
    /// Para una imagen nueva, por ejemplo al reconectar o al cambiar de cámara.
    pub fn reset(&mut self) {
        self.current = None;
    }

    pub fn apply(&mut self, bgra: &mut [u8], width: usize, height: usize, stride: usize) {
        if width == 0 || height == 0 {
            return;
        }
        let row_bytes = width.saturating_mul(4);
        if stride < row_bytes || bgra.len() < stride.saturating_mul(height - 1) + row_bytes {
            return;
        }
        let target = measure(bgra, width, height, stride);
        let levels = match self.current {
            Some(current) => current.toward(target, AUTO_SMOOTHING),
            None => target,
        };
        self.current = Some(levels);
        self.rebuild(levels);
        for y in 0..height {
            let row = &mut bgra[y * stride..y * stride + row_bytes];
            for px in row.chunks_exact_mut(4) {
                px[0] = self.lut[0][px[0] as usize];
                px[1] = self.lut[1][px[1] as usize];
                px[2] = self.lut[2][px[2] as usize];
            }
        }
    }

    fn rebuild(&mut self, levels: AutoLevels) {
        let span = (levels.white - levels.black).max(1.0);
        for (channel, lut) in self.lut.iter_mut().enumerate() {
            let gain = levels.gain[channel];
            for (i, slot) in lut.iter_mut().enumerate() {
                let stretched = ((i as f32 * gain - levels.black) / span).clamp(0.0, 1.0);
                *slot = clamp_u8(stretched.powf(levels.gamma) * 255.0);
            }
        }
    }
}

/// Mide sobre una de cada 4x4 posiciones: alcanza para el promedio y no cuesta.
fn measure(bgra: &[u8], width: usize, height: usize, stride: usize) -> AutoLevels {
    const STEP: usize = 4;
    let mut sums = [0f64; 3];
    let mut counted = 0u64;
    let mut histogram = [0u32; 256];
    let mut samples = 0u32;
    for y in (0..height).step_by(STEP) {
        let row = &bgra[y * stride..y * stride + width * 4];
        for px in row.chunks_exact(4).step_by(STEP) {
            let (b, g, r) = (px[0], px[1], px[2]);
            let luma = (0.0722 * b as f32 + 0.7152 * g as f32 + 0.2126 * r as f32) as usize;
            histogram[luma.min(255)] += 1;
            samples += 1;
            // Lo quemado y lo casi negro no dicen de qué color es la luz.
            let (low, high) = (b.min(g).min(r), b.max(g).max(r));
            if low > 12 && high < 245 {
                sums[0] += b as f64;
                sums[1] += g as f64;
                sums[2] += r as f64;
                counted += 1;
            }
        }
    }
    if samples == 0 {
        return AutoLevels::NEUTRAL;
    }

    // Mundo gris: el promedio de la escena tiende a gris. Se corrige la mitad del
    // tinte y con tope, para no borrar una pared de color ni la luz cálida de una lámpara.
    let mut gain = [1.0f32; 3];
    if counted * 50 >= samples as u64 {
        let mean = [sums[0] / counted as f64, sums[1] / counted as f64, sums[2] / counted as f64];
        let gray = (mean[0] + mean[1] + mean[2]) / 3.0;
        for c in 0..3 {
            if mean[c] > 1.0 {
                let full = (gray / mean[c]) as f32;
                gain[c] = (1.0 + (full - 1.0) * 0.5).clamp(0.8, 1.25);
            }
        }
        // Sin cambiar el brillo general: la luma media queda donde estaba.
        let luma_before = 0.0722 * mean[0] + 0.7152 * mean[1] + 0.2126 * mean[2];
        let luma_after = 0.0722 * mean[0] * gain[0] as f64 + 0.7152 * mean[1] * gain[1] as f64 + 0.2126 * mean[2] * gain[2] as f64;
        if luma_after > 1.0 {
            let keep = (luma_before / luma_after) as f32;
            for g in &mut gain {
                *g *= keep;
            }
        }
    }

    // Niveles: el 0,5 % más oscuro va a negro y el 0,5 % más claro a blanco. Con tope:
    // el blanco se estira hasta ×1,6 y el negro baja como mucho 24 niveles, para no
    // inventar contraste en una escena lavada a propósito.
    let percentile = |fraction: f32| -> f32 {
        let wanted = (samples as f32 * fraction) as u32;
        let mut seen = 0u32;
        for (value, count) in histogram.iter().enumerate() {
            seen += count;
            if seen > wanted {
                return value as f32;
            }
        }
        255.0
    };
    let mut black = percentile(0.005).min(24.0);
    let mut white = percentile(0.995).max(160.0);
    if white - black < 64.0 {
        black = 0.0;
        white = 255.0;
    }

    // Exposición: si la luma media, ya estirada, queda fuera de 0,40..0,56, una curva
    // la acerca al borde de esa franja. Dentro de la franja no se toca.
    let mut weighted = 0f64;
    for (value, count) in histogram.iter().enumerate() {
        weighted += value as f64 * *count as f64;
    }
    let mean_luma = (weighted / samples as f64) as f32;
    let stretched = ((mean_luma - black) / (white - black)).clamp(0.02, 0.98);
    let gamma = if stretched < 0.40 {
        0.40f32.ln() / stretched.ln()
    } else if stretched > 0.56 {
        0.56f32.ln() / stretched.ln()
    } else {
        1.0
    };
    let gamma = gamma.clamp(0.75, 1.25);

    AutoLevels { gain, black, white, gamma }
}

/// Quita los bloques de compresión de H.264 y el ruido de poca luz con
/// `ArtifactReduction` y `Denoising` de NVIDIA. Mismos requisitos que `BackgroundBlur`:
/// `blur_availability()` vale para los dos.
pub struct ImageCleanup {
    #[cfg(windows)]
    engine: nvidia::Cleanup,
    /// El cuadro acostado, cuando llega más alto de lo que aceptan los modelos.
    turned: Vec<u8>,
}

/// `ArtifactReduction` no carga con más de 1080 de alto. El teléfono vertical
/// (720x1280, 1080x1920) se transpone, se limpia acostado y se vuelve a transponer.
const CLEANUP_MAX_HEIGHT: usize = 1080;

/// Copia `src` (BGRA, `width`x`height`, paso `stride`) transpuesto en `dst`, con paso `height * 4`.
fn transpose_bgra(src: &[u8], width: usize, height: usize, stride: usize, dst: &mut [u8]) {
    let out_stride = height * 4;
    for y in 0..height {
        let row = &src[y * stride..y * stride + width * 4];
        for (x, px) in row.chunks_exact(4).enumerate() {
            let at = x * out_stride + y * 4;
            dst[at..at + 4].copy_from_slice(px);
        }
    }
}

/// Lo inverso de `transpose_bgra`: `src` tiene paso `height * 4` y `width` filas.
fn untranspose_bgra(src: &[u8], width: usize, height: usize, stride: usize, dst: &mut [u8]) {
    let in_stride = height * 4;
    for y in 0..height {
        let row = &mut dst[y * stride..y * stride + width * 4];
        for (x, px) in row.chunks_exact_mut(4).enumerate() {
            let at = x * in_stride + y * 4;
            px.copy_from_slice(&src[at..at + 4]);
        }
    }
}

impl ImageCleanup {
    /// Carga el runtime y los modelos. `strong` = limpieza fuerte, para Wi-Fi con poca señal.
    pub fn new(strong: bool) -> Result<Self, FxError> {
        match blur_availability() {
            BlurAvailability::NoRuntime => {
                return Err(FxError { message: "falta el runtime de NVIDIA Video Effects".into() });
            }
            BlurAvailability::NoNvidiaGpu => {
                return Err(FxError { message: "no hay una GPU NVIDIA".into() });
            }
            BlurAvailability::Available => {}
        }
        #[cfg(windows)]
        {
            return Ok(Self { engine: nvidia::Cleanup::start(strong)?, turned: Vec::new() });
        }
        #[cfg(not(windows))]
        {
            let _ = strong;
            Err(FxError { message: "la mejora de imagen solo corre en Windows".into() })
        }
    }

    /// Se aplica en el próximo cuadro; recarga los modelos.
    pub fn set_strong(&mut self, strong: bool) {
        #[cfg(windows)]
        self.engine.set_strong(strong);
        #[cfg(not(windows))]
        let _ = strong;
    }

    /// Aviso que no apaga el efecto, como dejar de quitar ruido para sostener 30 fps.
    pub fn notice(&self) -> Option<&str> {
        #[cfg(windows)]
        {
            return self.engine.notice();
        }
        #[cfg(not(windows))]
        None
    }

    pub fn apply(&mut self, bgra: &mut [u8], width: usize, height: usize, stride: usize) -> Result<(), FxError> {
        let row = width.saturating_mul(4);
        if width == 0 || height == 0 || stride < row || bgra.len() < stride.saturating_mul(height - 1) + row {
            return Err(FxError { message: "el cuadro no entra en el buffer".into() });
        }
        if height <= CLEANUP_MAX_HEIGHT {
            return self.run(bgra, width, height, stride);
        }
        if width > CLEANUP_MAX_HEIGHT {
            return Err(FxError { message: format!("la mejora de imagen no acepta {width}x{height}") });
        }
        let mut turned = std::mem::take(&mut self.turned);
        turned.resize(width * height * 4, 0);
        transpose_bgra(bgra, width, height, stride, &mut turned);
        let result = self.run(&mut turned, height, width, height * 4);
        if result.is_ok() {
            untranspose_bgra(&turned, width, height, stride, bgra);
        }
        self.turned = turned;
        result
    }

    fn run(&mut self, bgra: &mut [u8], width: usize, height: usize, stride: usize) -> Result<(), FxError> {
        #[cfg(windows)]
        {
            return self.engine.apply(bgra, width, height, stride);
        }
        #[cfg(not(windows))]
        {
            let _ = (bgra, width, height, stride);
            Err(FxError { message: "mejora de imagen apagada".into() })
        }
    }
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
    #[cfg(windows)]
    engine: nvidia::Engine,
}

impl BackgroundBlur {
    /// Carga el runtime y los modelos. Falla si esta máquina no puede desenfocar.
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
        #[cfg(windows)]
        {
            return Ok(Self { strength: 0.5, engine: nvidia::Engine::start()? });
        }
        #[cfg(not(windows))]
        Err(FxError { message: "el desenfoque solo corre en Windows".into() })
    }

    pub fn set_strength(&mut self, strength: f32) {
        self.strength = strength.clamp(0.0, 1.0);
        #[cfg(windows)]
        self.engine.set_strength(self.strength);
    }

    /// Aviso que no apaga el efecto, como el paso a modo rendimiento.
    pub fn notice(&self) -> Option<&str> {
        #[cfg(windows)]
        {
            return self.engine.notice();
        }
        #[cfg(not(windows))]
        None
    }

    /// Desenfoca el fondo del BGRA. `stride` es el paso de una fila, en bytes.
    pub fn apply(&mut self, bgra: &mut [u8], width: usize, height: usize, stride: usize) -> Result<(), FxError> {
        #[cfg(windows)]
        {
            return self.engine.apply(bgra, width, height, stride);
        }
        #[cfg(not(windows))]
        {
            let _ = (bgra, width, height, stride, self.strength);
            Err(FxError { message: "desenfoque apagado".into() })
        }
    }
}

#[cfg(windows)]
pub(crate) fn runtime_dll() -> Option<std::path::PathBuf> {
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

    #[cfg(windows)]
    #[test]
    #[ignore = "carga el runtime de NVIDIA; no forma parte de cargo test"]
    fn desenfoque_en_esta_gpu() {
        let mut fx = BackgroundBlur::new().expect("el runtime no arrancó");
        fx.set_strength(0.8);
        let (width, height) = (1280usize, 720usize);
        let mut bgra = vec![0u8; width * height * 4];
        for y in 0..height {
            for x in 0..width {
                let i = (y * width + x) * 4;
                bgra[i] = (x % 256) as u8;
                bgra[i + 1] = (y % 256) as u8;
                bgra[i + 2] = 40;
                bgra[i + 3] = 255;
            }
        }
        let before = bgra.clone();
        let mut steady = std::time::Duration::ZERO;
        let samples = 20u32;
        for n in 0..samples {
            let started = std::time::Instant::now();
            fx.apply(&mut bgra, width, height, width * 4).expect("apply");
            if n >= 2 {
                steady += started.elapsed();
            }
        }
        let avg = steady / (samples - 2);
        let mut delta = 0u64;
        for (a, b) in before.chunks_exact(4).zip(bgra.chunks_exact(4)) {
            delta += a[0].abs_diff(b[0]) as u64;
            delta += a[1].abs_diff(b[1]) as u64;
            delta += a[2].abs_diff(b[2]) as u64;
        }
        let mad = delta as f64 / (width * height * 3) as f64;
        eprintln!("mad={mad:.2} avg720={avg:?} notice={:?}", fx.notice());
        assert!(mad > 2.0, "el cambio parece una conversión, no un desenfoque");
        let mut hd = vec![0u8; 1920 * 1080 * 4];
        for (i, px) in hd.chunks_exact_mut(4).enumerate() {
            px[0] = (i % 251) as u8;
            px[1] = ((i / 3) % 251) as u8;
            px[2] = 90;
            px[3] = 255;
        }
        let t = std::time::Instant::now();
        fx.apply(&mut hd, 1920, 1080, 1920 * 4).expect("1080");
        let load = t.elapsed();
        let mut hd_steady = std::time::Duration::ZERO;
        for _ in 0..8 {
            let t = std::time::Instant::now();
            fx.apply(&mut hd, 1920, 1080, 1920 * 4).expect("1080");
            hd_steady += t.elapsed();
        }
        eprintln!("load1080={load:?} avg1080={:?}", hd_steady / 8);
    }

    fn frame(width: usize, height: usize, mut paint: impl FnMut(usize, usize) -> [u8; 3]) -> Vec<u8> {
        let mut bgra = vec![0u8; width * height * 4];
        for y in 0..height {
            for x in 0..width {
                let [b, g, r] = paint(x, y);
                let i = (y * width + x) * 4;
                bgra[i..i + 4].copy_from_slice(&[b, g, r, 255]);
            }
        }
        bgra
    }

    fn channel_means(bgra: &[u8]) -> [f64; 3] {
        let mut sums = [0f64; 3];
        for px in bgra.chunks_exact(4) {
            for c in 0..3 {
                sums[c] += px[c] as f64;
            }
        }
        let n = (bgra.len() / 4) as f64;
        [sums[0] / n, sums[1] / n, sums[2] / n]
    }

    /// Escena gris, con sombras y luces de punta a punta y bien expuesta.
    fn balanced(x: usize, y: usize) -> [u8; 3] {
        let v = ((x * 7 + y * 3) % 256) as u8;
        [v, v, v]
    }

    #[test]
    fn auto_no_toca_una_escena_ya_balanceada() {
        let mut auto = AutoColor::new();
        let original = frame(64, 48, balanced);
        let mut pixels = original.clone();
        auto.apply(&mut pixels, 64, 48, 64 * 4);
        let worst = original.iter().zip(&pixels).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
        assert!(worst <= 8, "una escena gris completa cambió {worst} niveles");
    }

    #[test]
    fn auto_saca_el_tinte_azul() {
        let mut auto = AutoColor::new();
        let mut pixels = frame(64, 48, |x, y| {
            let v = 40 + ((x * 5 + y * 2) % 160) as u8;
            [v.saturating_add(40), v, v.saturating_sub(30)]
        });
        let before = channel_means(&pixels);
        auto.apply(&mut pixels, 64, 48, 64 * 4);
        let after = channel_means(&pixels);
        // El tinte relativo al brillo: los niveles estiran todo y no deben contar como tinte.
        let tint = |m: [f64; 3]| (m[0] - m[2]) / m[1];
        assert!(tint(after) < tint(before) * 0.8, "{before:?} → {after:?}");
        assert!(after[0] > after[2], "corrige la mitad, no da vuelta el tinte");
    }

    #[test]
    fn auto_aclara_una_imagen_oscura_y_lavada() {
        let mut auto = AutoColor::new();
        let mut pixels = frame(64, 48, |x, y| {
            let v = 30 + ((x + y) % 60) as u8;
            [v, v, v]
        });
        let before = channel_means(&pixels)[1];
        auto.apply(&mut pixels, 64, 48, 64 * 4);
        let after = channel_means(&pixels)[1];
        assert!(after > before + 20.0, "la media pasó de {before} a {after}");
        let (lo, hi) = pixels.chunks_exact(4).fold((255u8, 0u8), |(lo, hi), px| (lo.min(px[1]), hi.max(px[1])));
        assert!(hi - lo > 100, "el rango quedó en {lo}..{hi}");
    }

    #[test]
    fn auto_cambia_de_a_poco_y_respeta_el_alfa_y_el_stride() {
        let mut auto = AutoColor::new();
        let (width, height, stride) = (32, 16, 32 * 4 + 12);
        let mut dark = vec![7u8; stride * height];
        let mut bright = vec![7u8; stride * height];
        for y in 0..height {
            for x in 0..width {
                let i = y * stride + x * 4;
                let v = ((x * 8 + y) % 256) as u8;
                dark[i..i + 4].copy_from_slice(&[v / 3, v / 3, v / 3, 77]);
                bright[i..i + 4].copy_from_slice(&[v, v, v, 77]);
            }
        }
        auto.apply(&mut dark.clone(), width, height, stride);
        let mut next = bright.clone();
        auto.apply(&mut next, width, height, stride);
        let mut first = bright.clone();
        AutoColor::new().apply(&mut first, width, height, stride);
        for y in 0..height {
            for x in 0..width {
                let i = y * stride + x * 4;
                assert_eq!(next[i + 3], 77);
                // Lo que viene de la escena oscura sigue pesando en el segundo cuadro.
                assert!(next[i + 1] >= first[i + 1]);
            }
            assert!(next[y * stride + width * 4..(y + 1) * stride].iter().all(|&b| b == 7), "tocó el relleno del stride");
        }
        assert_ne!(next, first);
        auto.reset();
        let mut again = bright.clone();
        auto.apply(&mut again, width, height, stride);
        assert_eq!(again, first, "después de reset el ajuste no arrastra el anterior");
    }

    #[test]
    fn transponer_y_volver_deja_el_cuadro_igual() {
        let (width, height, stride) = (3usize, 5usize, 3 * 4 + 4);
        let mut original = vec![9u8; stride * height];
        for y in 0..height {
            for x in 0..width {
                let i = y * stride + x * 4;
                original[i..i + 4].copy_from_slice(&[x as u8, y as u8, (x * 10 + y) as u8, 200]);
            }
        }
        let mut turned = vec![0u8; width * height * 4];
        transpose_bgra(&original, width, height, stride, &mut turned);
        // Acostado: ancho 5, alto 3. El píxel (x, y) pasa a (y, x).
        let at = |x: usize, y: usize| (y * height + x) * 4;
        assert_eq!(&turned[at(4, 2)..at(4, 2) + 4], &[2, 4, 24, 200]);
        let mut back = vec![9u8; stride * height];
        untranspose_bgra(&turned, width, height, stride, &mut back);
        assert_eq!(back, original);
    }

    #[test]
    fn auto_aguanta_cuadros_planos_y_vacios() {
        let mut auto = AutoColor::new();
        for value in [0u8, 128, 255] {
            let mut pixels = vec![value; 16 * 16 * 4];
            auto.reset();
            auto.apply(&mut pixels, 16, 16, 64);
            assert!(pixels.chunks_exact(4).all(|px| px[3] == value));
        }
        let mut empty: Vec<u8> = Vec::new();
        auto.apply(&mut empty, 0, 0, 0);
        auto.apply(&mut empty, 4, 4, 16);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "carga el runtime de NVIDIA; no forma parte de cargo test"]
    fn limpieza_en_esta_gpu() {
        for (width, height) in [(1280usize, 720usize), (1920, 1080), (720, 1280)] {
            let mut fx = ImageCleanup::new(false).expect("el runtime no arrancó");
            // Bloques de 8x8 con ruido: lo que deja H.264 con poca señal.
            let mut seed = 0x1234_5678u32;
            let mut bgra = frame(width, height, |x, y| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let block = (((x / 8) * 37 + (y / 8) * 11) % 64) as i32;
                let noise = (seed % 24) as i32 - 12;
                let v = (90 + block + noise).clamp(0, 255) as u8;
                [v, v.saturating_add(10), v.saturating_add(25)]
            });
            let before = bgra.clone();
            let t = std::time::Instant::now();
            fx.apply(&mut bgra, width, height, width * 4).expect("primer cuadro");
            let first = t.elapsed();
            let mut steady = std::time::Duration::ZERO;
            for _ in 0..10 {
                // Cada cuadro sale del original, como en vivo: si no, el efecto se acumula.
                bgra.copy_from_slice(&before);
                let t = std::time::Instant::now();
                fx.apply(&mut bgra, width, height, width * 4).expect("apply");
                steady += t.elapsed();
            }
            let mut delta = 0u64;
            for (a, b) in before.chunks_exact(4).zip(bgra.chunks_exact(4)) {
                delta += (0..3).map(|c| a[c].abs_diff(b[c]) as u64).sum::<u64>();
            }
            let mad = delta as f64 / (width * height * 3) as f64;
            let means = (channel_means(&before), channel_means(&bgra));
            eprintln!(
                "{width}x{height}: primero={first:?} promedio={:?} mad={mad:.2} medias {:?} → {:?} notice={:?}",
                steady / 10,
                means.0,
                means.1,
                fx.notice()
            );
            assert!(mad > 0.5, "la limpieza no cambió el cuadro");
            assert!((means.0[1] - means.1[1]).abs() < 12.0, "la limpieza movió el brillo");
            assert!(bgra.chunks_exact(4).all(|px| px[3] == 255));
            fx.set_strong(true);
            fx.apply(&mut bgra, width, height, width * 4).expect("fuerte");
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
