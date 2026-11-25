//! Fuente NV12 que Frame Server carga como LOCAL SERVICE.
//! Lee el archivo mapeado con `vcam_shm::Reader` y no escribe en esa vista.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use vcam_shm::{Reader, HEIGHT, SLOT_BYTES, STRIDE, WIDTH, Y_BYTES};
use windows::core::{implement, Interface, GUID, HRESULT, IUnknown, PCWSTR, PWSTR, BOOL};
use windows::Win32::Foundation::{CLASS_E_NOAGGREGATION, E_FAIL, E_INVALIDARG, E_NOTIMPL, E_POINTER, ERROR_SET_NOT_FOUND};
use windows::Win32::Media::KernelStreaming::{IKsControl, IKsControl_Impl, KSCATEGORY_VIDEO_CAMERA, KSIDENTIFIER, PINNAME_VIDEO_CAPTURE};
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFActivate_Impl, IMFAttributes, IMFAttributes_Impl, IMFGetService, IMFGetService_Impl, IMFMediaBuffer,
    IMFMediaEvent, IMFMediaEventQueue, IMFMediaSource, IMFMediaSourceEx, IMFMediaSourceEx_Impl, IMFMediaStream2,
    IMFMediaStream2_Impl, IMFMediaType, IMFPresentationDescriptor, IMFStreamDescriptor,
    MFCreate2DMediaBuffer, MFCreateAttributes, MFCreateEventQueue, MFCreateMediaType, MFCreatePresentationDescriptor,
    MFGetSystemTime,
    MFCreateSample, MFCreateStreamDescriptor, MFFrameSourceTypes_Color, MFMEDIASOURCE_IS_LIVE, MFSampleExtension_CleanPoint,
    MFSampleExtension_Token, MFVideoFormat_NV12, MFVideoInterlace_Progressive, MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES,
    MF_DEVICESTREAM_FRAMESERVER_SHARED, MF_DEVICESTREAM_STREAM_CATEGORY, MF_DEVICESTREAM_STREAM_ID,
    MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_CATEGORY,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID, MF_E_INVALID_STATE_TRANSITION, MF_E_MEDIA_SOURCE_WRONGSTATE, MF_E_SHUTDOWN,
    MF_E_UNSUPPORTED_SERVICE, MF_E_UNSUPPORTED_TIME_FORMAT, MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_DEFAULT_STRIDE,
    MF_MT_FIXED_SIZE_SAMPLES, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_PIXEL_ASPECT_RATIO,
    MF_MT_SAMPLE_SIZE, MF_MT_SUBTYPE, MF_STREAM_STATE, MF_STREAM_STATE_RUNNING, MF_STREAM_STATE_STOPPED, MENewStream,
    MESourceStarted, MESourceStopped, MEStreamStarted, MEStreamStopped, MEUpdatedStream, MFMediaType_Video,
    MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS, MF_ATTRIBUTE_TYPE, MF_ATTRIBUTES_MATCH_TYPE,
};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl, StructuredStorage::PROPVARIANT};
use windows::Win32::System::Variant::VT_I8;

use crate::log_line;

const NV12_FOURCC: u32 = u32::from_le_bytes(*b"NV12");

static OBJECTS: AtomicI32 = AtomicI32::new(0);
static LOCKS: AtomicI32 = AtomicI32::new(0);

pub fn can_unload() -> bool {
    OBJECTS.load(Ordering::Acquire) == 0 && LOCKS.load(Ordering::Acquire) == 0
}

struct Track;
impl Track {
    fn new() -> Self {
        OBJECTS.fetch_add(1, Ordering::Release);
        Self
    }
}
impl Drop for Track {
    fn drop(&mut self) {
        OBJECTS.fetch_sub(1, Ordering::Release);
    }
}

fn fail(code: HRESULT) -> windows::core::Error {
    windows::core::Error::from(code)
}

fn win32_error(code: u32) -> windows::core::Error {
    let hr = if code == 0 { 0 } else { ((code & 0xFFFF) | 0x80070000) as i32 };
    fail(HRESULT(hr))
}

fn create_attributes(count: u32) -> windows::core::Result<IMFAttributes> {
    let mut attributes = None;
    unsafe { MFCreateAttributes(&mut attributes, count)? };
    attributes.ok_or_else(|| fail(E_FAIL))
}

fn guid_ref(guid: *const GUID) -> &'static GUID {
    unsafe { &*guid }
}

fn pack_size(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | low as u64
}

fn int64_variant(value: i64) -> PROPVARIANT {
    #[repr(C)]
    struct Packed {
        vt: u16,
        reserved1: u16,
        reserved2: u16,
        reserved3: u16,
        value: i64,
    }
    let packed = Packed {
        vt: VT_I8.0,
        reserved1: 0,
        reserved2: 0,
        reserved3: 0,
        value,
    };
    let mut prop = PROPVARIANT::default();
    unsafe {
        std::ptr::copy_nonoverlapping(
            &packed as *const Packed as *const u8,
            &mut prop as *mut PROPVARIANT as *mut u8,
            std::mem::size_of::<Packed>(),
        );
    }
    prop
}

