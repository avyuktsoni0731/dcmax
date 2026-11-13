#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <d3d11.h>
#include <dxgi.h>

#include <cstdio>
#include <cstring>
#include <mutex>

#include "nvEncodeAPI.h"
#include "nvenc_session.h"

struct NvencSession {
    std::mutex mutex;
    char error[512];
    HMODULE module = nullptr;
    NV_ENCODE_API_FUNCTION_LIST api{};
    void* encoder = nullptr;
    ID3D11Device* device = nullptr;
    ID3D11DeviceContext* context = nullptr;
    ID3D11Texture2D* scratch[2] = {};
    ID3D11Texture2D* encode_tex = nullptr;
    ID3D11Query* blit_query[2] = {};
    ID3D11Query* copy_query = nullptr;
    bool blit_issued[2] = {};
    int write_slot = 0;
    int latest_slot = -1;
    NV_ENC_REGISTERED_PTR registered = nullptr;
    NV_ENC_OUTPUT_PTR bitstream = nullptr;
    NV_ENC_INITIALIZE_PARAMS init{};
    NV_ENC_CONFIG config{};
    int width = 0;
    int height = 0;
    int fps = 1;
    bool has_frame = false;
    uint64_t frame_index = 0;
};

static char g_open_error[512] = "nvenc failed";

static void set_open_error(const char* message) {
    std::snprintf(g_open_error, sizeof(g_open_error), "%s", message);
}

static void set_error(NvencSession* session, const char* message) {
    std::snprintf(session->error, sizeof(session->error), "%s", message);
}

static bool check(NvencSession* session, NVENCSTATUS status, const char* what) {
    if (status == NV_ENC_SUCCESS) {
        return true;
    }
    const char* detail = "";
    if (session->encoder != nullptr && session->api.nvEncGetLastErrorString != nullptr) {
        const char* reported = session->api.nvEncGetLastErrorString(session->encoder);
        if (reported != nullptr) {
            detail = reported;
        }
    }
    std::snprintf(
        session->error,
        sizeof(session->error),
        "%s failed (status %d): %s",
        what,
        static_cast<int>(status),
        detail);
    return false;
}

static bool create_texture(NvencSession* session, ID3D11Texture2D** target) {
    D3D11_TEXTURE2D_DESC desc{};
    desc.Width = static_cast<UINT>(session->width);
    desc.Height = static_cast<UINT>(session->height);
    desc.MipLevels = 1;
    desc.ArraySize = 1;
    desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
    desc.SampleDesc.Count = 1;
    desc.Usage = D3D11_USAGE_DEFAULT;
    desc.BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
    HRESULT hr = session->device->CreateTexture2D(&desc, nullptr, target);
    if (FAILED(hr)) {
        std::snprintf(session->error, sizeof(session->error), "CreateTexture2D failed (0x%08lx)", static_cast<unsigned long>(hr));
        return false;
    }
    return true;
}

static bool wait_for_gpu(NvencSession* session, ID3D11Query* query, bool end_first) {
    if (query == nullptr) {
        set_error(session, "GPU query is missing");
        return false;
    }
    if (end_first) {
        session->context->End(query);
    }
    ULONGLONG started = GetTickCount64();
    for (;;) {
        HRESULT hr = session->context->GetData(query, nullptr, 0, 0);
        if (hr != S_FALSE) {
            return SUCCEEDED(hr);
        }
        if (GetTickCount64() - started > 1000) {
            set_error(session, "GPU copy did not finish within 1s");
            return false;
        }
        SwitchToThread();
    }
}

static int clamp_bitrate(int bitrate_bps) {
    if (bitrate_bps < 800000) {
        return 800000;
    }
    if (bitrate_bps > 50000000) {
        return 50000000;
    }
    return bitrate_bps;
}

static void apply_bitrate(NvencSession* session, int bitrate_bps) {
    uint32_t bitrate = static_cast<uint32_t>(clamp_bitrate(bitrate_bps));
    uint32_t frame_bits = bitrate / static_cast<uint32_t>(session->fps);
    if (frame_bits < 1) {
        frame_bits = 1;
    }
    session->config.rcParams.averageBitRate = bitrate;
    session->config.rcParams.maxBitRate = bitrate;
    session->config.rcParams.vbvBufferSize = frame_bits;
    session->config.rcParams.vbvInitialDelay = frame_bits;
}

