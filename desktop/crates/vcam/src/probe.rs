use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use vcam_shm::{Writer, HEIGHT, STRIDE, WIDTH};
use windows::core::{Interface, PCWSTR, PWSTR};
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, MFCreateAttributes, MFCreateDeviceSource, MFCreateSourceReaderFromMediaSource, MFEnumDeviceSources,
    IMFActivate, IMFAttributes, MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM,
};
use windows::Win32::System::Com::CoTaskMemFree;

use crate::VirtualCamera;

fn attributes(count: u32) -> IMFAttributes {
    let mut attributes = None;
    unsafe { MFCreateAttributes(&mut attributes, count).expect("atributos") };
    attributes.expect("atributos")
}

fn wide_text(pointer: PWSTR) -> String {
    if pointer.0.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    unsafe {
        while *pointer.0.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(pointer.0, len))
    }
}

fn allocated_string(activate: &IMFActivate, key: &windows::core::GUID) -> Option<String> {
    let mut pointer = PWSTR::null();
    let mut len = 0u32;
    unsafe {
        activate.GetAllocatedString(key, &mut pointer, &mut len).ok()?;
        let text = wide_text(pointer);
        CoTaskMemFree(Some(pointer.0 as *const _));
        Some(text)
    }
}

fn bar_column(sample: &windows::Win32::Media::MediaFoundation::IMFSample) -> Option<i32> {
    let buffer = unsafe { sample.GetBufferByIndex(0).ok()? };
    let two_d: IMF2DBuffer = buffer.cast().ok()?;
    let mut scan = std::ptr::null_mut();
    let mut pitch = 0i32;
    unsafe { two_d.Lock2D(&mut scan, &mut pitch).ok()? };
    let column = if scan.is_null() || pitch < WIDTH as i32 {
        None
    } else {
        let row = unsafe { scan.add(540 * pitch as usize) };
        let mut found = None;
        for x in 0..WIDTH {
            if unsafe { *row.add(x as usize) } == 235 {
                found = Some(x as i32);
                break;
            }
        }
        found
    };
    let _ = unsafe { two_d.Unlock2D() };
    column
}

fn paint(column: u32, y: &mut [u8], uv: &mut [u8]) {
    y.fill(16);
    uv.fill(128);
    for row in 0..HEIGHT as usize {
        y[row * STRIDE as usize + column as usize] = 235;
    }
}

fn find_cellcam() -> Option<(String, Vec<u16>)> {
    let attributes = attributes(1);
    unsafe {
        attributes
            .SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)
            .expect("tipo");
    }
    let mut list = std::ptr::null_mut();
    let mut count = 0u32;
    unsafe { MFEnumDeviceSources(&attributes, &mut list, &mut count).expect("enumerar") };
    let mut found = None;
    if !list.is_null() {
        let devices = unsafe { std::slice::from_raw_parts(list, count as usize) };
        for device in devices {
            if let Some(activate) = device {
                let name = allocated_string(activate, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME).unwrap_or_default();
                println!("camara={name}");
                let _ = std::io::Write::flush(&mut std::io::stdout());
                if found.is_none() && name.to_lowercase().contains("cellcam") {
                    if let Some(link) = allocated_string(activate, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK) {
                        let mut wide: Vec<u16> = link.encode_utf16().collect();
                        wide.push(0);
                        found = Some((name, wide));
                    }
                }
            }
        }
        for device in devices {
            drop(unsafe { std::ptr::read(device) });
        }
        unsafe { CoTaskMemFree(Some(list as *const _)) };
    }
    found
}

fn read_before_and_after(link: &[u16], between: impl FnOnce()) -> Result<(Vec<i32>, Vec<i32>), String> {
    let attributes = attributes(2);
    unsafe {
        attributes
            .SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)
            .map_err(|error| error.to_string())?;
        attributes
            .SetString(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, PCWSTR(link.as_ptr()))
            .map_err(|error| error.to_string())?;
    }
    let source = unsafe { MFCreateDeviceSource(&attributes).map_err(|error| format!("fuente {error}"))? };
    let first;
    let second;
    {
        let reader = unsafe { MFCreateSourceReaderFromMediaSource(&source, None).map_err(|error| format!("lector {error}"))? };
        first = collect(&reader, 4)?;
        between();
        second = collect(&reader, 4)?;
        let _ = unsafe { source.Shutdown() };
    }
    Ok((first, second))
}

fn collect(reader: &windows::Win32::Media::MediaFoundation::IMFSourceReader, samples: usize) -> Result<Vec<i32>, String> {
    let mut columns = Vec::new();
    let mut index = 0;
    while columns.len() < samples && index < 12 {
        let mut stream = 0u32;
        let mut flags = 0u32;
        let mut time = 0i64;
        let mut sample = None;
        unsafe {
            reader
                .ReadSample(
                    MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                    0,
                    Some(&mut stream),
                    Some(&mut flags),
                    Some(&mut time),
                    Some(&mut sample),
                )
                .map_err(|error| format!("muestra {index} {error}"))?;
        }
        println!("muestra={index} flags={flags} time={time} hay={}", sample.is_some());
        let _ = std::io::Write::flush(&mut std::io::stdout());
        if let Some(sample) = sample.as_ref() {
            if let Some(column) = bar_column(sample) {
                println!("columna={column}");
                let _ = std::io::Write::flush(&mut std::io::stdout());
                columns.push(column);
            }
        }
        index += 1;
    }
    Ok(columns)
}

#[test]
#[ignore = "abre la cámara virtual, registra HKLM y pide administrador la primera vez"]
fn la_barra_se_mueve() {
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../target/{profile}"));
    let mut dll = root.join("vcam.dll");
    if let Ok(entries) = std::fs::read_dir(root.join("build").join("vcam")) {
        for entry in entries.flatten() {
            let candidate = entry.path().join("out").join("vcam.dll");
            if candidate.is_file()
                && candidate.metadata().ok().map(|meta| meta.modified().ok()) > dll.metadata().ok().map(|meta| meta.modified().ok())
            {
                dll = candidate;
            }
        }
    }
    assert!(dll.is_file(), "no está {}", dll.display());
    println!("dll={}", dll.display());
    std::env::set_var("CAMDESK_VCAM_DLL", &dll);

    let mut writer = match Writer::create(&vcam_shm::default_path()) {
        Ok(writer) => writer,
        Err(error) if error.raw_os_error() == Some(1224) => Writer::open_existing(&vcam_shm::default_path()).expect("archivo ya mapeado"),
        Err(error) => panic!("archivo mapeado: {error}"),
    };
    writer.write(0, |y, uv| paint(200, y, uv)).expect("barra");

    let camera = VirtualCamera::create().expect("cámara");
    thread::sleep(Duration::from_millis(1500));
    let found = find_cellcam().ok_or_else(|| "Windows no listó cellCam".to_string());
    let read = found.and_then(|(_name, link)| {
        read_before_and_after(&link, || {
            writer.write(1, |y, uv| paint(900, y, uv)).expect("mover");
            thread::sleep(Duration::from_millis(400));
        })
    });
    drop(camera);
    drop(writer);
    let (first, second) = read.expect("lectura");

    println!("columnas={first:?} {second:?}");
    let left = *first.first().expect("sin muestras");
    let right = *second.last().expect("sin muestras después de mover");
    assert_ne!(left, right, "la barra no se movió");
    assert!(left > 0 && right > 0);
}
