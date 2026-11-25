//! Cámara virtual `cellCam`. La DLL la carga Frame Server; la parte rlib la da de alta y de baja.
//! No decodifica H.264 y no abre el socket del teléfono.

mod media;

#[cfg(test)]
mod probe;

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::{Interface, GUID, HRESULT, PCWSTR};
use windows::Win32::Foundation::{E_POINTER, ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION, LocalFree, HLOCAL};
use windows::Win32::Media::MediaFoundation::{
    MFCreateVirtualCamera, MFShutdown, MFStartup, IMFVirtualCamera, MFSTARTUP_FULL, MFVirtualCameraAccess_CurrentUser,
    MFVirtualCameraLifetime_Session, MFVirtualCameraType_SoftwareCameraSource,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::Diagnostics::Debug::OutputDebugStringW;

const CLSID_TEXT: &str = "{8F4E2A11-6C3D-4B9E-9170-2A5B8C4D6E11}";

#[derive(Debug)]
pub struct VcamError {
    pub message: String,
}

impl std::fmt::Display for VcamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for VcamError {}

fn err(message: impl Into<String>) -> VcamError {
    VcamError { message: message.into() }
}

pub(crate) fn log_line(message: &str) {
    let wide: Vec<u16> = format!("cellcam: {message}\n").encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { OutputDebugStringW(PCWSTR(wide.as_ptr())) };
    let path = log_path();
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{message}");
    }
}

fn log_path() -> PathBuf {
    vcam_shm::default_path().with_file_name("cellcam-vcam.log")
}

fn installed_dll() -> PathBuf {
    vcam_shm::default_path().with_file_name("cellcam.dll")
}

fn source_dll() -> Result<PathBuf, VcamError> {
    if let Some(path) = std::env::var_os("CAMDESK_VCAM_DLL") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(err(format!("no está {}", path.display())));
    }
    let exe = std::env::current_exe().map_err(|error| err(error.to_string()))?;
    let dir = exe.parent().ok_or_else(|| err("no hay carpeta del ejecutable"))?;
    for candidate in [dir.join("vcam.dll"), dir.join("..\\vcam.dll")] {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(err("no está vcam.dll junto al ejecutable"))
}

pub struct VirtualCamera {
    camera: Option<IMFVirtualCamera>,
    com_owned: bool,
    mf_started: bool,
}

impl VirtualCamera {
    /// Copia la DLL a ProgramData, registra el CLSID en HKLM si falta, y deja la cámara viva.
    /// HKLM pide administrador la primera vez. Las siguientes no.
    pub fn create() -> Result<Self, VcamError> {
        let source = source_dll()?;
        let dest = installed_dll();
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|error| err(error.to_string()))?;
            let _ = grant(parent, file_rx(), true);
        }
        if source.canonicalize().ok() != dest.canonicalize().ok() {
            match std::fs::copy(&source, &dest) {
                Ok(_) => {}
                Err(error) if dest.is_file() && std::fs::read(&source).ok() == std::fs::read(&dest).ok() => {
                    log_line(&format!("la DLL está en uso y ya es la misma: {error}"));
                }
                Err(error)
                    if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION.0 as i32)
                        && dest.is_file()
                        && registered_path().as_deref() == dest.to_str() =>
                {
                    log_line("la DLL está en uso; se usa la copia registrada y queda pendiente actualizarla");
                }
                Err(error) => return Err(err(format!("no se pudo copiar la DLL: {error}"))),
            }
        }
        grant(&dest, file_rx(), false).map_err(|error| err(format!("ACL de la DLL: {error}")))?;
        let log = log_path();
        let _ = OpenOptions::new().create(true).append(true).open(&log);
        let _ = grant(&log, file_rw(), false);
        register_if_needed(&dest)?;

        let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let com_owned = com.is_ok();
        if com.is_err() && com != windows::Win32::Foundation::RPC_E_CHANGED_MODE {
            return Err(err(format!("COM {com:?}")));
        }
        let mf = unsafe { MFStartup(MFSTARTUP_FULL, 0) };
        if mf.is_err() {
            if com_owned {
                unsafe { CoUninitialize() };
            }
            return Err(err(format!("MFStartup {mf:?}")));
        }
        let guid = wide(CLSID_TEXT);
        let name = wide("cellCam");
        let camera = unsafe {
            MFCreateVirtualCamera(
                MFVirtualCameraType_SoftwareCameraSource,
                MFVirtualCameraLifetime_Session,
                MFVirtualCameraAccess_CurrentUser,
                PCWSTR(name.as_ptr()),
                PCWSTR(guid.as_ptr()),
                None,
            )
        };
        let camera = match camera {
            Ok(camera) => camera,
            Err(error) => {
                let _ = unsafe { MFShutdown() };
                if com_owned {
                    unsafe { CoUninitialize() };
                }
                return Err(err(format!("no se pudo crear la cámara: {error}")));
            }
        };
        if let Err(error) = unsafe { camera.Start(None::<&windows::Win32::Media::MediaFoundation::IMFAsyncCallback>) } {
            log_line(&format!("primer arranque: {error}"));
            let _ = unsafe { camera.Stop() };
            if let Err(error) = unsafe { camera.Start(None::<&windows::Win32::Media::MediaFoundation::IMFAsyncCallback>) } {
                let _ = unsafe { camera.Shutdown() };
                drop(camera);
                let _ = unsafe { MFShutdown() };
                if com_owned {
                    unsafe { CoUninitialize() };
                }
                return Err(err(format!("no se pudo iniciar la cámara: {error}")));
            }
        }
        log_line("cámara iniciada");
        Ok(Self { camera: Some(camera), com_owned, mf_started: true })
    }
}