fn start_origin(position: *const PROPVARIANT) -> i64 {
    if position.is_null() {
        log_line("posicion nula");
        return 0;
    }
    #[repr(C)]
    struct Packed {
        vt: u16,
        _reserved: [u16; 3],
        value: i64,
    }
    let packed = unsafe { std::ptr::read_unaligned(position as *const Packed) };
    log_line(&format!("posicion vt={} valor={}", packed.vt, packed.value));
    if packed.vt == VT_I8.0 as u16 {
        packed.value
    } else {
        // VT_EMPTY: el pipeline usa el reloj de Media Foundation. Un cero absoluto
        // queda en el pasado y Frame Server descarta todas las muestras.
        unsafe { MFGetSystemTime() }
    }
}

fn mark_state(inner: &SourceInner, next: SourceState) {
    *inner.state.lock().unwrap_or_else(|poison| poison.into_inner()) = next;
}

fn nv12_type() -> windows::core::Result<IMFMediaType> {
    let media_type = unsafe { MFCreateMediaType() }?;
    unsafe {
        media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        media_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        media_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_size(WIDTH, HEIGHT))?;
        media_type.SetUINT64(&MF_MT_FRAME_RATE, pack_size(30, 1))?;
        media_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_size(1, 1))?;
        media_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        media_type.SetUINT32(&MF_MT_DEFAULT_STRIDE, STRIDE)?;
        media_type.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
        media_type.SetUINT32(&MF_MT_FIXED_SIZE_SAMPLES, 1)?;
        media_type.SetUINT32(&MF_MT_SAMPLE_SIZE, SLOT_BYTES as u32)?;
    }
    Ok(media_type)
}

fn copy_nv12(buffer: &IMFMediaBuffer, src: &[u8]) -> windows::core::Result<()> {
    let two_d: windows::Win32::Media::MediaFoundation::IMF2DBuffer = buffer.cast()?;
    let mut scan = std::ptr::null_mut();
    let mut pitch = 0i32;
    unsafe { two_d.Lock2D(&mut scan, &mut pitch)? };
    let result = (|| {
        if scan.is_null() || pitch < WIDTH as i32 || src.len() < SLOT_BYTES {
            return Err(fail(E_INVALIDARG));
        }
        unsafe {
            for row in 0..HEIGHT as usize {
                std::ptr::copy_nonoverlapping(
                    src.as_ptr().add(row * STRIDE as usize),
                    scan.add(row * pitch as usize),
                    WIDTH as usize,
                );
            }
            let uv = scan.add(pitch as usize * HEIGHT as usize);
            let src_uv = src.as_ptr().add(Y_BYTES);
            for row in 0..(HEIGHT as usize / 2) {
                std::ptr::copy_nonoverlapping(
                    src_uv.add(row * STRIDE as usize),
                    uv.add(row * pitch as usize),
                    WIDTH as usize,
                );
            }
        }
        Ok(())
    })();
    let unlocked = unsafe { two_d.Unlock2D() };
    result.and(unlocked)
}

