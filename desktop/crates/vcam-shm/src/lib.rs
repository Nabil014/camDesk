//! Archivo mapeado de la cámara virtual. No llama a Media Foundation.
//!
//! El lector mapea el archivo de solo lectura y solo hace cargas atómicas.
//! Una operación que escribe (compare-exchange) tira a Frame Server.

use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};

use memmap2::{Mmap, MmapMut};

pub const WIDTH: u32 = 1920;
pub const HEIGHT: u32 = 1080;
pub const STRIDE: u32 = 1920;
pub const MAGIC: u32 = 0x4356_4331;
pub const VERSION: u32 = 1;

pub const HEADER_BYTES: usize = 64;
pub const SLOT_BYTES: usize = (STRIDE as usize) * (HEIGHT as usize) * 3 / 2;
pub const Y_BYTES: usize = (STRIDE as usize) * (HEIGHT as usize);
pub const UV_BYTES: usize = Y_BYTES / 2;
pub const FILE_BYTES: usize = HEADER_BYTES + SLOT_BYTES * 2;

const OFF_ACTIVE: usize = 20;
const OFF_SEQ0: usize = 24;
const OFF_PTS0: usize = 32;
const OFF_SEQ1: usize = 40;
const OFF_PTS1: usize = 48;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameInfo {
    pub pts_100ns: i64,
    pub sequence: u64,
}

pub fn default_path() -> PathBuf {
    #[cfg(windows)]
    {
        let base = std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        return base.join("Camdesk").join("cellcam-vcam.bin");
    }
    #[cfg(not(windows))]
    {
        std::env::temp_dir().join("Camdesk").join("cellcam-vcam.bin")
    }
}

pub struct Writer {
    _file: std::fs::File,
    map: MmapMut,
    /// Última sequence par publicada. El siguiente frame usa este valor + 2.
    /// Es global, no por slot: si cada slot suma 2 por su cuenta, los dos
    /// quedan en el mismo número y el lector repite el cuadro anterior.
    next_seq: u64,
}

impl Writer {
    pub fn create(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        file.set_len(FILE_BYTES as u64)?;
        grant_local_service_read(path)?;
        let mut map = unsafe { MmapMut::map_mut(&file)? };
        map.fill(0);
        map[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        map[4..8].copy_from_slice(&VERSION.to_le_bytes());
        map[8..12].copy_from_slice(&WIDTH.to_le_bytes());
        map[12..16].copy_from_slice(&HEIGHT.to_le_bytes());
        map[16..20].copy_from_slice(&STRIDE.to_le_bytes());
        for slot in 0..2 {
            paint_black(&mut map[slot_range(slot)]);
        }
        unsafe {
            seq_mut(map.as_mut_ptr(), 0).store(2, Ordering::Release);
            seq_mut(map.as_mut_ptr(), 1).store(2, Ordering::Release);
            active_mut(map.as_mut_ptr()).store(0, Ordering::Release);
        }
        map.flush()?;
        Ok(Self { _file: file, map, next_seq: 2 })
    }

    /// Abre un archivo que ya tiene el tamaño y la cabecera. No lo trunca:
    /// otro proceso puede tenerlo mapeado.
    pub fn open_existing(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        if file.metadata()?.len() < FILE_BYTES as u64 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "archivo corto"));
        }
        let map = unsafe { MmapMut::map_mut(&file)? };
        if !header_ok(&map) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "cabecera"));
        }
        let next_seq = unsafe {
            let base = map.as_ptr() as *mut u8;
            let highest = seq_mut(base, 0).load(Ordering::Acquire).max(seq_mut(base, 1).load(Ordering::Acquire));
            highest + (highest & 1)
        };
        Ok(Self { _file: file, map, next_seq })
    }

    pub fn write<F>(&mut self, pts_100ns: i64, fill: F) -> io::Result<()>
    where
        F: FnOnce(&mut [u8], &mut [u8]),
    {
        let base = self.map.as_mut_ptr();
        let slot = unsafe {
            let active = active_mut(base).load(Ordering::Acquire);
            if active == 0 { 1 } else { 0 }
        };
        let published = self.next_seq.wrapping_add(2);
        self.next_seq = published;
        unsafe {
            seq_mut(base, slot).store(published - 1, Ordering::Release);
        }
        {
            let range = slot_range(slot);
            let slot_bytes = &mut self.map[range];
            let (y, uv) = slot_bytes.split_at_mut(Y_BYTES);
            fill(y, uv);
        }
        unsafe {
            pts_mut(base, slot).store(pts_100ns, Ordering::Release);
            seq_mut(base, slot).store(published, Ordering::Release);
            active_mut(base).store(slot as u32, Ordering::Release);
        }
        Ok(())
    }
}

pub struct Reader {
    _file: std::fs::File,
    map: Mmap,
    last_y: Vec<u8>,
    last_uv: Vec<u8>,
    last_info: Option<FrameInfo>,
    valid: bool,
}