impl Drop for VirtualCamera {
    fn drop(&mut self) {
        if let Some(camera) = self.camera.take() {
            let _ = unsafe { camera.Stop() };
            let _ = unsafe { camera.Remove() };
            let _ = unsafe { camera.Shutdown() };
        }
        if self.mf_started {
            let _ = unsafe { MFShutdown() };
        }
        if self.com_owned {
            unsafe { CoUninitialize() };
        }
    }
}

fn register_if_needed(dll: &Path) -> Result<(), VcamError> {
    let dll_text = dll.to_string_lossy().to_string();
    if registered_path().as_deref() == Some(dll_text.as_str()) {
        return Ok(());
    }
    register_server(dll).map_err(|error| {
        if error.code == ERROR_ACCESS_DENIED.0 {
            err("hace falta registrar cellCam una vez como administrador")
        } else {
            err(format!("no se pudo registrar el CLSID: {}", error.message))
        }
    })
}

struct RegError {
    code: u32,
    message: String,
}

fn registered_path() -> Option<String> {
    let subkey = wide(&format!("Software\\Classes\\CLSID\\{CLSID_TEXT}\\InprocServer32"));
    unsafe {
        let mut key = windows::Win32::System::Registry::HKEY::default();
        let rc = windows::Win32::System::Registry::RegOpenKeyExW(
            windows::Win32::System::Registry::HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            None,
            windows::Win32::System::Registry::KEY_QUERY_VALUE,
            &mut key,
        );
        if rc.0 != 0 {
            return None;
        }
        let mut bytes = vec![0u8; 1024];
        let mut size = bytes.len() as u32;
        let mut kind = windows::Win32::System::Registry::REG_NONE;
        let rc = windows::Win32::System::Registry::RegGetValueW(
            key,
            None,
            None,
            windows::Win32::System::Registry::RRF_RT_REG_SZ,
            Some(&mut kind),
            Some(bytes.as_mut_ptr() as *mut _),
            Some(&mut size),
        );
        let _ = windows::Win32::System::Registry::RegCloseKey(key);
        if rc.0 != 0 || size < 2 {
            return None;
        }
        let wide = std::slice::from_raw_parts(bytes.as_ptr() as *const u16, (size as usize / 2).saturating_sub(1));
        Some(String::from_utf16_lossy(wide))
    }
}

fn register_server(dll: &Path) -> Result<(), RegError> {
    let clsid = wide(&format!("Software\\Classes\\CLSID\\{CLSID_TEXT}"));
    let inproc = wide(&format!("Software\\Classes\\CLSID\\{CLSID_TEXT}\\InprocServer32"));
    unsafe {
        let key = create_key(&clsid)?;
        set_sz(key, None, "cellCam")?;
        let _ = windows::Win32::System::Registry::RegCloseKey(key);
        let key = create_key(&inproc)?;
        set_sz(key, None, &dll.to_string_lossy())?;
        set_sz(key, Some("ThreadingModel"), "Both")?;
        let _ = windows::Win32::System::Registry::RegCloseKey(key);
    }
    Ok(())
}