fn black_frame() -> Vec<u8> {
    let mut frame = vec![16u8; SLOT_BYTES];
    frame[Y_BYTES..].fill(128);
    frame
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceState {
    Stopped,
    Started,
    Shutdown,
}

struct SourceInner {
    queue: IMFMediaEventQueue,
    attributes: IMFAttributes,
    descriptor: IMFPresentationDescriptor,
    stream: Mutex<Option<IMFMediaStream2>>,
    source: Mutex<Option<IMFMediaSource>>,
    state: Mutex<SourceState>,
    started_before: AtomicBool,
    origin: AtomicI64,
    d3d: Mutex<Option<IUnknown>>,
}

#[implement(IMFMediaSourceEx, IKsControl, IMFGetService)]
struct Source {
    inner: Arc<SourceInner>,
    _track: Track,
}

#[implement(IMFMediaStream2)]
struct Stream {
    source: Arc<SourceInner>,
    descriptor: IMFStreamDescriptor,
    queue: IMFMediaEventQueue,
    state: Mutex<MF_STREAM_STATE>,
    shutdown: AtomicBool,
    reader: Mutex<Option<Reader>>,
    reader_tried: AtomicBool,
    samples: AtomicU32,
    frame: Mutex<Vec<u8>>,
    _track: Track,
}

#[implement(IMFActivate)]
struct Activate {
    store: IMFAttributes,
    source: Mutex<Option<IMFMediaSourceEx>>,
    _track: Track,
}

#[implement(IClassFactory)]
struct ClassFactory {
    _track: Track,
}

pub fn class_factory() -> IClassFactory {
    ClassFactory { _track: Track::new() }.into()
}

impl Source {
    fn build() -> windows::core::Result<IMFMediaSourceEx> {
        let queue = unsafe { MFCreateEventQueue() }?;
        let attributes = create_attributes(4)?;
        unsafe {
            attributes.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)?;
            attributes.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_CATEGORY, &KSCATEGORY_VIDEO_CAMERA)?;
            attributes.SetString(&MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, windows::core::w!("cellCam"))?;
        }
        let media_type = nv12_type()?;
        let stream_descriptor = unsafe { MFCreateStreamDescriptor(0, &[Some(media_type)]) }?;
        unsafe {
            stream_descriptor.SetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY, &PINNAME_VIDEO_CAPTURE)?;
            stream_descriptor.SetUINT32(&MF_DEVICESTREAM_STREAM_ID, 0)?;
            stream_descriptor.SetUINT32(&MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES, MFFrameSourceTypes_Color.0 as u32)?;
            stream_descriptor.SetUINT32(&MF_DEVICESTREAM_FRAMESERVER_SHARED, 1)?;
        }
        let descriptor = unsafe { MFCreatePresentationDescriptor(Some(&[Some(stream_descriptor.clone())])) }?;
        unsafe { descriptor.SelectStream(0)? };
        let inner = Arc::new(SourceInner {
            queue,
            attributes,
            descriptor,
            stream: Mutex::new(None),
            source: Mutex::new(None),
            state: Mutex::new(SourceState::Stopped),
            started_before: AtomicBool::new(false),
            origin: AtomicI64::new(0),
            d3d: Mutex::new(None),
        });
        let source: IMFMediaSourceEx = Source { inner: inner.clone(), _track: Track::new() }.into();
        let as_source: IMFMediaSource = source.cast()?;
        *inner.source.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(as_source.clone());
        let stream_queue = unsafe { MFCreateEventQueue() }?;
        let stream: IMFMediaStream2 = Stream {
            source: inner.clone(),
            descriptor: stream_descriptor,
            queue: stream_queue,
            state: Mutex::new(MF_STREAM_STATE_STOPPED),
            shutdown: AtomicBool::new(false),
            reader: Mutex::new(None),
            reader_tried: AtomicBool::new(false),
            samples: AtomicU32::new(0),
            frame: Mutex::new(black_frame()),
            _track: Track::new(),
        }
        .into();
        *inner.stream.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(stream);
        log_line("fuente creada");
        Ok(source)
    }
}

impl IMFMediaSourceEx_Impl for Source_Impl {
    fn GetSourceAttributes(&self) -> windows::core::Result<IMFAttributes> {
        let state = *self.inner.state.lock().unwrap_or_else(|poison| poison.into_inner());
        if state == SourceState::Shutdown {
            return Err(fail(MF_E_SHUTDOWN));
        }
        Ok(self.inner.attributes.clone())
    }

    fn GetStreamAttributes(&self, dwstreamidentifier: u32) -> windows::core::Result<IMFAttributes> {
        let state = *self.inner.state.lock().unwrap_or_else(|poison| poison.into_inner());
        if state == SourceState::Shutdown {
            return Err(fail(MF_E_SHUTDOWN));
        }
        if dwstreamidentifier != 0 {
            return Err(fail(E_INVALIDARG));
        }
        let stream = self.inner.stream.lock().unwrap_or_else(|poison| poison.into_inner()).clone();
        let stream = stream.ok_or_else(|| fail(MF_E_SHUTDOWN))?;
        let descriptor = unsafe { stream.GetStreamDescriptor() }?;
        descriptor.cast()
    }

    fn SetD3DManager(&self, pmanager: windows::core::Ref<'_, IUnknown>) -> windows::core::Result<()> {
        let state = *self.inner.state.lock().unwrap_or_else(|poison| poison.into_inner());
        if state == SourceState::Shutdown {
            return Err(fail(MF_E_SHUTDOWN));
        }
        *self.inner.d3d.lock().unwrap_or_else(|poison| poison.into_inner()) = pmanager.cloned();
        Ok(())
    }
}

impl windows::Win32::Media::MediaFoundation::IMFMediaSource_Impl for Source_Impl {
    fn GetCharacteristics(&self) -> windows::core::Result<u32> {
        let state = *self.inner.state.lock().unwrap_or_else(|poison| poison.into_inner());
        if state == SourceState::Shutdown {
            return Err(fail(MF_E_SHUTDOWN));
        }
        Ok(MFMEDIASOURCE_IS_LIVE.0 as u32)
    }

    fn CreatePresentationDescriptor(&self) -> windows::core::Result<IMFPresentationDescriptor> {
        let state = *self.inner.state.lock().unwrap_or_else(|poison| poison.into_inner());
        if state == SourceState::Shutdown {
            return Err(fail(MF_E_SHUTDOWN));
        }
        unsafe { self.inner.descriptor.Clone() }
    }

