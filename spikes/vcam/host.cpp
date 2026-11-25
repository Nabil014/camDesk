// Prueba descartable del paso 0. Registra la DLL, crea la cámara virtual
// "cellCam" y comprueba si Frame Server llega a leer el archivo mapeado.

#include "shm.h"

#include <aclapi.h>
#include <mfapi.h>
#include <mfidl.h>
#include <mfreadwrite.h>
#include <mfvirtualcamera.h>
#include <sddl.h>
#include <shlobj.h>

#include <cstdio>
#include <cstring>
#include <string>

#include <wrl/client.h>

namespace {

constexpr wchar_t kLocalService[] = L"S-1-5-19";

void PrintHr(const wchar_t* label, HRESULT hr) {
    wprintf(L"%s=0x%08X\n", label, static_cast<unsigned>(hr));
}

bool IsElevated() {
    HANDLE token = nullptr;
    if (!OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &token)) {
        return false;
    }
    TOKEN_ELEVATION elevation{};
    DWORD bytes = 0;
    const BOOL ok = GetTokenInformation(token, TokenElevation, &elevation, sizeof(elevation), &bytes);
    CloseHandle(token);
    return ok && elevation.TokenIsElevated != 0;
}

DWORD GrantAccess(const wchar_t* path, DWORD access, DWORD inheritance) {
    PSID sid = nullptr;
    if (!ConvertStringSidToSidW(kLocalService, &sid)) {
        return GetLastError();
    }
    EXPLICIT_ACCESSW entry{};
    entry.grfAccessPermissions = access;
    entry.grfAccessMode = GRANT_ACCESS;
    entry.grfInheritance = inheritance;
    entry.Trustee.TrusteeForm = TRUSTEE_IS_SID;
    entry.Trustee.TrusteeType = TRUSTEE_IS_WELL_KNOWN_GROUP;
    entry.Trustee.ptstrName = static_cast<LPWSTR>(sid);

    PACL oldDacl = nullptr;
    PSECURITY_DESCRIPTOR sd = nullptr;
    DWORD rc = GetNamedSecurityInfoW(path, SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, nullptr, nullptr, &oldDacl, nullptr, &sd);
    if (rc == ERROR_SUCCESS) {
        PACL newDacl = nullptr;
        rc = SetEntriesInAclW(1, &entry, oldDacl, &newDacl);
        if (rc == ERROR_SUCCESS) {
            rc = SetNamedSecurityInfoW(const_cast<wchar_t*>(path), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, nullptr, nullptr, newDacl, nullptr);
            LocalFree(newDacl);
        }
    }
    LocalFree(sd);
    LocalFree(sid);
    return rc;
}

std::wstring ClsidKey() {
    wchar_t guid[64];
    StringFromGUID2(CLSID_CellCamSpike, guid, 64);
    return std::wstring(L"Software\\Classes\\CLSID\\") + guid;
}

HRESULT RegisterServer(HKEY root, const wchar_t* dllPath) {
    const std::wstring clsid = ClsidKey();
    HKEY key = nullptr;
    LONG rc = RegCreateKeyExW(root, clsid.c_str(), 0, nullptr, 0, KEY_SET_VALUE, nullptr, &key, nullptr);
    if (rc != ERROR_SUCCESS) {
        return HRESULT_FROM_WIN32(rc);
    }
    const wchar_t* name = L"cellCam spike";
    RegSetValueExW(key, nullptr, 0, REG_SZ, reinterpret_cast<const BYTE*>(name), static_cast<DWORD>((wcslen(name) + 1) * sizeof(wchar_t)));
    RegCloseKey(key);

    const std::wstring inproc = clsid + L"\\InprocServer32";
    rc = RegCreateKeyExW(root, inproc.c_str(), 0, nullptr, 0, KEY_SET_VALUE, nullptr, &key, nullptr);
    if (rc != ERROR_SUCCESS) {
        return HRESULT_FROM_WIN32(rc);
    }
    rc = RegSetValueExW(key, nullptr, 0, REG_SZ, reinterpret_cast<const BYTE*>(dllPath), static_cast<DWORD>((wcslen(dllPath) + 1) * sizeof(wchar_t)));
    if (rc == ERROR_SUCCESS) {
        const wchar_t* threading = L"Both";
        rc = RegSetValueExW(key, L"ThreadingModel", 0, REG_SZ, reinterpret_cast<const BYTE*>(threading), static_cast<DWORD>((wcslen(threading) + 1) * sizeof(wchar_t)));
    }
    RegCloseKey(key);
    return HRESULT_FROM_WIN32(rc);
}

