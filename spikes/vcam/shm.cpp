#include "shm.h"

#include <cstdarg>
#include <cstdio>
#include <cstring>

#include <shlobj.h>
#include <sddl.h>

void PaintBar(uint8_t* slot, uint32_t frame) {
    const int bar = 80;
    const int x0 = static_cast<int>((frame * 24u) % kWidth);
    uint8_t* y = slot;
    std::memset(y, 16, static_cast<size_t>(kStride) * kHeight);
    for (uint32_t row = 0; row < kHeight; ++row) {
        for (int col = 0; col < bar; ++col) {
            const int x = (x0 + col) % static_cast<int>(kWidth);
            y[static_cast<size_t>(row) * kStride + x] = 235;
        }
    }
    std::memset(slot + static_cast<size_t>(kStride) * kHeight, 128, static_cast<size_t>(kStride) * kHeight / 2);
}

void CamdeskDir(wchar_t* out, size_t cch) {
    PWSTR programData = nullptr;
    if (SUCCEEDED(SHGetKnownFolderPath(FOLDERID_ProgramData, 0, nullptr, &programData))) {
        swprintf_s(out, cch, L"%s\\Camdesk", programData);
        CoTaskMemFree(programData);
        return;
    }
    wcscpy_s(out, cch, L"C:\\ProgramData\\Camdesk");
}

void CamdeskShmPath(wchar_t* out, size_t cch) {
    wchar_t dir[MAX_PATH];
    CamdeskDir(dir, MAX_PATH);
    swprintf_s(out, cch, L"%s\\cellcam-vcam.bin", dir);
}

namespace {

CRITICAL_SECTION* LogLock() {
    static CRITICAL_SECTION cs;
    static INIT_ONCE once = INIT_ONCE_STATIC_INIT;
    InitOnceExecuteOnce(&once, [](PINIT_ONCE, PVOID param, PVOID*) -> BOOL {
        InitializeCriticalSection(static_cast<CRITICAL_SECTION*>(param));
        return TRUE;
    }, &cs, nullptr);
    return &cs;
}

}  // namespace

void SpikeLogf(const wchar_t* fmt, ...) {
    wchar_t buf[1200];
    va_list ap;
    va_start(ap, fmt);
    _vsnwprintf_s(buf, _TRUNCATE, fmt, ap);
    va_end(ap);

    OutputDebugStringW(buf);
    OutputDebugStringW(L"\n");

    wchar_t dir[MAX_PATH];
    CamdeskDir(dir, MAX_PATH);
    wchar_t path[MAX_PATH];
    swprintf_s(path, L"%s\\spike.log", dir);

    CRITICAL_SECTION* cs = LogLock();
    EnterCriticalSection(cs);
    HANDLE file = CreateFileW(
        path,
        FILE_APPEND_DATA,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        nullptr,
        OPEN_EXISTING,
        FILE_ATTRIBUTE_NORMAL,
        nullptr);
    if (file != INVALID_HANDLE_VALUE) {
        char utf8[1800];
        const int chars = WideCharToMultiByte(CP_UTF8, 0, buf, -1, utf8, static_cast<int>(sizeof(utf8)) - 2, nullptr, nullptr);
        if (chars > 1) {
            utf8[chars - 1] = '\n';
            DWORD written = 0;
            WriteFile(file, utf8, static_cast<DWORD>(chars), &written, nullptr);
        }
        CloseHandle(file);
    }
    LeaveCriticalSection(cs);
}