impl Reader {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).open(path)?;
        let meta = file.metadata()?;
        if meta.len() < FILE_BYTES as u64 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "archivo corto"));
        }
        let map = unsafe { Mmap::map(&file)? };
        let valid = header_ok(&map);
        Ok(Self {
            _file: file,
            map,
            last_y: vec![16; Y_BYTES],
            last_uv: vec![128; UV_BYTES],
            last_info: None,
            valid,
        })
    }

    pub fn read_latest(&mut self, y: &mut [u8], uv: &mut [u8]) -> Option<FrameInfo> {
        if y.len() != Y_BYTES || uv.len() != UV_BYTES {
            return None;
        }
        if !self.valid || !header_ok(&self.map) {
            y.fill(16);
            uv.fill(128);
            let info = FrameInfo { pts_100ns: 0, sequence: 0 };
            self.remember(y, uv, info);
            return Some(info);
        }
        let base = self.map.as_ptr() as *mut u8;
        for _ in 0..3 {
            let slot = unsafe { active_mut(base).load(Ordering::Acquire) };
            if slot > 1 {
                continue;
            }
            let seq_a = unsafe { seq_mut(base, slot as usize).load(Ordering::Acquire) };
            if seq_a & 1 != 0 {
                continue;
            }
            let pts = unsafe { pts_mut(base, slot as usize).load(Ordering::Acquire) };
            let range = slot_range(slot as usize);
            let src = &self.map[range];
            y.copy_from_slice(&src[..Y_BYTES]);
            uv.copy_from_slice(&src[Y_BYTES..]);
            std::sync::atomic::fence(Ordering::Acquire);
            let seq_b = unsafe { seq_mut(base, slot as usize).load(Ordering::Acquire) };
            if seq_a == seq_b {
                if self.last_info.map(|info| info.sequence) == Some(seq_a) {
                    y.copy_from_slice(&self.last_y);
                    uv.copy_from_slice(&self.last_uv);
                    return self.last_info;
                }
                let info = FrameInfo { pts_100ns: pts, sequence: seq_a };
                self.remember(y, uv, info);
                return Some(info);
            }
        }
        y.copy_from_slice(&self.last_y);
        uv.copy_from_slice(&self.last_uv);
        self.last_info.or(Some(FrameInfo { pts_100ns: 0, sequence: 0 }))
    }

    fn remember(&mut self, y: &[u8], uv: &[u8], info: FrameInfo) {
        self.last_y.copy_from_slice(y);
        self.last_uv.copy_from_slice(uv);
        self.last_info = Some(info);
    }
}

fn header_ok(map: &[u8]) -> bool {
    if map.len() < FILE_BYTES {
        return false;
    }
    let magic = u32::from_le_bytes(map[0..4].try_into().unwrap());
    let version = u32::from_le_bytes(map[4..8].try_into().unwrap());
    let width = u32::from_le_bytes(map[8..12].try_into().unwrap());
    let height = u32::from_le_bytes(map[12..16].try_into().unwrap());
    let stride = u32::from_le_bytes(map[16..20].try_into().unwrap());
    magic == MAGIC && version == VERSION && width == WIDTH && height == HEIGHT && stride == STRIDE
}

fn slot_range(slot: usize) -> std::ops::Range<usize> {
    let start = HEADER_BYTES + slot * SLOT_BYTES;
    start..start + SLOT_BYTES
}

fn paint_black(slot: &mut [u8]) {
    slot[..Y_BYTES].fill(16);
    slot[Y_BYTES..].fill(128);
}

unsafe fn active_mut(base: *mut u8) -> &'static AtomicU32 {
    unsafe { AtomicU32::from_ptr(base.add(OFF_ACTIVE) as *mut u32) }
}

unsafe fn seq_mut(base: *mut u8, slot: usize) -> &'static AtomicU64 {
    let offset = if slot == 0 { OFF_SEQ0 } else { OFF_SEQ1 };
    unsafe { AtomicU64::from_ptr(base.add(offset) as *mut u64) }
}

unsafe fn pts_mut(base: *mut u8, slot: usize) -> &'static AtomicI64 {
    let offset = if slot == 0 { OFF_PTS0 } else { OFF_PTS1 };
    unsafe { AtomicI64::from_ptr(base.add(offset) as *mut i64) }
}