    fn Start(
        &self,
        ppresentationdescriptor: windows::core::Ref<'_, IMFPresentationDescriptor>,
        pguidtimeformat: *const GUID,
        pvarstartposition: *const PROPVARIANT,
    ) -> windows::core::Result<()> {
        log_line(&format!(
            "start descriptor={} posicion={} formato={}",
            if ppresentationdescriptor.is_none() { 0 } else { 1 },
            if pvarstartposition.is_null() { 0 } else { 1 },
            if pguidtimeformat.is_null() { 0 } else { 1 }
        ));
        if ppresentationdescriptor.is_none() || pvarstartposition.is_null() {
            return Err(fail(E_INVALIDARG));
        }
        if !pguidtimeformat.is_null() && unsafe { *pguidtimeformat } != GUID::zeroed() {
            return Err(fail(MF_E_UNSUPPORTED_TIME_FORMAT));
        }
        let state = *self.inner.state.lock().unwrap_or_else(|poison| poison.into_inner());
        if state == SourceState::Shutdown {
            return Err(fail(MF_E_SHUTDOWN));
        }
        if state != SourceState::Stopped {
            return Err(fail(MF_E_INVALID_STATE_TRANSITION));
        }
        let descriptor = ppresentationdescriptor.ok()?;
        let mut selected = BOOL(0);
        let mut incoming = None;
        unsafe { descriptor.GetStreamDescriptorByIndex(0, &mut selected, &mut incoming)? };
        let stream = self.inner.stream.lock().unwrap_or_else(|poison| poison.into_inner()).clone();
        let Some(stream) = stream else {
            return Err(fail(MF_E_SHUTDOWN));
        };
        let origin = start_origin(pvarstartposition);
        self.inner.origin.store(origin, Ordering::Release);
        mark_state(&self.inner, SourceState::Started);
        let when = int64_variant(origin);
        let none = GUID::zeroed();
        let fail_start = |inner: &SourceInner, error: windows::core::Error| {
            mark_state(inner, SourceState::Stopped);
            log_line(&format!("arranque error {error}"));
            Err(error)
        };
        if let Err(error) = unsafe { self.inner.queue.QueueEventParamVar(MESourceStarted.0 as u32, &none, HRESULT(0), &when) } {
            return fail_start(&self.inner, error);
        }
        if let Err(error) = unsafe { self.inner.descriptor.SelectStream(0) } {
            return fail_start(&self.inner, error);
        }
        if let Err(error) = unsafe { stream.SetStreamState(MF_STREAM_STATE_RUNNING) } {
            return fail_start(&self.inner, error);
        }
        let kind = if self.inner.started_before.swap(true, Ordering::AcqRel) { MEUpdatedStream } else { MENewStream };
        let unknown: IUnknown = match stream.cast() {
            Ok(unknown) => unknown,
            Err(error) => return fail_start(&self.inner, error),
        };
        if let Err(error) = unsafe { self.inner.queue.QueueEventParamUnk(kind.0 as u32, &none, HRESULT(0), &unknown) } {
            return fail_start(&self.inner, error);
        }
        if let Err(error) = unsafe { stream.QueueEvent(MEStreamStarted.0 as u32, &none, HRESULT(0), &when) } {
            return fail_start(&self.inner, error);
        }
        log_line(&format!("arranque seleccionado={}", selected.as_bool()));
        Ok(())
    }

    fn Stop(&self) -> windows::core::Result<()> {
        let state = *self.inner.state.lock().unwrap_or_else(|poison| poison.into_inner());
        if state == SourceState::Shutdown {
            return Err(fail(MF_E_SHUTDOWN));
        }
        if state != SourceState::Started {
            return Err(fail(MF_E_INVALID_STATE_TRANSITION));
        }
        let stream = self.inner.stream.lock().unwrap_or_else(|poison| poison.into_inner()).clone();
        mark_state(&self.inner, SourceState::Stopped);
        let none = GUID::zeroed();
        if let Some(stream) = stream {
            unsafe { stream.SetStreamState(MF_STREAM_STATE_STOPPED)? };
            unsafe { stream.QueueEvent(MEStreamStopped.0 as u32, &none, HRESULT(0), std::ptr::null())? };
        }
        unsafe { self.inner.queue.QueueEventParamVar(MESourceStopped.0 as u32, &none, HRESULT(0), std::ptr::null())? };
        Ok(())
    }

    fn Pause(&self) -> windows::core::Result<()> {
        let state = *self.inner.state.lock().unwrap_or_else(|poison| poison.into_inner());
        Err(fail(if state == SourceState::Shutdown { MF_E_SHUTDOWN } else { MF_E_INVALID_STATE_TRANSITION }))
    }

    fn Shutdown(&self) -> windows::core::Result<()> {
        let mut state = self.inner.state.lock().unwrap_or_else(|poison| poison.into_inner());
        if *state == SourceState::Shutdown {
            return Err(fail(MF_E_SHUTDOWN));
        }
        *state = SourceState::Shutdown;
        self.inner.source.lock().unwrap_or_else(|poison| poison.into_inner()).take();
        self.inner.stream.lock().unwrap_or_else(|poison| poison.into_inner()).take();
        self.inner.d3d.lock().unwrap_or_else(|poison| poison.into_inner()).take();
        unsafe { self.inner.queue.Shutdown() }
    }
}

