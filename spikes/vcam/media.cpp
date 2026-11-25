#include "media.h"

#include <cstring>
#include <new>

namespace {

constexpr LONGLONG kFrameDuration = 333333;

#ifndef MAKEFOURCC
#define MAKEFOURCC(a, b, c, d) \
    ((DWORD)(BYTE)(a) | ((DWORD)(BYTE)(b) << 8) | ((DWORD)(BYTE)(c) << 16) | ((DWORD)(BYTE)(d) << 24))
#endif

HRESULT MakeNv12Type(IMFMediaType** type) {
    Microsoft::WRL::ComPtr<IMFMediaType> mediaType;
    HRESULT hr = MFCreateMediaType(&mediaType);
    if (FAILED(hr)) {
        return hr;
    }
    hr = mediaType->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video);
    if (SUCCEEDED(hr)) {
        hr = mediaType->SetGUID(MF_MT_SUBTYPE, MFVideoFormat_NV12);
    }
    if (SUCCEEDED(hr)) {
        hr = MFSetAttributeSize(mediaType.Get(), MF_MT_FRAME_SIZE, kWidth, kHeight);
    }
    if (SUCCEEDED(hr)) {
        hr = MFSetAttributeRatio(mediaType.Get(), MF_MT_FRAME_RATE, 30, 1);
    }
    if (SUCCEEDED(hr)) {
        hr = MFSetAttributeRatio(mediaType.Get(), MF_MT_PIXEL_ASPECT_RATIO, 1, 1);
    }
    if (SUCCEEDED(hr)) {
        hr = mediaType->SetUINT32(MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive);
    }
    if (SUCCEEDED(hr)) {
        hr = mediaType->SetUINT32(MF_MT_DEFAULT_STRIDE, kStride);
    }
    if (SUCCEEDED(hr)) {
        hr = mediaType->SetUINT32(MF_MT_ALL_SAMPLES_INDEPENDENT, TRUE);
    }
    if (SUCCEEDED(hr)) {
        hr = mediaType->SetUINT32(MF_MT_FIXED_SIZE_SAMPLES, TRUE);
    }
    if (SUCCEEDED(hr)) {
        hr = mediaType->SetUINT32(MF_MT_SAMPLE_SIZE, kSlotBytes);
    }
    if (FAILED(hr)) {
        return hr;
    }
    *type = mediaType.Detach();
    return S_OK;
}

HRESULT CopyNv12(IMFMediaBuffer* buffer, const uint8_t* src) {
    Microsoft::WRL::ComPtr<IMF2DBuffer> twoD;
    HRESULT hr = buffer->QueryInterface(IID_PPV_ARGS(&twoD));
    if (FAILED(hr)) {
        return hr;
    }
    BYTE* scan = nullptr;
    LONG pitch = 0;
    hr = twoD->Lock2D(&scan, &pitch);
    if (FAILED(hr)) {
        return hr;
    }
    if (pitch < static_cast<LONG>(kWidth)) {
        twoD->Unlock2D();
        return E_FAIL;
    }
    for (uint32_t row = 0; row < kHeight; ++row) {
        std::memcpy(scan + static_cast<size_t>(row) * pitch, src + static_cast<size_t>(row) * kStride, kWidth);
    }
    BYTE* uv = scan + static_cast<size_t>(pitch) * kHeight;
    const uint8_t* srcUv = src + static_cast<size_t>(kStride) * kHeight;
    for (uint32_t row = 0; row < kHeight / 2; ++row) {
        std::memcpy(uv + static_cast<size_t>(row) * pitch, srcUv + static_cast<size_t>(row) * kStride, kWidth);
    }
    return twoD->Unlock2D();
}

int BarColumn(const uint8_t* frame) {
    const uint8_t* row = frame + static_cast<size_t>(540) * kStride;
    for (uint32_t x = 0; x < kWidth; ++x) {
        if (row[x] == 235) {
            return static_cast<int>(x);
        }
    }
    return -1;
}

}  // namespace

MediaStream::MediaStream() {
    InitializeCriticalSection(&lock_);
    SpikeTrack(true);
}

MediaStream::~MediaStream() {
    Shutdown();
    DeleteCriticalSection(&lock_);
    delete[] frame_;
    SpikeTrack(false);
}