#[cfg(windows)]
fn grant_local_service_read(path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Authorization::{
        ConvertStringSidToSidW, GetNamedSecurityInfoW, SetEntriesInAclW, SetNamedSecurityInfoW,
        EXPLICIT_ACCESS_W, GRANT_ACCESS, SE_FILE_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP,
        TRUSTEE_W,
    };
    use windows::Win32::Security::{DACL_SECURITY_INFORMATION, NO_INHERITANCE, PSECURITY_DESCRIPTOR, ACL};
    use windows::Win32::Storage::FileSystem::FILE_GENERIC_READ;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let sid_text: Vec<u16> = "S-1-5-19".encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let mut sid = windows::Win32::Security::PSID::default();
        ConvertStringSidToSidW(PCWSTR(sid_text.as_ptr()), &mut sid)
            .map_err(|err| io::Error::from_raw_os_error(err.code().0))?;
        let name = PCWSTR(wide.as_ptr());
        let mut old_dacl = std::ptr::null_mut::<ACL>();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        let rc = GetNamedSecurityInfoW(
            name,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut old_dacl),
            None,
            &mut sd,
        );
        if rc.0 != 0 {
            let _ = LocalFree(Some(HLOCAL(sid.0 as *mut _)));
            return Err(io::Error::from_raw_os_error(rc.0 as i32));
        }
        let entry = EXPLICIT_ACCESS_W {
            grfAccessPermissions: FILE_GENERIC_READ.0,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: NO_INHERITANCE,
            Trustee: TRUSTEE_W {
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
                ptstrName: windows::core::PWSTR(sid.0 as *mut u16),
                ..Default::default()
            },
        };
        let mut new_acl = std::ptr::null_mut::<ACL>();
        let old = if old_dacl.is_null() { None } else { Some(old_dacl as *const ACL) };
        let rc = SetEntriesInAclW(Some(std::slice::from_ref(&entry)), old, &mut new_acl);
        if rc.0 != 0 {
            let _ = LocalFree(Some(HLOCAL(sd.0)));
            let _ = LocalFree(Some(HLOCAL(sid.0 as *mut _)));
            return Err(io::Error::from_raw_os_error(rc.0 as i32));
        }
        let rc = SetNamedSecurityInfoW(
            name,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(new_acl),
            None,
        );
        let _ = LocalFree(Some(HLOCAL(new_acl as *mut _)));
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        let _ = LocalFree(Some(HLOCAL(sid.0 as *mut _)));
        if rc.0 != 0 {
            return Err(io::Error::from_raw_os_error(rc.0 as i32));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn grant_local_service_read(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};

    #[test]
    fn el_archivo_nace_en_negro() {
        let path = std::env::temp_dir().join(format!("camdesk-shm-negro-{}.bin", std::process::id()));
        let _writer = Writer::create(&path).unwrap();
        let mut reader = Reader::open(&path).unwrap();
        let mut y = vec![0; Y_BYTES];
        let mut uv = vec![0; UV_BYTES];
        let info = reader.read_latest(&mut y, &mut uv).unwrap();
        assert!(y.iter().all(|b| *b == 16));
        assert!(uv.iter().all(|b| *b == 128));
        assert_eq!(info.sequence, 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn lector_y_escritor_no_entregan_un_frame_mezclado() {
        let path = std::env::temp_dir().join(format!("camdesk-shm-mix-{}.bin", std::process::id()));
        let mut writer = Writer::create(&path).unwrap();
        let mut reader = Reader::open(&path).unwrap();
        writer
            .write(99, |y, uv| {
                y.fill(7);
                uv.fill(128);
            })
            .unwrap();
        let mut y = vec![0; Y_BYTES];
        let mut uv = vec![0; UV_BYTES];
        let info = reader.read_latest(&mut y, &mut uv).unwrap();
        assert_eq!(y[0], 7, "no se ve la escritura seq={}", info.sequence);
        writer
            .write(100, |y, uv| {
                y.fill(9);
                uv.fill(128);
            })
            .unwrap();
        let info = reader.read_latest(&mut y, &mut uv).unwrap();
        assert_eq!(y[0], 9, "el segundo frame se repitió como el primero seq={}", info.sequence);
        assert_ne!(info.sequence, 4);
        let barrier = Arc::new(Barrier::new(2));
        let barrier_w = barrier.clone();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_w = stop.clone();
        let join = std::thread::spawn(move || {
            barrier_w.wait();
            let mut n = 0u8;
            while !stop_w.load(Ordering::Relaxed) {
                let mark = n;
                writer
                    .write(n as i64, |y, uv| {
                        y.fill(mark);
                        uv.fill(128);
                    })
                    .unwrap();
                n = n.wrapping_add(1);
                if n == 0 {
                    n = 1;
                }
                // El hueco deja el seqlock en par. A 30 fps pasa lo mismo; sin pausa el lector siempre cae en medio de la escritura.
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        barrier.wait();
        let deadline = Instant::now() + Duration::from_millis(400);
        let mut marks = std::collections::BTreeSet::new();
        while Instant::now() < deadline {
            let info = reader.read_latest(&mut y, &mut uv).unwrap();
            let first = y[0];
            assert!(y.iter().all(|b| *b == first), "frame mezclado seq {}", info.sequence);
            assert!(uv.iter().all(|b| *b == 128));
            marks.insert(first);
        }
        stop.store(true, Ordering::Relaxed);
        join.join().unwrap();
        assert!(marks.len() > 3, "el lector vio pocas imágenes distintas: {marks:?}");
        let _ = std::fs::remove_file(&path);
    }
}