impl windows::Win32::Media::MediaFoundation::IMFMediaEventGenerator_Impl for Source_Impl {
    fn GetEvent(&self, dwflags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> windows::core::Result<IMFMediaEvent> {
        let state = *self.inner.state.lock().unwrap_or_else(|poison| poison.into_inner());
        if state == SourceState::Shutdown {
            return Err(fail(MF_E_SHUTDOWN));
        }
        let queue = self.inner.queue.clone();
        unsafe { queue.GetEvent(dwflags.0) }
    }

    fn BeginGetEvent(&self, pcallback: windows::core::Ref<'_, windows::Win32::Media::MediaFoundation::IMFAsyncCallback>, punkstate: windows::core::Ref<'_, IUnknown>) -> windows::core::Result<()> {
        let state = *self.inner.state.lock().unwrap_or_else(|poison| poison.into_inner());
        if state == SourceState::Shutdown {
            return Err(fail(MF_E_SHUTDOWN));
        }
        unsafe { self.inner.queue.BeginGetEvent(pcallback.as_ref(), punkstate.as_ref()) }
    }

    fn EndGetEvent(&self, presult: windows::core::Ref<'_, windows::Win32::Media::MediaFoundation::IMFAsyncResult>) -> windows::core::Result<IMFMediaEvent> {
        let state = *self.inner.state.lock().unwrap_or_else(|poison| poison.into_inner());
        if state == SourceState::Shutdown {
            return Err(fail(MF_E_SHUTDOWN));
        }
        unsafe { self.inner.queue.EndGetEvent(presult.as_ref()) }
    }

    fn QueueEvent(&self, met: u32, guidextendedtype: *const GUID, hrstatus: HRESULT, pvvalue: *const PROPVARIANT) -> windows::core::Result<()> {
        let state = *self.inner.state.lock().unwrap_or_else(|poison| poison.into_inner());
        if state == SourceState::Shutdown {
            return Err(fail(MF_E_SHUTDOWN));
        }
        unsafe { self.inner.queue.QueueEventParamVar(met, guidextendedtype, hrstatus, pvvalue) }
    }
}

impl IKsControl_Impl for Source_Impl {
    fn KsProperty(&self, _property: *const KSIDENTIFIER, _propertylength: u32, _propertydata: *mut core::ffi::c_void, _datalength: u32, bytesreturned: *mut u32) -> windows::core::Result<()> {
        if !bytesreturned.is_null() {
            unsafe { *bytesreturned = 0 };
        }
        Err(win32_error(ERROR_SET_NOT_FOUND.0))
    }

    fn KsMethod(&self, _method: *const KSIDENTIFIER, _methodlength: u32, _methoddata: *mut core::ffi::c_void, _datalength: u32, bytesreturned: *mut u32) -> windows::core::Result<()> {
        if !bytesreturned.is_null() {
            unsafe { *bytesreturned = 0 };
        }
        Err(win32_error(ERROR_SET_NOT_FOUND.0))
    }

    fn KsEvent(&self, _event: *const KSIDENTIFIER, _eventlength: u32, _eventdata: *mut core::ffi::c_void, _datalength: u32, bytesreturned: *mut u32) -> windows::core::Result<()> {
        if !bytesreturned.is_null() {
            unsafe { *bytesreturned = 0 };
        }
        Err(win32_error(ERROR_SET_NOT_FOUND.0))
    }
}

impl IMFGetService_Impl for Source_Impl {
    fn GetService(&self, _guidservice: *const GUID, _riid: *const GUID, ppvobject: *mut *mut core::ffi::c_void) -> windows::core::Result<()> {
        if ppvobject.is_null() {
            return Err(fail(E_POINTER));
        }
        unsafe { *ppvobject = std::ptr::null_mut() };
        let state = *self.inner.state.lock().unwrap_or_else(|poison| poison.into_inner());
        Err(fail(if state == SourceState::Shutdown { MF_E_SHUTDOWN } else { MF_E_UNSUPPORTED_SERVICE }))
    }
}

impl IMFMediaStream2_Impl for Stream_Impl {
    fn SetStreamState(&self, value: MF_STREAM_STATE) -> windows::core::Result<()> {
        if self.shutdown.load(Ordering::Acquire) {
            return Err(fail(MF_E_SHUTDOWN));
        }
        *self.state.lock().unwrap_or_else(|poison| poison.into_inner()) = value;
        Ok(())
    }

    fn GetStreamState(&self) -> windows::core::Result<MF_STREAM_STATE> {
        if self.shutdown.load(Ordering::Acquire) {
            return Err(fail(MF_E_SHUTDOWN));
        }
        Ok(*self.state.lock().unwrap_or_else(|poison| poison.into_inner()))
    }
}

fn stream_down(stream: &Stream) -> bool {
    stream.shutdown.load(Ordering::Acquire)
        || *stream.source.state.lock().unwrap_or_else(|poison| poison.into_inner()) == SourceState::Shutdown
}

impl windows::Win32::Media::MediaFoundation::IMFMediaStream_Impl for Stream_Impl {
    fn GetMediaSource(&self) -> windows::core::Result<IMFMediaSource> {
        if stream_down(self) {
            return Err(fail(MF_E_SHUTDOWN));
        }
        self.source.source.lock().unwrap_or_else(|poison| poison.into_inner()).clone().ok_or_else(|| fail(MF_E_SHUTDOWN))
    }