HRESULT MediaStream::Create(MediaSource* source, IMFStreamDescriptor* descriptor, MediaStream** out) {
    auto* stream = new (std::nothrow) MediaStream();
    if (stream == nullptr) {
        return E_OUTOFMEMORY;
    }
    const HRESULT hr = stream->Init(source, descriptor);
    if (FAILED(hr)) {
        stream->Release();
        return hr;
    }
    *out = stream;
    return S_OK;
}

HRESULT MediaStream::Init(MediaSource* source, IMFStreamDescriptor* descriptor) {
    source_ = source;
    descriptor_ = descriptor;
    frame_ = new (std::nothrow) uint8_t[kSlotBytes];
    if (frame_ == nullptr) {
        return E_OUTOFMEMORY;
    }
    std::memset(frame_, 16, static_cast<size_t>(kStride) * kHeight);
    std::memset(frame_ + static_cast<size_t>(kStride) * kHeight, 128, static_cast<size_t>(kStride) * kHeight / 2);
    return MFCreateEventQueue(&queue_);
}

STDMETHODIMP MediaStream::QueryInterface(REFIID riid, void** object) {
    if (object == nullptr) {
        return E_POINTER;
    }
    if (riid == IID_IUnknown || riid == IID_IMFMediaEventGenerator || riid == IID_IMFMediaStream || riid == IID_IMFMediaStream2) {
        *object = static_cast<IMFMediaStream2*>(this);
        AddRef();
        return S_OK;
    }
    *object = nullptr;
    return E_NOINTERFACE;
}

STDMETHODIMP_(ULONG) MediaStream::AddRef() {
    return InterlockedIncrement(&ref_);
}

STDMETHODIMP_(ULONG) MediaStream::Release() {
    const ULONG left = InterlockedDecrement(&ref_);
    if (left == 0) {
        delete this;
    }
    return left;
}

STDMETHODIMP MediaStream::BeginGetEvent(IMFAsyncCallback* callback, IUnknown* state) {
    EnterCriticalSection(&lock_);
    const HRESULT hr = shutdown_ || !queue_ ? MF_E_SHUTDOWN : queue_->BeginGetEvent(callback, state);
    LeaveCriticalSection(&lock_);
    return hr;
}

STDMETHODIMP MediaStream::EndGetEvent(IMFAsyncResult* result, IMFMediaEvent** event) {
    EnterCriticalSection(&lock_);
    const HRESULT hr = shutdown_ || !queue_ ? MF_E_SHUTDOWN : queue_->EndGetEvent(result, event);
    LeaveCriticalSection(&lock_);
    return hr;
}

STDMETHODIMP MediaStream::GetEvent(DWORD flags, IMFMediaEvent** event) {
    Microsoft::WRL::ComPtr<IMFMediaEventQueue> queue;
    EnterCriticalSection(&lock_);
    if (shutdown_ || !queue_) {
        LeaveCriticalSection(&lock_);
        return MF_E_SHUTDOWN;
    }
    queue = queue_;
    LeaveCriticalSection(&lock_);
    return queue->GetEvent(flags, event);
}

STDMETHODIMP MediaStream::QueueEvent(MediaEventType type, REFGUID extendedType, HRESULT status, const PROPVARIANT* value) {
    EnterCriticalSection(&lock_);
    const HRESULT hr = shutdown_ || !queue_ ? MF_E_SHUTDOWN : queue_->QueueEventParamVar(type, extendedType, status, value);
    LeaveCriticalSection(&lock_);
    return hr;
}

STDMETHODIMP MediaStream::GetMediaSource(IMFMediaSource** mediaSource) {
    if (mediaSource == nullptr) {
        return E_POINTER;
    }
    EnterCriticalSection(&lock_);
    if (shutdown_ || source_ == nullptr) {
        LeaveCriticalSection(&lock_);
        return MF_E_SHUTDOWN;
    }
    const HRESULT hr = source_->QueryInterface(IID_PPV_ARGS(mediaSource));
    LeaveCriticalSection(&lock_);
    return hr;
}