extern "C" const char* nvenc_open_error(void) {
    return g_open_error;
}

extern "C" const char* nvenc_last_error(NvencSession* session) {
    if (session == nullptr) {
        return g_open_error;
    }
    return session->error;
}

extern "C" NvencSession* nvenc_open(void* device_ptr, int width, int height, int fps, int bitrate_bps, int gop) {
    auto* session = new NvencSession();
    session->error[0] = '\0';
    session->width = width;
    session->height = height;
    session->fps = fps > 0 ? fps : 1;
    if (gop < 1) {
        gop = session->fps * 2;
    }
    if (device_ptr == nullptr || width < 2 || height < 2) {
        set_error(session, "NVENC open received an empty capture device or size");
        set_open_error(session->error);
        delete session;
        return nullptr;
    }

    session->module = LoadLibraryW(L"nvEncodeAPI64.dll");
    if (session->module == nullptr) {
        set_error(session, "nvEncodeAPI64.dll was not found. The NVIDIA driver should provide it.");
        set_open_error(session->error);
        delete session;
        return nullptr;
    }
    auto create = reinterpret_cast<NVENCSTATUS(NVENCAPI*)(NV_ENCODE_API_FUNCTION_LIST*)>(
        GetProcAddress(session->module, "NvEncodeAPICreateInstance"));
    if (create == nullptr) {
        set_error(session, "nvEncodeAPI64.dll is missing NvEncodeAPICreateInstance");
        set_open_error(session->error);
        FreeLibrary(session->module);
        delete session;
        return nullptr;
    }

    session->api = {};
    session->api.version = NV_ENCODE_API_FUNCTION_LIST_VER;
    NVENCSTATUS status = create(&session->api);
    if (status != NV_ENC_SUCCESS) {
        std::snprintf(
            session->error,
            sizeof(session->error),
            "NvEncodeAPICreateInstance failed (status %d, API %u.%u)",
            static_cast<int>(status),
            NVENCAPI_MAJOR_VERSION,
            NVENCAPI_MINOR_VERSION);
        set_open_error(session->error);
        FreeLibrary(session->module);
        delete session;
        return nullptr;
    }

    session->device = static_cast<ID3D11Device*>(device_ptr);
    session->device->AddRef();
    session->device->GetImmediateContext(&session->context);

    NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS open_params{};
    open_params.version = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER;
    open_params.deviceType = NV_ENC_DEVICE_TYPE_DIRECTX;
    open_params.device = session->device;
    open_params.apiVersion = NVENCAPI_VERSION;
    status = session->api.nvEncOpenEncodeSessionEx(&open_params, &session->encoder);
    if (!check(session, status, "NvEncOpenEncodeSessionEx")) {
        char message[512];
        std::snprintf(
            message,
            sizeof(message),
            "%s (API %u.%u). The installed driver may be older than this header.",
            session->error,
            NVENCAPI_MAJOR_VERSION,
            NVENCAPI_MINOR_VERSION);
        set_open_error(message);
        nvenc_close(session);
        return nullptr;
    }

    NV_ENC_PRESET_CONFIG preset{};
    preset.version = NV_ENC_PRESET_CONFIG_VER;
    preset.presetCfg.version = NV_ENC_CONFIG_VER;
    status = session->api.nvEncGetEncodePresetConfigEx(
        session->encoder,
        NV_ENC_CODEC_H264_GUID,
        NV_ENC_PRESET_P4_GUID,
        NV_ENC_TUNING_INFO_LOW_LATENCY,
        &preset);
    if (!check(session, status, "NvEncGetEncodePresetConfigEx")) {
        set_open_error(session->error);
        nvenc_close(session);
        return nullptr;
    }

    session->config = preset.presetCfg;
    session->config.version = NV_ENC_CONFIG_VER;
    session->config.profileGUID = NV_ENC_H264_PROFILE_HIGH_GUID;
    session->config.gopLength = static_cast<uint32_t>(gop);
    session->config.frameIntervalP = 1;
    session->config.rcParams.rateControlMode = NV_ENC_PARAMS_RC_CBR;
    session->config.rcParams.enableLookahead = 0;
    session->config.rcParams.lookaheadDepth = 0;
    session->config.rcParams.zeroReorderDelay = 1;
    session->config.rcParams.multiPass = NV_ENC_MULTI_PASS_DISABLED;
    session->config.rcParams.strictGOPTarget = 1;
    apply_bitrate(session, bitrate_bps);
    auto& h264 = session->config.encodeCodecConfig.h264Config;
    h264.idrPeriod = static_cast<uint32_t>(gop);
    h264.repeatSPSPPS = 1;
    h264.outputAUD = 1;
    h264.disableSPSPPS = 0;
    h264.outputBufferingPeriodSEI = 0;
    h264.outputPictureTimingSEI = 0;

    session->init = {};
    session->init.version = NV_ENC_INITIALIZE_PARAMS_VER;
    session->init.encodeGUID = NV_ENC_CODEC_H264_GUID;
    session->init.presetGUID = NV_ENC_PRESET_P4_GUID;
    session->init.tuningInfo = NV_ENC_TUNING_INFO_LOW_LATENCY;
    session->init.encodeWidth = static_cast<uint32_t>(width);
    session->init.encodeHeight = static_cast<uint32_t>(height);
    session->init.darWidth = static_cast<uint32_t>(width);
    session->init.darHeight = static_cast<uint32_t>(height);
    session->init.frameRateNum = static_cast<uint32_t>(session->fps);
    session->init.frameRateDen = 1;
    session->init.enableEncodeAsync = 0;
    session->init.enablePTD = 1;
    session->init.encodeConfig = &session->config;
    session->init.maxEncodeWidth = static_cast<uint32_t>(width);
    session->init.maxEncodeHeight = static_cast<uint32_t>(height);
    status = session->api.nvEncInitializeEncoder(session->encoder, &session->init);
    if (!check(session, status, "NvEncInitializeEncoder")) {
        set_open_error(session->error);
        nvenc_close(session);
        return nullptr;
    }

    if (!create_texture(session, &session->scratch[0]) || !create_texture(session, &session->scratch[1]) ||
        !create_texture(session, &session->encode_tex)) {
        set_open_error(session->error);
        nvenc_close(session);
        return nullptr;
    }
    D3D11_QUERY_DESC query_desc{};
    query_desc.Query = D3D11_QUERY_EVENT;
    HRESULT hr = session->device->CreateQuery(&query_desc, &session->blit_query[0]);
    if (SUCCEEDED(hr)) {
        hr = session->device->CreateQuery(&query_desc, &session->blit_query[1]);
    }
    if (SUCCEEDED(hr)) {
        hr = session->device->CreateQuery(&query_desc, &session->copy_query);
    }
    if (FAILED(hr)) {
        std::snprintf(session->error, sizeof(session->error), "CreateQuery failed (0x%08lx)", static_cast<unsigned long>(hr));
        set_open_error(session->error);
        nvenc_close(session);
        return nullptr;
    }

    NV_ENC_REGISTER_RESOURCE resource{};
    resource.version = NV_ENC_REGISTER_RESOURCE_VER;
    resource.resourceType = NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX;
    resource.width = static_cast<uint32_t>(width);
    resource.height = static_cast<uint32_t>(height);
    resource.pitch = 0;
    resource.subResourceIndex = 0;
    resource.resourceToRegister = session->encode_tex;
    resource.bufferFormat = NV_ENC_BUFFER_FORMAT_ARGB;
    resource.bufferUsage = NV_ENC_INPUT_IMAGE;
    status = session->api.nvEncRegisterResource(session->encoder, &resource);
    if (!check(session, status, "NvEncRegisterResource")) {
        set_open_error(session->error);
        nvenc_close(session);
        return nullptr;
    }
    session->registered = resource.registeredResource;

    NV_ENC_CREATE_BITSTREAM_BUFFER bits{};
    bits.version = NV_ENC_CREATE_BITSTREAM_BUFFER_VER;
    status = session->api.nvEncCreateBitstreamBuffer(session->encoder, &bits);
    if (!check(session, status, "NvEncCreateBitstreamBuffer")) {
        set_open_error(session->error);
        nvenc_close(session);
        return nullptr;
    }
    session->bitstream = bits.bitstreamBuffer;
    return session;
}