unsafe fn create_key(path: &[u16]) -> Result<windows::Win32::System::Registry::HKEY, RegError> {
    let mut key = windows::Win32::System::Registry::HKEY::default();
    let mut disposition = windows::Win32::System::Registry::REG_CREATE_KEY_DISPOSITION::default();
    let rc = windows::Win32::System::Registry::RegCreateKeyExW(
        windows::Win32::System::Registry::HKEY_LOCAL_MACHINE,
        PCWSTR(path.as_ptr()),
        None,
        PCWSTR::null(),
        windows::Win32::System::Registry::REG_OPTION_NON_VOLATILE,
        windows::Win32::System::Registry::KEY_SET_VALUE,
        None,
        &mut key,
        Some(&mut disposition),
    );
    if rc.0 != 0 {
        Err(RegError { code: rc.0, message: format!("registro {}", rc.0) })
    } else {
        Ok(key)
    }
}

unsafe fn set_sz(key: windows::Win32::System::Registry::HKEY, name: Option<&str>, value: &str) -> Result<(), RegError> {
    let value = wide(value);
    let bytes = std::slice::from_raw_parts(value.as_ptr() as *const u8, value.len() * 2);
    let name_wide;
    let name_ptr = if let Some(name) = name {
        name_wide = wide(name);
        PCWSTR(name_wide.as_ptr())
    } else {
        PCWSTR::null()
    };
    let rc = windows::Win32::System::Registry::RegSetValueExW(
        key,
        name_ptr,
        None,
        windows::Win32::System::Registry::REG_SZ,
        Some(bytes),
    );
    if rc.0 != 0 {
        Err(RegError { code: rc.0, message: format!("valor {}", rc.0) })
    } else {
        Ok(())
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn file_rx() -> u32 {
    windows::Win32::Storage::FileSystem::FILE_GENERIC_READ.0 | windows::Win32::Storage::FileSystem::FILE_GENERIC_EXECUTE.0
}

fn file_rw() -> u32 {
    windows::Win32::Storage::FileSystem::FILE_GENERIC_READ.0 | windows::Win32::Storage::FileSystem::FILE_GENERIC_WRITE.0
}

fn grant(path: &Path, access: u32, inherit: bool) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Security::Authorization::{
        ConvertStringSidToSidW, GetNamedSecurityInfoW, SetEntriesInAclW, SetNamedSecurityInfoW, EXPLICIT_ACCESS_W,
        GRANT_ACCESS, SE_FILE_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
    };
    use windows::Win32::Security::{
        ACE_FLAGS, DACL_SECURITY_INFORMATION, NO_INHERITANCE, OBJECT_INHERIT_ACE, CONTAINER_INHERIT_ACE, PSECURITY_DESCRIPTOR, ACL,
    };
    let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let sid_text = wide("S-1-5-19");
    unsafe {
        let mut sid = windows::Win32::Security::PSID::default();
        ConvertStringSidToSidW(PCWSTR(sid_text.as_ptr()), &mut sid).map_err(|error| std::io::Error::from_raw_os_error(error.code().0))?;
        let mut old_dacl = std::ptr::null_mut::<ACL>();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        let rc = GetNamedSecurityInfoW(
            PCWSTR(path_wide.as_ptr()),
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
            return Err(std::io::Error::from_raw_os_error(rc.0 as i32));
        }
        let inheritance = if inherit {
            ACE_FLAGS(OBJECT_INHERIT_ACE.0 | CONTAINER_INHERIT_ACE.0)
        } else {
            NO_INHERITANCE
        };
        let entry = EXPLICIT_ACCESS_W {
            grfAccessPermissions: access,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: inheritance,
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
            return Err(std::io::Error::from_raw_os_error(rc.0 as i32));
        }
        let rc = SetNamedSecurityInfoW(
            PCWSTR(path_wide.as_ptr()),
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
            return Err(std::io::Error::from_raw_os_error(rc.0 as i32));
        }
    }
    Ok(())
}

#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(clsid: *const GUID, riid: *const GUID, object: *mut *mut core::ffi::c_void) -> HRESULT {
    static LOGGED: AtomicBool = AtomicBool::new(false);
    if !LOGGED.swap(true, Ordering::AcqRel) {
        log_line("carga de la DLL");
    }
    if object.is_null() {
        return E_POINTER;
    }
    unsafe { *object = std::ptr::null_mut() };
    if clsid.is_null() || unsafe { *clsid } != CLSID {
        return windows::Win32::Foundation::CLASS_E_CLASSNOTAVAILABLE;
    }
    let factory = media::class_factory();
    unsafe { factory.query(riid, object) }
}

#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if media::can_unload() {
        HRESULT(0)
    } else {
        windows::Win32::Foundation::S_FALSE
    }
}

const CLSID: GUID = GUID::from_u128(0x8f4e2a11_6c3d_4b9e_9170_2a5b8c4d6e11);