STDMETHODIMP MediaStream::GetStreamDescriptor(IMFStreamDescriptor** descriptor) {
    if (descriptor == nullptr) {
        return E_POINTER;
    }
    EnterCriticalSection(&lock_);
    if (shutdown_ || !descriptor_) {
        LeaveCriticalSection(&lock_);
        return MF_E_SHUTDOWN;
    }
    *descriptor = descriptor_.Get();
    (*descriptor)->AddRef();
    LeaveCriticalSection(&lock_);
    return S_OK;
}

STDMETHODIMP MediaStream::RequestSample(IUnknown* token) {
    EnterCriticalSection(&lock_);
    if (shutdown_ || !queue_) {
        LeaveCriticalSection(&lock_);
        return MF_E_SHUTDOWN;
    }
    if (state_ != MF_STREAM_STATE_RUNNING) {
        LeaveCriticalSection(&lock_);
        return MF_E_MEDIA_SOURCE_WRONGSTATE;
    }

    if (!shmTried_) {
        shmTried_ = true;
        wchar_t path[MAX_PATH];
        CamdeskShmPath(path, MAX_PATH);
        DWORD error = 0;
        shmOk_ = reader_.Open(path, &error);
        SpikeLogf(L"shm=%s win32=%lu ruta=%s", shmOk_ ? L"ok" : L"error", error, path);
    }
    const bool got = shmOk_ && reader_.CopyLatest(frame_);
    if (!got) {
        PaintBar(frame_, samples_);
    }

    Microsoft::WRL::ComPtr<IMFSample> sample;
    Microsoft::WRL::ComPtr<IMFMediaBuffer> buffer;
    HRESULT hr = MFCreateSample(&sample);
    if (SUCCEEDED(hr)) {
        hr = MFCreate2DMediaBuffer(kWidth, kHeight, MAKEFOURCC('N', 'V', '1', '2'), FALSE, &buffer);
    }
    if (SUCCEEDED(hr)) {
        hr = CopyNv12(buffer.Get(), frame_);
    }
    if (SUCCEEDED(hr)) {
        hr = sample->AddBuffer(buffer.Get());
    }
    if (SUCCEEDED(hr)) {
        hr = sample->SetSampleTime(MFGetSystemTime());
    }
    if (SUCCEEDED(hr)) {
        hr = sample->SetSampleDuration(kFrameDuration);
    }
    if (SUCCEEDED(hr)) {
        hr = sample->SetUINT32(MFSampleExtension_CleanPoint, TRUE);
    }
    if (SUCCEEDED(hr) && token != nullptr) {
        hr = sample->SetUnknown(MFSampleExtension_Token, token);
    }
    if (SUCCEEDED(hr)) {
        hr = queue_->QueueEventParamUnk(MEMediaSample, GUID_NULL, S_OK, sample.Get());
    }
    if (SUCCEEDED(hr)) {
        ++samples_;
        if (samples_ == 1 || samples_ % 30 == 0) {
            SpikeLogf(L"muestra n=%u barra=%d shm=%d", samples_, BarColumn(frame_), shmOk_ ? 1 : 0);
        }
    } else {
        SpikeLogf(L"muestra error=0x%08X", hr);
    }
    LeaveCriticalSection(&lock_);
    return hr;
}

STDMETHODIMP MediaStream::SetStreamState(MF_STREAM_STATE value) {
    EnterCriticalSection(&lock_);
    if (shutdown_) {
        LeaveCriticalSection(&lock_);
        return MF_E_SHUTDOWN;
    }
    state_ = value;
    LeaveCriticalSection(&lock_);
    SpikeLogf(L"stream estado=%d", static_cast<int>(value));
    return S_OK;
}

STDMETHODIMP MediaStream::GetStreamState(MF_STREAM_STATE* value) {
    if (value == nullptr) {
        return E_POINTER;
    }
    EnterCriticalSection(&lock_);
    if (shutdown_) {
        LeaveCriticalSection(&lock_);
        return MF_E_SHUTDOWN;
    }
    *value = state_;
    LeaveCriticalSection(&lock_);
    return S_OK;
}

HRESULT MediaStream::Shutdown() {
    EnterCriticalSection(&lock_);
    if (shutdown_) {
        LeaveCriticalSection(&lock_);
        return MF_E_SHUTDOWN;
    }
    shutdown_ = true;
    source_ = nullptr;
    if (queue_) {
        queue_->Shutdown();
        queue_.Reset();
    }
    LeaveCriticalSection(&lock_);
    return S_OK;
}