void UnregisterServer(HKEY root) {
    RegDeleteTreeW(root, ClsidKey().c_str());
}

int BarColumn(const uint8_t* y, LONG pitch) {
    const uint8_t* row = y + static_cast<size_t>(540) * pitch;
    for (uint32_t x = 0; x < kWidth; ++x) {
        if (row[x] == 235) {
            return static_cast<int>(x);
        }
    }
    return -1;
}

bool SampleBar(IMFSample* sample, int* column) {
    Microsoft::WRL::ComPtr<IMFMediaBuffer> buffer;
    if (FAILED(sample->GetBufferByIndex(0, &buffer))) {
        return false;
    }
    Microsoft::WRL::ComPtr<IMF2DBuffer> twoD;
    if (FAILED(buffer.As(&twoD))) {
        return false;
    }
    BYTE* scan = nullptr;
    LONG pitch = 0;
    if (FAILED(twoD->Lock2D(&scan, &pitch))) {
        return false;
    }
    *column = BarColumn(scan, pitch);
    twoD->Unlock2D();
    return true;
}

struct ProbeResult {
    HRESULT open = E_FAIL;
    int samples = 0;
    int firstBar = -1;
    int lastBar = -1;
    bool timedOut = false;
};

ProbeResult Probe(const wchar_t* symbolicLink) {
    ProbeResult result;
    Microsoft::WRL::ComPtr<IMFAttributes> attributes;
    result.open = MFCreateAttributes(&attributes, 2);
    if (FAILED(result.open)) {
        return result;
    }
    result.open = attributes->SetGUID(MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID);
    if (FAILED(result.open)) {
        return result;
    }
    result.open = attributes->SetString(MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, symbolicLink);
    if (FAILED(result.open)) {
        return result;
    }

    Microsoft::WRL::ComPtr<IMFMediaSource> source;
    result.open = MFCreateDeviceSource(attributes.Get(), &source);
    if (FAILED(result.open)) {
        return result;
    }
    Microsoft::WRL::ComPtr<IMFSourceReader> reader;
    result.open = MFCreateSourceReaderFromMediaSource(source.Get(), nullptr, &reader);
    if (FAILED(result.open)) {
        return result;
    }

    HANDLE done = CreateEventW(nullptr, TRUE, FALSE, nullptr);
    struct ReadState {
        IMFSourceReader* reader = nullptr;
        ProbeResult* result = nullptr;
        HANDLE done = nullptr;
    } state{reader.Get(), &result, done};

    HANDLE worker = CreateThread(nullptr, 0, [](LPVOID param) -> DWORD {
        auto* state = static_cast<ReadState*>(param);
        for (int i = 0; i < 8; ++i) {
            if (i == 2) {
                Sleep(400);
            }
            DWORD stream = 0;
            DWORD flags = 0;
            LONGLONG time = 0;
            Microsoft::WRL::ComPtr<IMFSample> sample;
            const HRESULT hr = state->reader->ReadSample(MF_SOURCE_READER_FIRST_VIDEO_STREAM, 0, &stream, &flags, &time, &sample);
            if (FAILED(hr)) {
                state->result->open = hr;
                break;
            }
            if (sample == nullptr) {
                continue;
            }
            int column = -1;
            if (!SampleBar(sample.Get(), &column)) {
                continue;
            }
            if (state->result->samples == 0) {
                state->result->firstBar = column;
            }
            state->result->lastBar = column;
            state->result->samples += 1;
        }
        SetEvent(state->done);
        return 0;
    }, &state, 0, nullptr);

    if (WaitForSingleObject(done, 15000) == WAIT_TIMEOUT) {
        result.timedOut = true;
    }
    if (worker != nullptr) {
        CloseHandle(worker);
    }
    CloseHandle(done);
    if (source != nullptr) {
        source->Shutdown();
    }
    return result;
}