    fn GetStreamDescriptor(&self) -> windows::core::Result<IMFStreamDescriptor> {
        if stream_down(self) {
            return Err(fail(MF_E_SHUTDOWN));
        }
        Ok(self.descriptor.clone())
    }

    fn RequestSample(&self, ptoken: windows::core::Ref<'_, IUnknown>) -> windows::core::Result<()> {
        if stream_down(self) {
            return Err(fail(MF_E_SHUTDOWN));
        }
        if *self.state.lock().unwrap_or_else(|poison| poison.into_inner()) != MF_STREAM_STATE_RUNNING {
            return Err(fail(MF_E_MEDIA_SOURCE_WRONGSTATE));
        }
        let index_due = self.samples.load(Ordering::Acquire) as i64;
        let due = self.source.origin.load(Ordering::Acquire) + index_due * 10_000_000 / 30;
        let now = unsafe { MFGetSystemTime() };
        if due > now {
            let nanos = ((due - now) as u64).saturating_mul(100);
            std::thread::sleep(std::time::Duration::from_nanos(nanos.min(50_000_000)));
        }
        let mut reader = self.reader.lock().unwrap_or_else(|poison| poison.into_inner());
        if reader.is_none() {
            match Reader::open(&vcam_shm::default_path()) {
                Ok(opened) => {
                    log_line("shm=ok");
                    *reader = Some(opened);
                }
                Err(error) => {
                    if !self.reader_tried.swap(true, Ordering::AcqRel) {
                        log_line(&format!("shm=error {error}"));
                    }
                }
            }
        }
        let mut frame = self.frame.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(opened) = reader.as_mut() {
            let (y, uv) = frame.split_at_mut(Y_BYTES);
            let _ = opened.read_latest(y, uv);
        }
        drop(reader);
        let sample = unsafe { MFCreateSample() }?;
        let buffer = unsafe { MFCreate2DMediaBuffer(WIDTH, HEIGHT, NV12_FOURCC, false) }?;
        copy_nv12(&buffer, &frame)?;
        unsafe { sample.AddBuffer(&buffer)? };
        let index = self.samples.fetch_add(1, Ordering::AcqRel) as i64;
        let time = self.source.origin.load(Ordering::Acquire) + index * 10_000_000 / 30;
        let duration = (index + 1) * 10_000_000 / 30 - time;
        unsafe {
            sample.SetSampleTime(time)?;
            sample.SetSampleDuration(duration)?;
            sample.SetUINT32(&MFSampleExtension_CleanPoint, 1)?;
            if let Some(token) = ptoken.as_ref() {
                sample.SetUnknown(&MFSampleExtension_Token, token)?;
            }
            let unknown: IUnknown = sample.cast()?;
            let none = GUID::zeroed();
            self.queue.QueueEventParamUnk(windows::Win32::Media::MediaFoundation::MEMediaSample.0 as u32, &none, HRESULT(0), &unknown)?;
        }
        if index == 0 || index % 300 == 0 {
            log_line(&format!("muestra n={}", index + 1));
        }
        Ok(())
    }
}

impl windows::Win32::Media::MediaFoundation::IMFMediaEventGenerator_Impl for Stream_Impl {
    fn GetEvent(&self, dwflags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> windows::core::Result<IMFMediaEvent> {
        if stream_down(self) {
            return Err(fail(MF_E_SHUTDOWN));
        }
        unsafe { self.queue.GetEvent(dwflags.0) }
    }

    fn BeginGetEvent(&self, pcallback: windows::core::Ref<'_, windows::Win32::Media::MediaFoundation::IMFAsyncCallback>, punkstate: windows::core::Ref<'_, IUnknown>) -> windows::core::Result<()> {
        if stream_down(self) {
            return Err(fail(MF_E_SHUTDOWN));
        }
        unsafe { self.queue.BeginGetEvent(pcallback.as_ref(), punkstate.as_ref()) }
    }

    fn EndGetEvent(&self, presult: windows::core::Ref<'_, windows::Win32::Media::MediaFoundation::IMFAsyncResult>) -> windows::core::Result<IMFMediaEvent> {
        if stream_down(self) {
            return Err(fail(MF_E_SHUTDOWN));
        }
        unsafe { self.queue.EndGetEvent(presult.as_ref()) }
    }

    fn QueueEvent(&self, met: u32, guidextendedtype: *const GUID, hrstatus: HRESULT, pvvalue: *const PROPVARIANT) -> windows::core::Result<()> {
        if stream_down(self) {
            return Err(fail(MF_E_SHUTDOWN));
        }
        unsafe { self.queue.QueueEventParamVar(met, guidextendedtype, hrstatus, pvvalue) }
    }
}

macro_rules! forward_attrs {
    ($impl:ty, $field:ident) => {
        impl IMFAttributes_Impl for $impl {
            fn GetItem(&self, guidkey: *const GUID, pvalue: *mut PROPVARIANT) -> windows::core::Result<()> {
                let dest = if pvalue.is_null() { None } else { Some(pvalue) };
                unsafe { self.$field.GetItem(guid_ref(guidkey), dest) }
            }
            fn GetItemType(&self, guidkey: *const GUID) -> windows::core::Result<MF_ATTRIBUTE_TYPE> {
                unsafe { self.$field.GetItemType(guid_ref(guidkey)) }
            }
            fn CompareItem(&self, guidkey: *const GUID, value: *const PROPVARIANT) -> windows::core::Result<BOOL> {
                unsafe { self.$field.CompareItem(guid_ref(guidkey), &*value) }
            }
            fn Compare(&self, ptheirs: windows::core::Ref<'_, IMFAttributes>, matchtype: MF_ATTRIBUTES_MATCH_TYPE) -> windows::core::Result<BOOL> {
                unsafe { self.$field.Compare(ptheirs.as_ref(), matchtype) }
            }
            fn GetUINT32(&self, guidkey: *const GUID) -> windows::core::Result<u32> {
                unsafe { self.$field.GetUINT32(guid_ref(guidkey)) }
            }
            fn GetUINT64(&self, guidkey: *const GUID) -> windows::core::Result<u64> {
                unsafe { self.$field.GetUINT64(guid_ref(guidkey)) }
            }
            fn GetDouble(&self, guidkey: *const GUID) -> windows::core::Result<f64> {
                unsafe { self.$field.GetDouble(guid_ref(guidkey)) }
            }
            fn GetGUID(&self, guidkey: *const GUID) -> windows::core::Result<GUID> {
                unsafe { self.$field.GetGUID(guid_ref(guidkey)) }
            }
            fn GetStringLength(&self, guidkey: *const GUID) -> windows::core::Result<u32> {
                unsafe { self.$field.GetStringLength(guid_ref(guidkey)) }
            }
            fn GetString(&self, guidkey: *const GUID, pwszvalue: PWSTR, cchbufsize: u32, pcchlength: *mut u32) -> windows::core::Result<()> {
                let mut empty = [0u16; 0];
                let slice = if pwszvalue.is_null() || cchbufsize == 0 {
                    &mut empty[..]
                } else {
                    unsafe { std::slice::from_raw_parts_mut(pwszvalue.0, cchbufsize as usize) }
                };
                let length = if pcchlength.is_null() { None } else { Some(pcchlength) };
                unsafe { self.$field.GetString(guid_ref(guidkey), slice, length) }
            }
            fn GetAllocatedString(&self, guidkey: *const GUID, ppwszvalue: *mut PWSTR, pcchlength: *mut u32) -> windows::core::Result<()> {
                unsafe { self.$field.GetAllocatedString(guid_ref(guidkey), ppwszvalue, pcchlength) }
            }
            fn GetBlobSize(&self, guidkey: *const GUID) -> windows::core::Result<u32> {
                unsafe { self.$field.GetBlobSize(guid_ref(guidkey)) }
            }
            fn GetBlob(&self, guidkey: *const GUID, pbuf: *mut u8, cbbufsize: u32, pcbblobsize: *mut u32) -> windows::core::Result<()> {
                let mut empty = [0u8; 0];
                let slice = if pbuf.is_null() || cbbufsize == 0 {
                    &mut empty[..]
                } else {
                    unsafe { std::slice::from_raw_parts_mut(pbuf, cbbufsize as usize) }
                };
                let written = if pcbblobsize.is_null() { None } else { Some(pcbblobsize) };
                unsafe { self.$field.GetBlob(guid_ref(guidkey), slice, written) }
            }
            fn GetAllocatedBlob(&self, guidkey: *const GUID, ppbuf: *mut *mut u8, pcbsize: *mut u32) -> windows::core::Result<()> {
                unsafe { self.$field.GetAllocatedBlob(guid_ref(guidkey), ppbuf, pcbsize) }
            }
            fn GetUnknown(&self, guidkey: *const GUID, riid: *const GUID, ppv: *mut *mut core::ffi::c_void) -> windows::core::Result<()> {
                let unknown: IUnknown = unsafe { self.$field.GetUnknown(guid_ref(guidkey)) }?;
                unsafe { unknown.query(riid, ppv).ok() }
            }
            fn SetItem(&self, guidkey: *const GUID, value: *const PROPVARIANT) -> windows::core::Result<()> {
                unsafe { self.$field.SetItem(guid_ref(guidkey), &*value) }
            }
            fn DeleteItem(&self, guidkey: *const GUID) -> windows::core::Result<()> {
                unsafe { self.$field.DeleteItem(guid_ref(guidkey)) }
            }
            fn DeleteAllItems(&self) -> windows::core::Result<()> {
                unsafe { self.$field.DeleteAllItems() }
            }
            fn SetUINT32(&self, guidkey: *const GUID, unvalue: u32) -> windows::core::Result<()> {
                unsafe { self.$field.SetUINT32(guid_ref(guidkey), unvalue) }
            }
            fn SetUINT64(&self, guidkey: *const GUID, unvalue: u64) -> windows::core::Result<()> {
                unsafe { self.$field.SetUINT64(guid_ref(guidkey), unvalue) }
            }
            fn SetDouble(&self, guidkey: *const GUID, fvalue: f64) -> windows::core::Result<()> {
                unsafe { self.$field.SetDouble(guid_ref(guidkey), fvalue) }
            }
            fn SetGUID(&self, guidkey: *const GUID, guidvalue: *const GUID) -> windows::core::Result<()> {
                unsafe { self.$field.SetGUID(guid_ref(guidkey), guid_ref(guidvalue)) }
            }
            fn SetString(&self, guidkey: *const GUID, wszvalue: &PCWSTR) -> windows::core::Result<()> {
                unsafe { self.$field.SetString(guid_ref(guidkey), *wszvalue) }
            }
            fn SetBlob(&self, guidkey: *const GUID, pbuf: *const u8, cbbufsize: u32) -> windows::core::Result<()> {
                let bytes = if pbuf.is_null() || cbbufsize == 0 {
                    &[]
                } else {
                    unsafe { std::slice::from_raw_parts(pbuf, cbbufsize as usize) }
                };
                unsafe { self.$field.SetBlob(guid_ref(guidkey), bytes) }
            }
            fn SetUnknown(&self, guidkey: *const GUID, punknown: windows::core::Ref<'_, IUnknown>) -> windows::core::Result<()> {
                unsafe { self.$field.SetUnknown(guid_ref(guidkey), punknown.as_ref()) }
            }
            fn LockStore(&self) -> windows::core::Result<()> {
                unsafe { self.$field.LockStore() }
            }
            fn UnlockStore(&self) -> windows::core::Result<()> {
                unsafe { self.$field.UnlockStore() }
            }
            fn GetCount(&self) -> windows::core::Result<u32> {
                unsafe { self.$field.GetCount() }
            }
            fn GetItemByIndex(&self, unindex: u32, pguidkey: *mut GUID, pvalue: *mut PROPVARIANT) -> windows::core::Result<()> {
                let dest = if pvalue.is_null() { None } else { Some(pvalue) };
                unsafe { self.$field.GetItemByIndex(unindex, pguidkey, dest) }
            }
            fn CopyAllItems(&self, pdest: windows::core::Ref<'_, IMFAttributes>) -> windows::core::Result<()> {
                unsafe { self.$field.CopyAllItems(pdest.as_ref()) }
            }
        }
    };
}

forward_attrs!(Activate_Impl, store);

impl IMFActivate_Impl for Activate_Impl {
    fn ActivateObject(&self, riid: *const GUID, ppv: *mut *mut core::ffi::c_void) -> windows::core::Result<()> {
        if ppv.is_null() {
            return Err(fail(E_POINTER));
        }
        unsafe { *ppv = std::ptr::null_mut() };
        let mut slot = self.source.lock().unwrap_or_else(|poison| poison.into_inner());
        if slot.is_none() {
            let created = Source::build()?;
            if let Ok(attributes) = unsafe { created.GetSourceAttributes() } {
                let _ = unsafe { self.store.CopyAllItems(&attributes) };
            }
            *slot = Some(created);
            log_line("activada");
        }
        let source = slot.as_ref().unwrap();
        unsafe { source.query(riid, ppv).ok() }
    }