extern "C" int nvenc_blit(NvencSession* session, void* texture) {
    if (session == nullptr || texture == nullptr) {
        return -1;
    }
    std::lock_guard<std::mutex> lock(session->mutex);
    auto* source = static_cast<ID3D11Texture2D*>(texture);
    D3D11_TEXTURE2D_DESC desc{};
    source->GetDesc(&desc);
    if (desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM) {
        std::snprintf(session->error, sizeof(session->error), "capture texture format %u is not BGRA8", static_cast<unsigned>(desc.Format));
        return -1;
    }
    if (desc.Width < static_cast<UINT>(session->width) || desc.Height < static_cast<UINT>(session->height)) {
        std::snprintf(
            session->error,
            sizeof(session->error),
            "capture texture %ux%u is smaller than the encoder %dx%d",
            desc.Width,
            desc.Height,
            session->width,
            session->height);
        return -1;
    }
    D3D11_BOX box{};
    box.right = static_cast<UINT>(session->width);
    box.bottom = static_cast<UINT>(session->height);
    box.back = 1;
    // Copy into the scratch the encoder is not reading, and return without
    // waiting for the encode. The capture pool can hand over the next refresh
    // while NVENC is still compressing this one.
    const int slot = session->write_slot;
    if (session->blit_issued[slot]) {
        HRESULT ready = session->context->GetData(
            session->blit_query[slot], nullptr, 0, D3D11_ASYNC_GETDATA_DONOTFLUSH);
        if (ready == S_FALSE) {
            ready = session->context->GetData(session->blit_query[slot], nullptr, 0, 0);
        }
        if (ready != S_OK) {
            return 1;
        }
        session->blit_issued[slot] = false;
    }
    session->context->CopySubresourceRegion(session->scratch[slot], 0, 0, 0, 0, source, 0, &box);
    session->context->End(session->blit_query[slot]);
    session->blit_issued[slot] = true;
    session->latest_slot = slot;
    session->write_slot = slot ^ 1;
    session->has_frame = true;
    return 0;
}