void DumpLog() {
    wchar_t dir[MAX_PATH];
    CamdeskDir(dir, MAX_PATH);
    wchar_t path[MAX_PATH];
    swprintf_s(path, L"%s\\spike.log", dir);
    FILE* file = nullptr;
    if (_wfopen_s(&file, path, L"rb") != 0 || file == nullptr) {
        wprintf(L"log=ausente\n");
        return;
    }
    char bytes[4096];
    const size_t count = fread(bytes, 1, sizeof(bytes) - 1, file);
    fclose(file);
    bytes[count] = 0;
    wchar_t wide[4096];
    MultiByteToWideChar(CP_UTF8, 0, bytes, -1, wide, 4096);
    wprintf(L"--- log ---\n%s--- fin log ---\n", wide);
}

const wchar_t* ArgValue(int argc, wchar_t** argv, const wchar_t* name) {
    for (int i = 1; i < argc - 1; ++i) {
        if (_wcsicmp(argv[i], name) == 0) {
            return argv[i + 1];
        }
    }
    return nullptr;
}

bool HasFlag(int argc, wchar_t** argv, const wchar_t* name) {
    for (int i = 1; i < argc; ++i) {
        if (_wcsicmp(argv[i], name) == 0) {
            return true;
        }
    }
    return false;
}

}  // namespace