    fn ShutdownObject(&self) -> windows::core::Result<()> {
        if let Some(source) = self.source.lock().unwrap_or_else(|poison| poison.into_inner()).take() {
            unsafe { source.Shutdown()? };
        }
        Err(fail(E_NOTIMPL))
    }

    fn DetachObject(&self) -> windows::core::Result<()> {
        Err(fail(E_NOTIMPL))
    }
}

impl IClassFactory_Impl for ClassFactory_Impl {
    fn CreateInstance(&self, punkouter: windows::core::Ref<'_, IUnknown>, riid: *const GUID, ppvobject: *mut *mut core::ffi::c_void) -> windows::core::Result<()> {
        if ppvobject.is_null() {
            return Err(fail(E_POINTER));
        }
        unsafe { *ppvobject = std::ptr::null_mut() };
        if punkouter.is_some() {
            return Err(fail(CLASS_E_NOAGGREGATION));
        }
        let activate: IMFActivate = Activate {
            store: create_attributes(8)?,
            source: Mutex::new(None),
            _track: Track::new(),
        }
        .into();
        unsafe { activate.query(riid, ppvobject).ok() }
    }

    fn LockServer(&self, flock: BOOL) -> windows::core::Result<()> {
        if flock.as_bool() {
            LOCKS.fetch_add(1, Ordering::Release);
        } else {
            LOCKS.fetch_sub(1, Ordering::Release);
        }
        Ok(())
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.inner.stream.lock().unwrap_or_else(|poison| poison.into_inner()).take();
        self.inner.source.lock().unwrap_or_else(|poison| poison.into_inner()).take();
        let _ = unsafe { self.inner.queue.Shutdown() };
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = unsafe { self.queue.Shutdown() };
    }
}
