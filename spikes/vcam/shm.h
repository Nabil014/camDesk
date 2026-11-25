#pragma once

// Prueba descartable del paso 0. El layout copia el contrato de PLAN.md
// para comprobar la ruta y la ACL; no es el crate vcam-shm.

#include <cstdint>
#include <windows.h>

constexpr uint32_t kWidth = 1920;
constexpr uint32_t kHeight = 1080;
constexpr uint32_t kStride = 1920;
constexpr uint32_t kSlotBytes = kStride * kHeight * 3 / 2;
constexpr uint32_t kHeaderBytes = 64;
constexpr uint32_t kTotalBytes = kHeaderBytes + kSlotBytes * 2;
constexpr uint32_t kMagic = 0x43564331u;

// {8F4E2A11-6C3D-4B9E-9170-2A5B8C4D6E11}
static const GUID CLSID_CellCamSpike = {
    0x8f4e2a11, 0x6c3d, 0x4b9e, {0x91, 0x70, 0x2a, 0x5b, 0x8c, 0x4d, 0x6e, 0x11}};

struct ShmHeader {
    uint32_t magic;
    uint32_t version;
    uint32_t width;
    uint32_t height;
    uint32_t stride;
    uint32_t active;
    int64_t seq0;
    int64_t pts0;
    int64_t seq1;
    int64_t pts1;
    uint8_t reserved[8];
};

static_assert(sizeof(ShmHeader) == kHeaderBytes, "la cabecera del archivo mapeado mide 64 bytes");
static_assert(kSlotBytes == 3110400u, "cada slot NV12 mide 3110400 bytes");
static_assert(kTotalBytes == 6220864u, "el archivo mapeado mide 6220864 bytes");

struct ShmWriter {
    HANDLE file = INVALID_HANDLE_VALUE;
    HANDLE mapping = nullptr;
    ShmHeader* hdr = nullptr;
    HANDLE thread = nullptr;
    volatile LONG stop = 0;

    ShmWriter() = default;
    ShmWriter(const ShmWriter&) = delete;
    ShmWriter& operator=(const ShmWriter&) = delete;
    ~ShmWriter() { Close(); }

    bool Create(const wchar_t* path);
    void Start();
    void Close();
};

struct ShmReader {
    HANDLE file = INVALID_HANDLE_VALUE;
    HANDLE mapping = nullptr;
    ShmHeader* hdr = nullptr;
    uint8_t* last = nullptr;

    ShmReader() = default;
    ShmReader(const ShmReader&) = delete;
    ShmReader& operator=(const ShmReader&) = delete;
    ~ShmReader() { Close(); }

    bool Open(const wchar_t* path, DWORD* win32Error);
    bool CopyLatest(uint8_t* dst);
    void Close();
};

void PaintBar(uint8_t* slot, uint32_t frame);
void CamdeskDir(wchar_t* out, size_t cch);
void CamdeskShmPath(wchar_t* out, size_t cch);
void SpikeLogf(const wchar_t* fmt, ...);
void SpikeLogIdentity();