int wmain(int argc, wchar_t** argv) {
    const wchar_t* dll = ArgValue(argc, argv, L"--dll");
    const wchar_t* hiveName = ArgValue(argc, argv, L"--hive");
    const wchar_t* holdText = ArgValue(argc, argv, L"--hold-segundos");
    const bool aclDll = HasFlag(argc, argv, L"--acl-dll");
    if (dll == nullptr || hiveName == nullptr) {
        fwprintf(stderr, L"uso: vcam_spike --dll RUTA --hive hkcu|hklm [--acl-dll] [--hold-segundos N]\n");
        return 4;
    }
    const bool hklm = _wcsicmp(hiveName, L"hklm") == 0;
    if (!hklm && _wcsicmp(hiveName, L"hkcu") != 0) {
        fwprintf(stderr, L"hive debe ser hkcu o hklm\n");
        return 4;
    }
    const int holdSeconds = holdText != nullptr ? _wtoi(holdText) : 0;

    wprintf(L"elevado=%d pid=%lu\n", IsElevated() ? 1 : 0, GetCurrentProcessId());
    wprintf(L"dll=%s\n", dll);
    wprintf(L"hive=%s\n", hklm ? L"HKLM" : L"HKCU");

    wchar_t dir[MAX_PATH];
    CamdeskDir(dir, MAX_PATH);
    CreateDirectoryW(dir, nullptr);
    wchar_t shmPath[MAX_PATH];
    CamdeskShmPath(shmPath, MAX_PATH);
    wchar_t logPath[MAX_PATH];
    swprintf_s(logPath, L"%s\\spike.log", dir);

    ShmWriter writer;
    if (!writer.Create(shmPath)) {
        wprintf(L"archivo_mapeado=0x%08X\n", static_cast<unsigned>(HRESULT_FROM_WIN32(GetLastError())));
        return 4;
    }
    const DWORD dirAcl = GrantAccess(dir, FILE_GENERIC_READ | FILE_GENERIC_EXECUTE, CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE);
    const DWORD shmAcl = GrantAccess(shmPath, FILE_GENERIC_READ, 0);
    HANDLE logFile = CreateFileW(logPath, GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, nullptr, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (logFile != INVALID_HANDLE_VALUE) {
        CloseHandle(logFile);
    }
    const DWORD logAcl = GrantAccess(logPath, FILE_GENERIC_READ | FILE_GENERIC_WRITE, 0);
    wprintf(L"acl_directorio=%lu acl_archivo=%lu acl_log=%lu\n", dirAcl, shmAcl, logAcl);
    if (aclDll) {
        const DWORD dllAcl = GrantAccess(dll, FILE_GENERIC_READ | FILE_GENERIC_EXECUTE, 0);
        wprintf(L"acl_dll=%lu\n", dllAcl);
    }
    writer.Start();

    const HRESULT registered = RegisterServer(hklm ? HKEY_LOCAL_MACHINE : HKEY_CURRENT_USER, dll);
    PrintHr(L"registro", registered);
    if (FAILED(registered)) {
        writer.Close();
        return 3;
    }

    const HRESULT com = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    const HRESULT mf = SUCCEEDED(com) || com == RPC_E_CHANGED_MODE ? MFStartup(MF_VERSION) : com;
    PrintHr(L"mf", mf);

    Microsoft::WRL::ComPtr<IMFVirtualCamera> camera;
    HRESULT created = E_FAIL;
    HRESULT started = E_FAIL;
    if (SUCCEEDED(mf)) {
        wchar_t guid[64];
        StringFromGUID2(CLSID_CellCamSpike, guid, 64);
        created = MFCreateVirtualCamera(
            MFVirtualCameraType_SoftwareCameraSource,
            MFVirtualCameraLifetime_Session,
            MFVirtualCameraAccess_CurrentUser,
            L"cellCam",
            guid,
            nullptr,
            0,
            &camera);
        PrintHr(L"crear", created);
        if (SUCCEEDED(created)) {
            started = camera->Start(nullptr);
            if (FAILED(started)) {
                camera->Stop();
                started = camera->Start(nullptr);
            }
            PrintHr(L"iniciar", started);
        }
    }

    std::wstring foundName;
    std::wstring foundLink;
    if (SUCCEEDED(started)) {
        Sleep(1500);
        Microsoft::WRL::ComPtr<IMFAttributes> attributes;
        if (SUCCEEDED(MFCreateAttributes(&attributes, 1))) {
            attributes->SetGUID(MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID);
            IMFActivate** devices = nullptr;
            UINT32 count = 0;
            const HRESULT listed = MFEnumDeviceSources(attributes.Get(), &devices, &count);
            PrintHr(L"enumerar", listed);
            wprintf(L"camaras=%u\n", count);
            if (SUCCEEDED(listed)) {
                for (UINT32 i = 0; i < count; ++i) {
                    wchar_t* name = nullptr;
                    UINT32 nameCch = 0;
                    if (SUCCEEDED(devices[i]->GetAllocatedString(MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, &name, &nameCch))) {
                        wprintf(L"camara[%u]=%s\n", i, name);
                        if (foundName.empty() && wcsstr(name, L"cellCam") != nullptr) {
                            foundName = name;
                            wchar_t* link = nullptr;
                            UINT32 linkCch = 0;
                            if (SUCCEEDED(devices[i]->GetAllocatedString(MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, &link, &linkCch))) {
                                foundLink = link;
                                CoTaskMemFree(link);
                            }
                        }
                        CoTaskMemFree(name);
                    }
                    devices[i]->Release();
                }
                CoTaskMemFree(devices);
            }
        }
    }

    ProbeResult probe;
    if (!foundLink.empty()) {
        wprintf(L"nombre=%s\n", foundName.c_str());
        wprintf(L"enlace=%s\n", foundLink.c_str());
        probe = Probe(foundLink.c_str());
        PrintHr(L"abrir", probe.open);
        wprintf(L"muestras=%d barra_inicial=%d barra_final=%d timeout=%d\n", probe.samples, probe.firstBar, probe.lastBar, probe.timedOut ? 1 : 0);
    } else {
        wprintf(L"nombre=\n");
    }

    if (holdSeconds > 0 && SUCCEEDED(started) && !probe.timedOut) {
        wprintf(L"espera=%d\n", holdSeconds);
        fflush(stdout);
        Sleep(static_cast<DWORD>(holdSeconds) * 1000);
    }

    if (camera != nullptr && !probe.timedOut) {
        camera->Stop();
        const HRESULT removed = camera->Remove();
        PrintHr(L"quitar", removed);
        camera->Shutdown();
        camera.Reset();
    }
    UnregisterServer(hklm ? HKEY_LOCAL_MACHINE : HKEY_CURRENT_USER);
    writer.Close();
    if (SUCCEEDED(mf)) {
        MFShutdown();
    }
    if (SUCCEEDED(com)) {
        CoUninitialize();
    }
    DumpLog();
    fflush(stdout);

    if (probe.timedOut) {
        ExitProcess(2);
    }
    if (FAILED(registered) || FAILED(created) || FAILED(started) || foundLink.empty() || FAILED(probe.open)) {
        return 2;
    }
    if (probe.samples < 2 || probe.firstBar == probe.lastBar) {
        return 1;
    }
    return 0;
}