extern "C" int nvenc_encode(
    NvencSession* session,
    int force_idr,
    uint8_t* out,
    int out_cap,
    int* out_len,
    int* is_key) {
    if (session == nullptr || out == nullptr || out_len == nullptr || is_key == nullptr) {
        return -1;
    }
    std::unique_lock<std::mutex> lock(session->mutex);
    if (!session->has_frame || session->latest_slot < 0) {
        set_error(session, "NVENC encode was asked for a frame before capture produced one");
        return -1;
    }
    const int slot = session->latest_slot;
    session->has_frame = false;
    if (!wait_for_gpu(session, session->blit_query[slot], false)) {
        return -1;
    }
    session->context->CopyResource(session->encode_tex, session->scratch[slot]);
    if (!wait_for_gpu(session, session->copy_query, true)) {
        return -1;
    }

    NV_ENC_MAP_INPUT_RESOURCE map{};
    map.version = NV_ENC_MAP_INPUT_RESOURCE_VER;
    map.registeredResource = session->registered;
    if (!check(session, session->api.nvEncMapInputResource(session->encoder, &map), "NvEncMapInputResource")) {
        return -1;
    }

    NV_ENC_PIC_PARAMS picture{};
    picture.version = NV_ENC_PIC_PARAMS_VER;
    picture.inputWidth = static_cast<uint32_t>(session->width);
    picture.inputHeight = static_cast<uint32_t>(session->height);
    picture.inputPitch = static_cast<uint32_t>(session->width);
    picture.frameIdx = static_cast<uint32_t>(session->frame_index);
    picture.inputTimeStamp = session->frame_index;
    picture.inputDuration = 1;
    picture.inputBuffer = map.mappedResource;
    picture.outputBitstream = session->bitstream;
    picture.bufferFmt = map.mappedBufferFmt;
    picture.pictureStruct = NV_ENC_PIC_STRUCT_FRAME;
    if (force_idr) {
        picture.encodePicFlags = NV_ENC_PIC_FLAG_FORCEIDR | NV_ENC_PIC_FLAG_OUTPUT_SPSPPS;
    }
    session->frame_index += 1;
    // The hardware encode does not need the capture texture. Releasing the
    // lock lets the next refresh copy while this picture is compressed.
    lock.unlock();
    NVENCSTATUS status = session->api.nvEncEncodePicture(session->encoder, &picture);
    lock.lock();
    if (status != NV_ENC_SUCCESS) {
        check(session, status, "NvEncEncodePicture");
        session->api.nvEncUnmapInputResource(session->encoder, map.mappedResource);
        return -1;
    }

    NV_ENC_LOCK_BITSTREAM locked{};
    locked.version = NV_ENC_LOCK_BITSTREAM_VER;
    locked.outputBitstream = session->bitstream;
    status = session->api.nvEncLockBitstream(session->encoder, &locked);
    if (status != NV_ENC_SUCCESS) {
        check(session, status, "NvEncLockBitstream");
        session->api.nvEncUnmapInputResource(session->encoder, map.mappedResource);
        return -1;
    }
    int written = -1;
    if (locked.bitstreamBufferPtr != nullptr && locked.bitstreamSizeInBytes > 0 &&
        locked.bitstreamSizeInBytes <= static_cast<uint32_t>(out_cap)) {
        std::memcpy(out, locked.bitstreamBufferPtr, locked.bitstreamSizeInBytes);
        *out_len = static_cast<int>(locked.bitstreamSizeInBytes);
        *is_key = (locked.pictureType == NV_ENC_PIC_TYPE_IDR || locked.pictureType == NV_ENC_PIC_TYPE_I) ? 1 : 0;
        written = 0;
    } else {
        std::snprintf(
            session->error,
            sizeof(session->error),
            "encoded frame is %u bytes and does not fit the %d byte output buffer",
            locked.bitstreamSizeInBytes,
            out_cap);
    }
    session->api.nvEncUnlockBitstream(session->encoder, session->bitstream);
    session->api.nvEncUnmapInputResource(session->encoder, map.mappedResource);
    return written;
}