void SpikeLogIdentity() {
    HANDLE token = nullptr;
    if (!OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &token)) {
        SpikeLogf(L"identidad error=%lu", GetLastError());
        return;
    }
    DWORD bytes = 0;
    GetTokenInformation(token, TokenUser, nullptr, 0, &bytes);
    auto* buffer = static_cast<TOKEN_USER*>(HeapAlloc(GetProcessHeap(), 0, bytes));
    if (buffer == nullptr || !GetTokenInformation(token, TokenUser, buffer, bytes, &bytes)) {
        SpikeLogf(L"identidad error=%lu", GetLastError());
        if (buffer != nullptr) {
            HeapFree(GetProcessHeap(), 0, buffer);
        }
        CloseHandle(token);
        return;
    }
    wchar_t name[256] = L"?";
    wchar_t domain[256] = L"?";
    DWORD nameCch = 256;
    DWORD domainCch = 256;
    SID_NAME_USE use = SidTypeUnknown;
    LookupAccountSidW(nullptr, buffer->User.Sid, name, &nameCch, domain, &domainCch, &use);
    LPWSTR sid = nullptr;
    ConvertSidToStringSidW(buffer->User.Sid, &sid);
    SpikeLogf(L"identidad usuario=%s\\%s sid=%s pid=%lu", domain, name, sid != nullptr ? sid : L"?", GetCurrentProcessId());
    if (sid != nullptr) {
        LocalFree(sid);
    }
    HeapFree(GetProcessHeap(), 0, buffer);
    CloseHandle(token);
}

bool ShmWriter::Create(const wchar_t* path) {
    file = CreateFileW(
        path,
        GENERIC_READ | GENERIC_WRITE,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        nullptr,
        CREATE_ALWAYS,
        FILE_ATTRIBUTE_NORMAL,
        nullptr);
    if (file == INVALID_HANDLE_VALUE) {
        return false;
    }
    LARGE_INTEGER size{};
    size.QuadPart = kTotalBytes;
    if (!SetFilePointerEx(file, size, nullptr, FILE_BEGIN) || !SetEndOfFile(file)) {
        Close();
        return false;
    }
    mapping = CreateFileMappingW(file, nullptr, PAGE_READWRITE, 0, 0, nullptr);
    if (mapping == nullptr) {
        Close();
        return false;
    }
    hdr = static_cast<ShmHeader*>(MapViewOfFile(mapping, FILE_MAP_ALL_ACCESS, 0, 0, kTotalBytes));
    if (hdr == nullptr) {
        Close();
        return false;
    }
    std::memset(hdr, 0, kTotalBytes);
    hdr->magic = kMagic;
    hdr->version = 1;
    hdr->width = kWidth;
    hdr->height = kHeight;
    hdr->stride = kStride;
    auto* slots = reinterpret_cast<uint8_t*>(hdr) + kHeaderBytes;
    for (int slot = 0; slot < 2; ++slot) {
        uint8_t* base = slots + static_cast<size_t>(slot) * kSlotBytes;
        std::memset(base, 16, static_cast<size_t>(kStride) * kHeight);
        std::memset(base + static_cast<size_t>(kStride) * kHeight, 128, static_cast<size_t>(kStride) * kHeight / 2);
    }
    hdr->seq0 = 2;
    hdr->seq1 = 2;
    hdr->active = 0;
    return true;
}

void ShmWriter::Start() {
    thread = CreateThread(nullptr, 0, [](LPVOID param) -> DWORD {
        auto* self = static_cast<ShmWriter*>(param);
        uint32_t frame = 0;
        while (InterlockedCompareExchange(&self->stop, 0, 0) == 0) {
            const uint32_t slot = 1u - static_cast<uint32_t>(InterlockedCompareExchange(
                reinterpret_cast<LONG*>(&self->hdr->active), 0, 0));
            LONG64* seq = reinterpret_cast<LONG64*>(slot == 0 ? &self->hdr->seq0 : &self->hdr->seq1);
            InterlockedIncrement64(seq);
            MemoryBarrier();
            auto* pixels = reinterpret_cast<uint8_t*>(self->hdr) + kHeaderBytes + static_cast<size_t>(slot) * kSlotBytes;
            PaintBar(pixels, frame);
            if (slot == 0) {
                self->hdr->pts0 = frame;
            } else {
                self->hdr->pts1 = frame;
            }
            MemoryBarrier();
            InterlockedIncrement64(seq);
            InterlockedExchange(reinterpret_cast<LONG*>(&self->hdr->active), static_cast<LONG>(slot));
            ++frame;
            Sleep(33);
        }
        return 0;
    }, this, 0, nullptr);
}

