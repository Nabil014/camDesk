#include "media.h"

#include <new>
#include <unknwn.h>

volatile LONG g_objects = 0;
volatile LONG g_locks = 0;

void SpikeTrack(bool add) {
    if (add) {
        InterlockedIncrement(&g_objects);
    } else {
        InterlockedDecrement(&g_objects);
    }
}

namespace {

class ClassFactory final : public IClassFactory {
public:
    STDMETHODIMP QueryInterface(REFIID riid, void** object) override {
        if (object == nullptr) {
            return E_POINTER;
        }
        if (riid == IID_IUnknown || riid == IID_IClassFactory) {
            *object = static_cast<IClassFactory*>(this);
            AddRef();
            return S_OK;
        }
        *object = nullptr;
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

    STDMETHODIMP CreateInstance(IUnknown* outer, REFIID riid, void** object) override {
        if (object == nullptr) {
            return E_POINTER;
        }
        *object = nullptr;
        if (outer != nullptr) {
            return CLASS_E_NOAGGREGATION;
        }
        wchar_t guid[64];
        StringFromGUID2(riid, guid, 64);
        SpikeLogf(L"CreateInstance iid=%s", guid);
        return CreateSourceActivate(riid, object);
    }

    STDMETHODIMP LockServer(BOOL lock) override {
        if (lock) {
            InterlockedIncrement(&g_locks);
        } else {
            InterlockedDecrement(&g_locks);
        }
        return S_OK;
    }

private:
    LONG ref_ = 1;
};

HMODULE g_module = nullptr;
volatile LONG g_logged = 0;

}  // namespace

BOOL APIENTRY DllMain(HMODULE module, DWORD reason, LPVOID) {
    if (reason == DLL_PROCESS_ATTACH) {
        g_module = module;
        DisableThreadLibraryCalls(module);
    }
    return TRUE;
}

extern "C" HRESULT STDMETHODCALLTYPE DllGetClassObject(REFCLSID clsid, REFIID riid, void** object) {
    if (InterlockedCompareExchange(&g_logged, 1, 0) == 0) {
        wchar_t path[MAX_PATH] = L"?";
        GetModuleFileNameW(g_module, path, MAX_PATH);
        SpikeLogf(L"carga dll=%s", path);
        SpikeLogIdentity();
    }
    if (object == nullptr) {
        return E_POINTER;
    }
    *object = nullptr;
    if (clsid != CLSID_CellCamSpike) {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    auto* factory = new (std::nothrow) ClassFactory();
    if (factory == nullptr) {
        return E_OUTOFMEMORY;
    }
    const HRESULT hr = factory->QueryInterface(riid, object);
    factory->Release();
    return hr;
}

extern "C" HRESULT STDMETHODCALLTYPE DllCanUnloadNow() {
    return (g_locks == 0 && g_objects == 0) ? S_OK : S_FALSE;
}