MediaSource::MediaSource() {
    InitializeCriticalSection(&lock_);
    SpikeTrack(true);
}

MediaSource::~MediaSource() {
    Shutdown();
    DeleteCriticalSection(&lock_);
    SpikeTrack(false);
}

HRESULT MediaSource::Create(MediaSource** out) {
    auto* source = new (std::nothrow) MediaSource();
    if (source == nullptr) {
        return E_OUTOFMEMORY;
    }
    const HRESULT hr = source->Init();
    if (FAILED(hr)) {
        source->Release();
        return hr;
    }
    *out = source;
    return S_OK;
}

HRESULT MediaSource::Init() {
    HRESULT hr = MFCreateEventQueue(&queue_);
    if (FAILED(hr)) {
        return hr;
    }
    hr = MFCreateAttributes(&attributes_, 4);
    if (FAILED(hr)) {
        return hr;
    }
    hr = attributes_->SetGUID(MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID);
    if (SUCCEEDED(hr)) {
        hr = attributes_->SetGUID(MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_CATEGORY, KSCATEGORY_VIDEO_CAMERA);
    }
    if (SUCCEEDED(hr)) {
        hr = attributes_->SetString(MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, L"cellCam");
    }
    if (FAILED(hr)) {
        return hr;
    }

    Microsoft::WRL::ComPtr<IMFMediaType> mediaType;
    hr = MakeNv12Type(&mediaType);
    if (FAILED(hr)) {
        return hr;
    }
    IMFMediaType* types[] = {mediaType.Get()};
    Microsoft::WRL::ComPtr<IMFStreamDescriptor> streamDescriptor;
    hr = MFCreateStreamDescriptor(0, 1, types, &streamDescriptor);
    if (FAILED(hr)) {
        return hr;
    }
    hr = streamDescriptor->SetGUID(MF_DEVICESTREAM_STREAM_CATEGORY, PINNAME_VIDEO_CAPTURE);
    if (SUCCEEDED(hr)) {
        hr = streamDescriptor->SetUINT32(MF_DEVICESTREAM_STREAM_ID, 0);
    }
    if (SUCCEEDED(hr)) {
        hr = streamDescriptor->SetUINT32(MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES, MFFrameSourceTypes_Color);
    }
    if (SUCCEEDED(hr)) {
        hr = streamDescriptor->SetUINT32(MF_DEVICESTREAM_FRAMESERVER_SHARED, 1);
    }
    if (FAILED(hr)) {
        return hr;
    }

    IMFStreamDescriptor* streams[] = {streamDescriptor.Get()};
    hr = MFCreatePresentationDescriptor(1, streams, &descriptor_);
    if (FAILED(hr)) {
        return hr;
    }
    hr = descriptor_->SelectStream(0);
    if (FAILED(hr)) {
        return hr;
    }

    MediaStream* stream = nullptr;
    hr = MediaStream::Create(this, streamDescriptor.Get(), &stream);
    if (FAILED(hr)) {
        return hr;
    }
    stream_.Attach(stream);
    return S_OK;
}

STDMETHODIMP MediaSource::QueryInterface(REFIID riid, void** object) {
    if (object == nullptr) {
        return E_POINTER;
    }
    if (riid == IID_IUnknown || riid == IID_IMFMediaEventGenerator || riid == IID_IMFMediaSource || riid == IID_IMFMediaSourceEx) {
        *object = static_cast<IMFMediaSourceEx*>(this);
        AddRef();
        return S_OK;
    }
    if (riid == __uuidof(IKsControl)) {
        *object = static_cast<IKsControl*>(this);
        AddRef();
        return S_OK;
    }
    if (riid == IID_IMFGetService) {
        *object = static_cast<IMFGetService*>(this);
        AddRef();
        return S_OK;
    }
    *object = nullptr;
    return E_NOINTERFACE;
}

STDMETHODIMP_(ULONG) MediaSource::AddRef() {
    return InterlockedIncrement(&ref_);
}

STDMETHODIMP_(ULONG) MediaSource::Release() {
    const ULONG left = InterlockedDecrement(&ref_);
    if (left == 0) {
        delete this;
    }
    return left;
}