void ShmWriter::Close() {
    InterlockedExchange(&stop, 1);
    if (thread != nullptr) {
        WaitForSingleObject(thread, 5000);
        CloseHandle(thread);
        thread = nullptr;
    }
    if (hdr != nullptr) {
        UnmapViewOfFile(hdr);
        hdr = nullptr;
    }
    if (mapping != nullptr) {
        CloseHandle(mapping);
        mapping = nullptr;
    }
    if (file != INVALID_HANDLE_VALUE) {
        CloseHandle(file);
        file = INVALID_HANDLE_VALUE;
    }
}

bool ShmReader::Open(const wchar_t* path, DWORD* win32Error) {
    if (win32Error != nullptr) {
        *win32Error = ERROR_SUCCESS;
    }
    file = CreateFileW(
        path,
        GENERIC_READ,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        nullptr,
        OPEN_EXISTING,
        FILE_ATTRIBUTE_NORMAL,
        nullptr);
    if (file == INVALID_HANDLE_VALUE) {
        if (win32Error != nullptr) {
            *win32Error = GetLastError();
        }
        return false;
    }
    LARGE_INTEGER size{};
    if (!GetFileSizeEx(file, &size) || size.QuadPart < kTotalBytes) {
        if (win32Error != nullptr) {
            *win32Error = ERROR_INVALID_DATA;
        }
        Close();
        return false;
    }
    mapping = CreateFileMappingW(file, nullptr, PAGE_READONLY, 0, 0, nullptr);
    if (mapping == nullptr) {
        if (win32Error != nullptr) {
            *win32Error = GetLastError();
        }
        Close();
        return false;
    }
    hdr = static_cast<ShmHeader*>(MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, kTotalBytes));
    if (hdr == nullptr) {
        if (win32Error != nullptr) {
            *win32Error = GetLastError();
        }
        Close();
        return false;
    }
    if (hdr->magic != kMagic || hdr->version != 1 || hdr->width != kWidth || hdr->height != kHeight || hdr->stride != kStride) {
        if (win32Error != nullptr) {
            *win32Error = ERROR_INVALID_DATA;
        }
        Close();
        return false;
    }
    return true;
}

bool ShmReader::CopyLatest(uint8_t* dst) {
    if (hdr == nullptr) {
        return false;
    }
    // El mapeo es de solo lectura. InterlockedCompareExchange escribe y
    // provoca una violación de acceso dentro de Frame Server.
    for (int attempt = 0; attempt < 3; ++attempt) {
        const uint32_t slot = *reinterpret_cast<volatile uint32_t*>(&hdr->active);
        MemoryBarrier();
        if (slot > 1) {
            continue;
        }
        volatile LONG64* seq = reinterpret_cast<volatile LONG64*>(slot == 0 ? &hdr->seq0 : &hdr->seq1);
        const LONG64 a = *seq;
        MemoryBarrier();
        if (a & 1) {
            continue;
        }
        const auto* src = reinterpret_cast<const uint8_t*>(hdr) + kHeaderBytes + static_cast<size_t>(slot) * kSlotBytes;
        std::memcpy(dst, src, kSlotBytes);
        MemoryBarrier();
        const LONG64 b = *seq;
        if (a == b) {
            if (last == nullptr) {
                last = new uint8_t[kSlotBytes];
            }
            std::memcpy(last, dst, kSlotBytes);
            return true;
        }
    }
    if (last != nullptr) {
        std::memcpy(dst, last, kSlotBytes);
        return true;
    }
    return false;
}

void ShmReader::Close() {
    delete[] last;
    last = nullptr;
    if (hdr != nullptr) {
        UnmapViewOfFile(hdr);
        hdr = nullptr;
    }
    if (mapping != nullptr) {
        CloseHandle(mapping);
        mapping = nullptr;
    }
    if (file != INVALID_HANDLE_VALUE) {
        CloseHandle(file);
        file = INVALID_HANDLE_VALUE;
    }
}