extern "C" int nvenc_set_bitrate(NvencSession* session, int bitrate_bps) {
    if (session == nullptr) {
        return -1;
    }
    std::lock_guard<std::mutex> lock(session->mutex);
    apply_bitrate(session, bitrate_bps);
    session->init.encodeConfig = &session->config;
    NV_ENC_RECONFIGURE_PARAMS reconfig{};
    reconfig.version = NV_ENC_RECONFIGURE_PARAMS_VER;
    reconfig.reInitEncodeParams = session->init;
    reconfig.reInitEncodeParams.encodeConfig = &session->config;
    reconfig.resetEncoder = 0;
    reconfig.forceIDR = 0;
    if (!check(session, session->api.nvEncReconfigureEncoder(session->encoder, &reconfig), "NvEncReconfigureEncoder")) {
        return -1;
    }
    return 0;
}

extern "C" void nvenc_close(NvencSession* session) {
    if (session == nullptr) {
        return;
    }
    {
        std::lock_guard<std::mutex> lock(session->mutex);
        if (session->encoder != nullptr) {
            if (session->registered != nullptr && session->api.nvEncUnregisterResource != nullptr) {
                session->api.nvEncUnregisterResource(session->encoder, session->registered);
                session->registered = nullptr;
            }
            if (session->bitstream != nullptr && session->api.nvEncDestroyBitstreamBuffer != nullptr) {
                session->api.nvEncDestroyBitstreamBuffer(session->encoder, session->bitstream);
                session->bitstream = nullptr;
            }
            if (session->api.nvEncDestroyEncoder != nullptr) {
                session->api.nvEncDestroyEncoder(session->encoder);
                session->encoder = nullptr;
            }
        }
        if (session->copy_query != nullptr) {
            session->copy_query->Release();
            session->copy_query = nullptr;
        }
        for (int slot = 0; slot < 2; ++slot) {
            if (session->blit_query[slot] != nullptr) {
                session->blit_query[slot]->Release();
                session->blit_query[slot] = nullptr;
            }
            if (session->scratch[slot] != nullptr) {
                session->scratch[slot]->Release();
                session->scratch[slot] = nullptr;
            }
        }
        if (session->encode_tex != nullptr) {
            session->encode_tex->Release();
            session->encode_tex = nullptr;
        }
        if (session->context != nullptr) {
            session->context->Release();
            session->context = nullptr;
        }
        if (session->device != nullptr) {
            session->device->Release();
            session->device = nullptr;
        }
        if (session->module != nullptr) {
            FreeLibrary(session->module);
            session->module = nullptr;
        }
    }
    delete session;
}