STDMETHODIMP MediaSource::BeginGetEvent(IMFAsyncCallback* callback, IUnknown* state) {
    EnterCriticalSection(&lock_);
    const HRESULT hr = state_ == State::Shutdown || !queue_ ? MF_E_SHUTDOWN : queue_->BeginGetEvent(callback, state);
    LeaveCriticalSection(&lock_);
    return hr;
}

STDMETHODIMP MediaSource::EndGetEvent(IMFAsyncResult* result, IMFMediaEvent** event) {
    EnterCriticalSection(&lock_);
    const HRESULT hr = state_ == State::Shutdown || !queue_ ? MF_E_SHUTDOWN : queue_->EndGetEvent(result, event);
    LeaveCriticalSection(&lock_);
    return hr;
}

STDMETHODIMP MediaSource::GetEvent(DWORD flags, IMFMediaEvent** event) {
    Microsoft::WRL::ComPtr<IMFMediaEventQueue> queue;
    EnterCriticalSection(&lock_);
    if (state_ == State::Shutdown || !queue_) {
        LeaveCriticalSection(&lock_);
        return MF_E_SHUTDOWN;
    }
    queue = queue_;
    LeaveCriticalSection(&lock_);
    return queue->GetEvent(flags, event);
}

STDMETHODIMP MediaSource::QueueEvent(MediaEventType type, REFGUID extendedType, HRESULT status, const PROPVARIANT* value) {
    EnterCriticalSection(&lock_);
    const HRESULT hr = state_ == State::Shutdown || !queue_ ? MF_E_SHUTDOWN : queue_->QueueEventParamVar(type, extendedType, status, value);
    LeaveCriticalSection(&lock_);
    return hr;
}

STDMETHODIMP MediaSource::GetCharacteristics(DWORD* characteristics) {
    if (characteristics == nullptr) {
        return E_POINTER;
    }
    EnterCriticalSection(&lock_);
    if (state_ == State::Shutdown) {
        LeaveCriticalSection(&lock_);
        return MF_E_SHUTDOWN;
    }
    *characteristics = MFMEDIASOURCE_IS_LIVE;
    LeaveCriticalSection(&lock_);
    return S_OK;
}

STDMETHODIMP MediaSource::CreatePresentationDescriptor(IMFPresentationDescriptor** descriptor) {
    if (descriptor == nullptr) {
        return E_POINTER;
    }
    EnterCriticalSection(&lock_);
    if (state_ == State::Shutdown || !descriptor_) {
        LeaveCriticalSection(&lock_);
        return MF_E_SHUTDOWN;
    }
    const HRESULT hr = descriptor_->Clone(descriptor);
    LeaveCriticalSection(&lock_);
    return hr;
}

STDMETHODIMP MediaSource::Start(IMFPresentationDescriptor* descriptor, const GUID* timeFormat, const PROPVARIANT* start) {
    if (descriptor == nullptr || start == nullptr) {
        return E_INVALIDARG;
    }
    if (timeFormat != nullptr && *timeFormat != GUID_NULL) {
        return MF_E_UNSUPPORTED_TIME_FORMAT;
    }
    EnterCriticalSection(&lock_);
    if (state_ == State::Shutdown || !queue_ || !stream_) {
        LeaveCriticalSection(&lock_);
        return MF_E_SHUTDOWN;
    }
    if (state_ != State::Stopped) {
        LeaveCriticalSection(&lock_);
        return MF_E_INVALID_STATE_TRANSITION;
    }

    BOOL selected = FALSE;
    Microsoft::WRL::ComPtr<IMFStreamDescriptor> incoming;
    HRESULT hr = descriptor->GetStreamDescriptorByIndex(0, &selected, &incoming);
    if (FAILED(hr)) {
        LeaveCriticalSection(&lock_);
        return hr;
    }

    state_ = State::Started;
    PROPVARIANT startTime;
    PropVariantInit(&startTime);
    startTime.vt = VT_I8;
    startTime.hVal.QuadPart = MFGetSystemTime();
    hr = queue_->QueueEventParamVar(MESourceStarted, GUID_NULL, S_OK, &startTime);
    if (SUCCEEDED(hr)) {
        hr = descriptor_->SelectStream(0);
    }
    if (SUCCEEDED(hr)) {
        hr = stream_->SetStreamState(MF_STREAM_STATE_RUNNING);
    }
    if (SUCCEEDED(hr)) {
        const MediaEventType kind = streamWasSelected_ ? MEUpdatedStream : MENewStream;
        hr = queue_->QueueEventParamUnk(kind, GUID_NULL, S_OK, stream_.Get());
    }
    if (SUCCEEDED(hr)) {
        hr = stream_->QueueEvent(MEStreamStarted, GUID_NULL, S_OK, &startTime);
    }
    if (SUCCEEDED(hr)) {
        streamWasSelected_ = true;
    } else {
        state_ = State::Stopped;
    }
    PropVariantClear(&startTime);
    LeaveCriticalSection(&lock_);
    SpikeLogf(L"arranque hr=0x%08X seleccionado=%d", hr, selected ? 1 : 0);
    return hr;
}

STDMETHODIMP MediaSource::Stop() {
    EnterCriticalSection(&lock_);
    if (state_ == State::Shutdown || !queue_ || !stream_) {
        LeaveCriticalSection(&lock_);
        return MF_E_SHUTDOWN;
    }
    if (state_ != State::Started) {
        LeaveCriticalSection(&lock_);
        return MF_E_INVALID_STATE_TRANSITION;
    }
    state_ = State::Stopped;
    HRESULT hr = stream_->SetStreamState(MF_STREAM_STATE_STOPPED);
    if (SUCCEEDED(hr)) {
        hr = queue_->QueueEventParamVar(MESourceStopped, GUID_NULL, S_OK, nullptr);
    }
    if (SUCCEEDED(hr)) {
        hr = stream_->QueueEvent(MEStreamStopped, GUID_NULL, S_OK, nullptr);
    }
    LeaveCriticalSection(&lock_);
    return hr;
}

STDMETHODIMP MediaSource::Pause() {
    EnterCriticalSection(&lock_);
    const bool shutdown = state_ == State::Shutdown;
    LeaveCriticalSection(&lock_);
    return shutdown ? MF_E_SHUTDOWN : MF_E_INVALID_STATE_TRANSITION;
}

STDMETHODIMP MediaSource::Shutdown() {
    EnterCriticalSection(&lock_);
    if (state_ == State::Shutdown) {
        LeaveCriticalSection(&lock_);
        return MF_E_SHUTDOWN;
    }
    state_ = State::Shutdown;
    if (stream_) {
        stream_->Shutdown();
        stream_.Reset();
    }
    if (queue_) {
        queue_->Shutdown();
        queue_.Reset();
    }
    d3d_.Reset();
    LeaveCriticalSection(&lock_);
    return S_OK;
}

STDMETHODIMP MediaSource::GetSourceAttributes(IMFAttributes** attributes) {
    if (attributes == nullptr) {
        return E_POINTER;
    }
    EnterCriticalSection(&lock_);
    if (state_ == State::Shutdown || !attributes_) {
        LeaveCriticalSection(&lock_);
        return MF_E_SHUTDOWN;
    }
    *attributes = attributes_.Get();
    (*attributes)->AddRef();
    LeaveCriticalSection(&lock_);
    return S_OK;
}

STDMETHODIMP MediaSource::GetStreamAttributes(DWORD streamId, IMFAttributes** attributes) {
    if (attributes == nullptr) {
        return E_POINTER;
    }
    EnterCriticalSection(&lock_);
    if (state_ == State::Shutdown || !stream_) {
        LeaveCriticalSection(&lock_);
        return MF_E_SHUTDOWN;
    }
    if (streamId != 0) {
        LeaveCriticalSection(&lock_);
        return E_INVALIDARG;
    }
    Microsoft::WRL::ComPtr<IMFStreamDescriptor> descriptor;
    HRESULT hr = stream_->GetStreamDescriptor(&descriptor);
    if (SUCCEEDED(hr)) {
        hr = descriptor.CopyTo(attributes);
    }
    LeaveCriticalSection(&lock_);
    return hr;
}

STDMETHODIMP MediaSource::SetD3DManager(IUnknown* manager) {
    EnterCriticalSection(&lock_);
    if (state_ == State::Shutdown) {
        LeaveCriticalSection(&lock_);
        return MF_E_SHUTDOWN;
    }
    d3d_ = manager;
    LeaveCriticalSection(&lock_);
    return S_OK;
}

STDMETHODIMP MediaSource::KsProperty(PKSPROPERTY, ULONG, LPVOID, ULONG, ULONG* bytesReturned) {
    if (bytesReturned != nullptr) {
        *bytesReturned = 0;
    }
    return HRESULT_FROM_WIN32(ERROR_SET_NOT_FOUND);
}

STDMETHODIMP MediaSource::KsMethod(PKSMETHOD, ULONG, LPVOID, ULONG, ULONG* bytesReturned) {
    if (bytesReturned != nullptr) {
        *bytesReturned = 0;
    }
    return HRESULT_FROM_WIN32(ERROR_SET_NOT_FOUND);
}

STDMETHODIMP MediaSource::KsEvent(PKSEVENT, ULONG, LPVOID, ULONG, ULONG* bytesReturned) {
    if (bytesReturned != nullptr) {
        *bytesReturned = 0;
    }
    return HRESULT_FROM_WIN32(ERROR_SET_NOT_FOUND);
}

STDMETHODIMP MediaSource::GetService(REFGUID, REFIID, LPVOID* object) {
    if (object == nullptr) {
        return E_POINTER;
    }
    *object = nullptr;
    EnterCriticalSection(&lock_);
    const bool shutdown = state_ == State::Shutdown;
    LeaveCriticalSection(&lock_);
    return shutdown ? MF_E_SHUTDOWN : MF_E_UNSUPPORTED_SERVICE;
}

class SourceActivate final : public IMFActivate {
public:
    SourceActivate() {
        SpikeTrack(true);
    }

    ~SourceActivate() {
        SpikeTrack(false);
    }

    HRESULT Init() {
        return MFCreateAttributes(&store_, 8);
    }

    STDMETHODIMP QueryInterface(REFIID riid, void** object) override {
        if (object == nullptr) {
            return E_POINTER;
        }
        if (riid == IID_IUnknown || riid == IID_IMFAttributes || riid == IID_IMFActivate) {
            *object = static_cast<IMFActivate*>(this);
            AddRef();
            return S_OK;
        }
        *object = nullptr;
        wchar_t guid[64];
        StringFromGUID2(riid, guid, 64);
        SpikeLogf(L"activador sin interfaz %s", guid);
        return E_NOINTERFACE;
    }

    STDMETHODIMP_(ULONG) AddRef() override { return InterlockedIncrement(&ref_); }

    STDMETHODIMP_(ULONG) Release() override {
        const ULONG left = InterlockedDecrement(&ref_);
        if (left == 0) {
            delete this;
        }
        return left;
    }

    STDMETHODIMP GetItem(REFGUID key, PROPVARIANT* value) override { return store_->GetItem(key, value); }
    STDMETHODIMP GetItemType(REFGUID key, MF_ATTRIBUTE_TYPE* type) override { return store_->GetItemType(key, type); }
    STDMETHODIMP CompareItem(REFGUID key, REFPROPVARIANT value, BOOL* result) override { return store_->CompareItem(key, value, result); }
    STDMETHODIMP Compare(IMFAttributes* theirs, MF_ATTRIBUTES_MATCH_TYPE match, BOOL* result) override { return store_->Compare(theirs, match, result); }
    STDMETHODIMP GetUINT32(REFGUID key, UINT32* value) override { return store_->GetUINT32(key, value); }
    STDMETHODIMP GetUINT64(REFGUID key, UINT64* value) override { return store_->GetUINT64(key, value); }
    STDMETHODIMP GetDouble(REFGUID key, double* value) override { return store_->GetDouble(key, value); }
    STDMETHODIMP GetGUID(REFGUID key, GUID* value) override { return store_->GetGUID(key, value); }
    STDMETHODIMP GetStringLength(REFGUID key, UINT32* length) override { return store_->GetStringLength(key, length); }
    STDMETHODIMP GetString(REFGUID key, LPWSTR value, UINT32 size, UINT32* length) override { return store_->GetString(key, value, size, length); }
    STDMETHODIMP GetAllocatedString(REFGUID key, LPWSTR* value, UINT32* length) override { return store_->GetAllocatedString(key, value, length); }
    STDMETHODIMP GetBlobSize(REFGUID key, UINT32* size) override { return store_->GetBlobSize(key, size); }
    STDMETHODIMP GetBlob(REFGUID key, UINT8* buffer, UINT32 size, UINT32* written) override { return store_->GetBlob(key, buffer, size, written); }
    STDMETHODIMP GetAllocatedBlob(REFGUID key, UINT8** buffer, UINT32* size) override { return store_->GetAllocatedBlob(key, buffer, size); }
    STDMETHODIMP GetUnknown(REFGUID key, REFIID riid, LPVOID* object) override { return store_->GetUnknown(key, riid, object); }
    STDMETHODIMP SetItem(REFGUID key, REFPROPVARIANT value) override { return store_->SetItem(key, value); }
    STDMETHODIMP DeleteItem(REFGUID key) override { return store_->DeleteItem(key); }
    STDMETHODIMP DeleteAllItems() override { return store_->DeleteAllItems(); }
    STDMETHODIMP SetUINT32(REFGUID key, UINT32 value) override { return store_->SetUINT32(key, value); }
    STDMETHODIMP SetUINT64(REFGUID key, UINT64 value) override { return store_->SetUINT64(key, value); }
    STDMETHODIMP SetDouble(REFGUID key, double value) override { return store_->SetDouble(key, value); }
    STDMETHODIMP SetGUID(REFGUID key, REFGUID value) override { return store_->SetGUID(key, value); }
    STDMETHODIMP SetString(REFGUID key, LPCWSTR value) override { return store_->SetString(key, value); }
    STDMETHODIMP SetBlob(REFGUID key, const UINT8* buffer, UINT32 size) override { return store_->SetBlob(key, buffer, size); }
    STDMETHODIMP SetUnknown(REFGUID key, IUnknown* unknown) override { return store_->SetUnknown(key, unknown); }
    STDMETHODIMP LockStore() override { return store_->LockStore(); }
    STDMETHODIMP UnlockStore() override { return store_->UnlockStore(); }
    STDMETHODIMP GetCount(UINT32* count) override { return store_->GetCount(count); }
    STDMETHODIMP GetItemByIndex(UINT32 index, GUID* key, PROPVARIANT* value) override { return store_->GetItemByIndex(index, key, value); }
    STDMETHODIMP CopyAllItems(IMFAttributes* dest) override { return store_->CopyAllItems(dest); }

    STDMETHODIMP ActivateObject(REFIID riid, void** object) override {
        if (object == nullptr) {
            return E_POINTER;
        }
        *object = nullptr;
        wchar_t guid[64];
        StringFromGUID2(riid, guid, 64);
        SpikeLogf(L"ActivateObject iid=%s", guid);
        if (!source_) {
            MediaSource* created = nullptr;
            const HRESULT createdHr = MediaSource::Create(&created);
            SpikeLogf(L"MediaSource::Create hr=0x%08X", createdHr);
            if (FAILED(createdHr)) {
                return createdHr;
            }
            source_.Attach(created);
            Microsoft::WRL::ComPtr<IMFAttributes> attributes;
            if (SUCCEEDED(source_->GetSourceAttributes(&attributes))) {
                store_->CopyAllItems(attributes.Get());
            }
        }
        const HRESULT qi = source_->QueryInterface(riid, object);
        SpikeLogf(L"MediaSource::QueryInterface hr=0x%08X", qi);
        return qi;
    }

    STDMETHODIMP ShutdownObject() override {
        if (source_) {
            source_->Shutdown();
            source_.Reset();
        }
        return E_NOTIMPL;
    }

    STDMETHODIMP DetachObject() override { return E_NOTIMPL; }

private:
    LONG ref_ = 1;
    Microsoft::WRL::ComPtr<IMFAttributes> store_;
    Microsoft::WRL::ComPtr<MediaSource> source_;
};

HRESULT CreateSourceActivate(REFIID riid, void** object) {
    if (object == nullptr) {
        return E_POINTER;
    }
    *object = nullptr;
    auto* activate = new (std::nothrow) SourceActivate();
    if (activate == nullptr) {
        return E_OUTOFMEMORY;
    }
    const HRESULT hr = activate->Init();
    if (FAILED(hr)) {
        activate->Release();
        return hr;
    }
    const HRESULT qi = activate->QueryInterface(riid, object);
    activate->Release();
    return qi;
}
