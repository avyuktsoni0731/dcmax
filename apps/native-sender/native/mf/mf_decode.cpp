// Hardware H.264 decode through Media Foundation. The decoded NV12 is copied
// out so OpenGL can run the same limited-range conversion the Mac uses.
// A software Media Foundation decoder is the fallback when the GPU decoder
// will not start. openh264 remains the last resort in Rust.

#include <d3d11.h>
#include <mferror.h>
#include <mfapi.h>
#include <mfidl.h>
#include <mftransform.h>
#include <objbase.h>
#include <stdio.h>
#include <string.h>

#include <deque>
#include <vector>

// Some Windows SDKs hide this interface. The hardware decoder uses the device
// from its own thread, so the device has to be marked multithread-protected
// or a later Map of the NV12 readback fails and stalls teardown.
struct DcmaxD3D11Multithread : public IUnknown {
    virtual void STDMETHODCALLTYPE Enter(void) = 0;
    virtual void STDMETHODCALLTYPE Leave(void) = 0;
    virtual BOOL STDMETHODCALLTYPE SetMultithreadProtected(BOOL protect) = 0;
    virtual BOOL STDMETHODCALLTYPE GetMultithreadProtected(void) = 0;
};
static const GUID kDcmaxD3D11Multithread = {
    0x9B7E4E00, 0x342C, 0x4106, {0xA1, 0x9F, 0x4F, 0x27, 0x04, 0xF6, 0x89, 0xF0}};

struct HeldFrame {
    std::vector<uint8_t> y;
    std::vector<uint8_t> uv;
    int width;
    int height;
};

struct MfDecoder {
    IMFTransform* transform;
    ID3D11Device* device;
    ID3D11DeviceContext* context;
    ID3D11Texture2D* stage;
    IMFDXGIDeviceManager* manager;
    std::deque<HeldFrame> ready;
    std::vector<uint8_t> y;
    std::vector<uint8_t> uv;
    int width;
    int height;
    int hardware;
    int com_started;
    int gpu_output;
    int readback_note;
    UINT reset_token;
};

static char g_error[512] = "media foundation decode failed";

static void set_error(const char* message) {
    snprintf(g_error, sizeof(g_error), "%s", message);
}

static void set_hr(const char* message, HRESULT hr) {
    snprintf(g_error, sizeof(g_error), "%s (0x%08lx)", message, static_cast<unsigned long>(hr));
}

static void release_output(MFT_OUTPUT_DATA_BUFFER* out, IMFSample* owned, IMFMediaBuffer* owned_buf) {
    if (out->pEvents) {
        out->pEvents->Release();
        out->pEvents = nullptr;
    }
    if (out->pSample && out->pSample != owned) {
        out->pSample->Release();
    }
    out->pSample = nullptr;
    if (owned) {
        owned->Release();
    }
    if (owned_buf) {
        owned_buf->Release();
    }
}

static int current_size(MfDecoder* decoder, UINT32* width, UINT32* height, UINT32* stride) {
    IMFMediaType* type = nullptr;
    HRESULT hr = decoder->transform->GetOutputCurrentType(0, &type);
    if (FAILED(hr) || type == nullptr) {
        return -1;
    }
    hr = MFGetAttributeSize(type, MF_MT_FRAME_SIZE, width, height);
    UINT32 row_bits = 0;
    INT32 row = static_cast<INT32>(*width);
    if (SUCCEEDED(type->GetUINT32(MF_MT_DEFAULT_STRIDE, &row_bits))) {
        row = static_cast<INT32>(row_bits);
    }
    type->Release();
    if (FAILED(hr) || *width == 0 || *height < 2 || (*width % 2) != 0 || (*height % 2) != 0 || row < static_cast<INT32>(*width)) {
        return -1;
    }
    *stride = static_cast<UINT32>(row);
    return 0;
}

static int copy_planes(MfDecoder* decoder, const uint8_t* data, UINT32 width, UINT32 height, UINT32 stride) {
    if (data == nullptr || width > 7680 || height > 4320) {
        return -1;
    }
    const size_t y_bytes = static_cast<size_t>(width) * static_cast<size_t>(height);
    const size_t uv_bytes = y_bytes / 2;
    HeldFrame frame;
    frame.y.resize(y_bytes);
    frame.uv.resize(uv_bytes);
    for (UINT32 row = 0; row < height; ++row) {
        memcpy(frame.y.data() + static_cast<size_t>(row) * width, data + static_cast<size_t>(row) * stride, width);
    }
    const uint8_t* uv = data + static_cast<size_t>(stride) * height;
    for (UINT32 row = 0; row < height / 2; ++row) {
        memcpy(frame.uv.data() + static_cast<size_t>(row) * width, uv + static_cast<size_t>(row) * stride, width);
    }
    frame.width = static_cast<int>(width);
    frame.height = static_cast<int>(height);
    if (decoder->ready.size() >= 8) {
        decoder->ready.pop_front();
    }
    decoder->ready.push_back(std::move(frame));
    return 0;
}

static int emit_frame(MfDecoder* decoder, const uint8_t** y, const uint8_t** uv, int* width, int* height) {
    if (decoder->ready.empty()) {
        return 0;
    }
    HeldFrame frame = std::move(decoder->ready.front());
    decoder->ready.pop_front();
    decoder->y.swap(frame.y);
    decoder->uv.swap(frame.uv);
    decoder->width = frame.width;
    decoder->height = frame.height;
    *y = decoder->y.data();
    *uv = decoder->uv.data();
    *width = decoder->width;
    *height = decoder->height;
    return 1;
}

static int copy_dxgi(MfDecoder* decoder, IMFDXGIBuffer* dxgi) {
    ID3D11Texture2D* texture = nullptr;
    HRESULT hr = dxgi->GetResource(IID_PPV_ARGS(&texture));
    if (FAILED(hr) || texture == nullptr) {
        set_hr("dxgi frame has no texture", hr);
        return -1;
    }
    UINT subresource = 0;
    dxgi->GetSubresourceIndex(&subresource);
    D3D11_TEXTURE2D_DESC desc{};
    texture->GetDesc(&desc);
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    desc.MiscFlags = 0;
    desc.ArraySize = 1;
    desc.MipLevels = 1;
    desc.SampleDesc.Count = 1;
    if (decoder->stage != nullptr) {
        D3D11_TEXTURE2D_DESC have{};
        decoder->stage->GetDesc(&have);
        if (have.Width != desc.Width || have.Height != desc.Height) {
            decoder->stage->Release();
            decoder->stage = nullptr;
        }
    }
    auto make_stage = [&]() -> HRESULT {
        if (decoder->stage != nullptr) {
            decoder->stage->Release();
            decoder->stage = nullptr;
        }
        return decoder->device->CreateTexture2D(&desc, nullptr, &decoder->stage);
    };
    if (decoder->stage == nullptr) {
        hr = make_stage();
        if (FAILED(hr)) {
            texture->Release();
            set_hr("could not stage the decoded frame", hr);
            return -1;
        }
    }
    decoder->context->CopySubresourceRegion(decoder->stage, 0, 0, 0, 0, texture, subresource, nullptr);
    D3D11_MAPPED_SUBRESOURCE mapped{};
    hr = decoder->context->Map(decoder->stage, 0, D3D11_MAP_READ, 0, &mapped);
    if (FAILED(hr)) {
        // NVIDIA returns E_OUTOFMEMORY when a reused NV12 staging texture cannot
        // be locked. A fresh texture is the read that succeeds.
        HRESULT again = make_stage();
        if (SUCCEEDED(again)) {
            decoder->context->CopySubresourceRegion(decoder->stage, 0, 0, 0, 0, texture, subresource, nullptr);
            decoder->context->Flush();
            hr = decoder->context->Map(decoder->stage, 0, D3D11_MAP_READ, 0, &mapped);
        }
    }
    texture->Release();
    if (FAILED(hr)) {
        set_hr("could not read the decoded frame", hr);
        return -2;
    }
    int copied = copy_planes(
        decoder,
        static_cast<const uint8_t*>(mapped.pData),
        desc.Width,
        desc.Height,
        mapped.RowPitch);
    decoder->context->Unmap(decoder->stage, 0);
    return copied;
}

static int copy_sample(MfDecoder* decoder, IMFSample* sample) {
    IMFMediaBuffer* buffer = nullptr;
    HRESULT hr = sample->ConvertToContiguousBuffer(&buffer);
    if (FAILED(hr) || buffer == nullptr) {
        hr = sample->GetBufferByIndex(0, &buffer);
    }
    if (FAILED(hr) || buffer == nullptr) {
        set_error("decoded sample has no buffer");
        return -1;
    }
    IMFDXGIBuffer* dxgi = nullptr;
    if (SUCCEEDED(buffer->QueryInterface(IID_PPV_ARGS(&dxgi))) && dxgi != nullptr) {
        int copied = copy_dxgi(decoder, dxgi);
        dxgi->Release();
        buffer->Release();
        return copied;
    }
    BYTE* data = nullptr;
    DWORD length = 0;
    hr = buffer->Lock(&data, nullptr, &length);
    if (FAILED(hr) || data == nullptr) {
        buffer->Release();
        set_hr("could not lock the decoded frame", hr);
        return -1;
    }
    UINT32 width = 0;
    UINT32 height = 0;
    UINT32 stride = 0;
    int copied = -1;
    if (current_size(decoder, &width, &height, &stride) == 0) {
        copied = copy_planes(decoder, data, width, height, stride);
    } else {
        set_error("decoded frame has no size");
    }
    buffer->Unlock();
    buffer->Release();
    return copied;
}

static int select_nv12(MfDecoder* decoder) {
    for (DWORD index = 0;; ++index) {
        IMFMediaType* type = nullptr;
        HRESULT hr = decoder->transform->GetOutputAvailableType(0, index, &type);
        if (FAILED(hr) || type == nullptr) {
            return -1;
        }
        GUID subtype = GUID_NULL;
        type->GetGUID(MF_MT_SUBTYPE, &subtype);
        if (subtype == MFVideoFormat_NV12) {
            hr = decoder->transform->SetOutputType(0, type, 0);
            type->Release();
            return SUCCEEDED(hr) ? 0 : -1;
        }
        type->Release();
    }
}

static int pull(MfDecoder* decoder, int depth) {
    if (depth > 3) {
        set_error("decoder kept changing its output");
        return -1;
    }
    MFT_OUTPUT_STREAM_INFO info{};
    decoder->transform->GetOutputStreamInfo(0, &info);
    MFT_OUTPUT_DATA_BUFFER out{};
    out.dwStreamID = 0;
    IMFSample* owned = nullptr;
    IMFMediaBuffer* owned_buf = nullptr;
    if ((info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES) == 0) {
        DWORD bytes = info.cbSize;
        if (bytes == 0) {
            bytes = 1920 * 1080 * 3 / 2;
        }
        if (FAILED(MFCreateMemoryBuffer(bytes, &owned_buf)) || FAILED(MFCreateSample(&owned)) ||
            FAILED(owned->AddBuffer(owned_buf))) {
            if (owned) {
                owned->Release();
            }
            if (owned_buf) {
                owned_buf->Release();
            }
            set_error("could not allocate a decode buffer");
            return -1;
        }
        out.pSample = owned;
    }
    DWORD status = 0;
    HRESULT hr = decoder->transform->ProcessOutput(0, 1, &out, &status);
    if (hr == MF_E_TRANSFORM_NEED_MORE_INPUT) {
        release_output(&out, owned, owned_buf);
        return 0;
    }
    if (hr == MF_E_TRANSFORM_STREAM_CHANGE || hr == MF_E_TRANSFORM_TYPE_NOT_SET) {
        release_output(&out, owned, owned_buf);
        if (select_nv12(decoder) != 0) {
            set_error("decoder has no nv12 output");
            return -1;
        }
        return pull(decoder, depth + 1);
    }
    if (FAILED(hr) || out.pSample == nullptr) {
        release_output(&out, owned, owned_buf);
        set_hr("decode output failed", hr);
        return -1;
    }
    int copied = copy_sample(decoder, out.pSample);
    release_output(&out, owned, owned_buf);
    if (copied == -2) {
        // The frame was decoded. Only the GPU readback failed. Keep the
        // decoder and ask it for ordinary memory next time. Do not flush:
        // a flush here waits on the same device and stalls the viewer.
        if (decoder->stage) {
            decoder->stage->Release();
            decoder->stage = nullptr;
        }
        if (decoder->transform && decoder->gpu_output) {
            decoder->transform->ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, 0);
            decoder->gpu_output = 0;
            decoder->readback_note = 1;
            return 0;
        }
        return -1;
    }
    return copied == 0 ? 1 : -1;
}

static int set_input(IMFTransform* transform) {
    IMFMediaType* type = nullptr;
    HRESULT hr = MFCreateMediaType(&type);
    if (FAILED(hr) || type == nullptr) {
        return -1;
    }
    type->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video);
    type->SetGUID(MF_MT_SUBTYPE, MFVideoFormat_H264);
    type->SetUINT32(MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive);
    hr = transform->SetInputType(0, type, 0);
    type->Release();
    return SUCCEEDED(hr) ? 0 : -1;
}

static int is_async(IMFTransform* transform) {
    IMFAttributes* attributes = nullptr;
    if (FAILED(transform->GetAttributes(&attributes)) || attributes == nullptr) {
        return 0;
    }
    UINT32 async = 0;
    attributes->GetUINT32(MF_TRANSFORM_ASYNC, &async);
    attributes->Release();
    return async != 0;
}

static int accept_transform(MfDecoder* decoder, IMFTransform* transform, int hardware) {
    if (is_async(transform)) {
        return -1;
    }
    if (hardware && decoder->manager != nullptr) {
        HRESULT hr = transform->ProcessMessage(
            MFT_MESSAGE_SET_D3D_MANAGER,
            reinterpret_cast<ULONG_PTR>(decoder->manager));
        if (FAILED(hr)) {
            set_hr("decoder rejected the gpu device", hr);
            return -1;
        }
    }
    if (set_input(transform) != 0) {
        set_error("decoder rejected the h264 input");
        return -1;
    }
    decoder->transform = transform;
    decoder->hardware = hardware;
    decoder->gpu_output = hardware ? 1 : 0;
    IMFAttributes* attributes = nullptr;
    if (SUCCEEDED(transform->GetAttributes(&attributes)) && attributes != nullptr) {
        attributes->SetUINT32(MF_LOW_LATENCY, TRUE);
        attributes->Release();
    }
    transform->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
    transform->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
    return 0;
}

static int open_enum(MfDecoder* decoder, DWORD flags, int hardware) {
    MFT_REGISTER_TYPE_INFO input{MFMediaType_Video, MFVideoFormat_H264};
    MFT_REGISTER_TYPE_INFO output{MFMediaType_Video, MFVideoFormat_NV12};
    IMFActivate** activates = nullptr;
    UINT32 count = 0;
    HRESULT hr = MFTEnumEx(
        MFT_CATEGORY_VIDEO_DECODER,
        flags,
        &input,
        &output,
        &activates,
        &count);
    if (FAILED(hr) || count == 0 || activates == nullptr) {
        return -1;
    }
    int opened = -1;
    for (UINT32 index = 0; index < count; ++index) {
        if (opened != 0 && activates[index] != nullptr) {
            IMFTransform* transform = nullptr;
            if (SUCCEEDED(activates[index]->ActivateObject(IID_PPV_ARGS(&transform))) && transform != nullptr) {
                if (accept_transform(decoder, transform, hardware) == 0) {
                    opened = 0;
                } else {
                    transform->Release();
                }
            }
        }
        if (activates[index] != nullptr) {
            activates[index]->Release();
        }
    }
    CoTaskMemFree(activates);
    return opened;
}

static int open_device(MfDecoder* decoder) {
    UINT flags = D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT;
    HRESULT hr = D3D11CreateDevice(
        nullptr,
        D3D_DRIVER_TYPE_HARDWARE,
        nullptr,
        flags,
        nullptr,
        0,
        D3D11_SDK_VERSION,
        &decoder->device,
        nullptr,
        &decoder->context);
    if (FAILED(hr)) {
        set_hr("could not open a gpu for decode", hr);
        return -1;
    }
    hr = MFCreateDXGIDeviceManager(&decoder->reset_token, &decoder->manager);
    if (FAILED(hr)) {
        set_hr("could not attach the gpu to the decoder", hr);
        return -1;
    }
    hr = decoder->manager->ResetDevice(decoder->device, decoder->reset_token);
    if (FAILED(hr)) {
        set_hr("could not reset the decode device", hr);
        return -1;
    }
    DcmaxD3D11Multithread* guard = nullptr;
    if (SUCCEEDED(decoder->device->QueryInterface(kDcmaxD3D11Multithread, reinterpret_cast<void**>(&guard))) &&
        guard != nullptr) {
        guard->SetMultithreadProtected(TRUE);
        guard->Release();
    }
    return 0;
}

extern "C" {

MfDecoder* mf_open(void) {
    MfDecoder* decoder = new MfDecoder();
    decoder->transform = nullptr;
    decoder->device = nullptr;
    decoder->context = nullptr;
    decoder->stage = nullptr;
    decoder->manager = nullptr;
    decoder->width = 0;
    decoder->height = 0;
    decoder->hardware = 0;
    decoder->com_started = 0;
    decoder->gpu_output = 0;
    decoder->readback_note = 0;
    decoder->reset_token = 0;
    HRESULT hr = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    decoder->com_started = SUCCEEDED(hr);
    hr = MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET);
    if (FAILED(hr)) {
        set_hr("media foundation did not start", hr);
        delete decoder;
        return nullptr;
    }
    if (open_device(decoder) == 0) {
        DWORD hardware = MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER;
        if (open_enum(decoder, hardware, 1) == 0) {
            return decoder;
        }
    }
    DWORD software = MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER | MFT_ENUM_FLAG_LOCALMFT;
    if (open_enum(decoder, software, 0) == 0) {
        return decoder;
    }
    set_error("no h264 decoder is available");
    if (decoder->manager) {
        decoder->manager->Release();
    }
    if (decoder->context) {
        decoder->context->Release();
    }
    if (decoder->device) {
        decoder->device->Release();
    }
    if (decoder->com_started) {
        CoUninitialize();
    }
    delete decoder;
    return nullptr;
}

const char* mf_last_error(void) {
    return g_error;
}

int mf_hardware(MfDecoder* decoder) {
    return decoder != nullptr && decoder->hardware ? 1 : 0;
}

int mf_take_note(MfDecoder* decoder, char* out, int cap) {
    if (decoder == nullptr || decoder->readback_note == 0 || out == nullptr || cap < 16) {
        return 0;
    }
    decoder->readback_note = 0;
    snprintf(out, static_cast<size_t>(cap), "gpu readback failed, using system memory");
    return 1;
}

int mf_decode(
    MfDecoder* decoder,
    const uint8_t* data,
    int length,
    const uint8_t** y,
    const uint8_t** uv,
    int* width,
    int* height) {
    if (decoder == nullptr || decoder->transform == nullptr || data == nullptr || length <= 0) {
        return -1;
    }
    IMFMediaBuffer* buffer = nullptr;
    IMFSample* sample = nullptr;
    HRESULT hr = MFCreateMemoryBuffer(static_cast<DWORD>(length), &buffer);
    if (FAILED(hr)) {
        set_hr("could not allocate the input frame", hr);
        return -1;
    }
    BYTE* dest = nullptr;
    hr = buffer->Lock(&dest, nullptr, nullptr);
    if (FAILED(hr) || dest == nullptr) {
        buffer->Release();
        set_hr("could not write the input frame", hr);
        return -1;
    }
    memcpy(dest, data, static_cast<size_t>(length));
    buffer->Unlock();
    buffer->SetCurrentLength(static_cast<DWORD>(length));
    hr = MFCreateSample(&sample);
    if (FAILED(hr) || FAILED(sample->AddBuffer(buffer))) {
        if (sample) {
            sample->Release();
        }
        buffer->Release();
        set_error("could not wrap the input frame");
        return -1;
    }
    buffer->Release();
    hr = decoder->transform->ProcessInput(0, sample, 0);
    sample->Release();
    if (hr == MF_E_NOTACCEPTING) {
        // The decoder is holding a finished frame. Pull it into the queue
        // before submitting this one again. The old path pulled that frame
        // and then overwrote it, so the viewer showed about half the frames.
        for (int attempt = 0; attempt < 4 && hr == MF_E_NOTACCEPTING; ++attempt) {
            int pulled = pull(decoder, 0);
            if (pulled < 0) {
                return decoder->ready.empty() ? -1 : emit_frame(decoder, y, uv, width, height);
            }
            IMFSample* retry = nullptr;
            IMFMediaBuffer* retry_buf = nullptr;
            if (FAILED(MFCreateMemoryBuffer(static_cast<DWORD>(length), &retry_buf)) ||
                FAILED(MFCreateSample(&retry)) || FAILED(retry->AddBuffer(retry_buf))) {
                if (retry) {
                    retry->Release();
                }
                if (retry_buf) {
                    retry_buf->Release();
                }
                return emit_frame(decoder, y, uv, width, height);
            }
            BYTE* again = nullptr;
            retry_buf->Lock(&again, nullptr, nullptr);
            memcpy(again, data, static_cast<size_t>(length));
            retry_buf->Unlock();
            retry_buf->SetCurrentLength(static_cast<DWORD>(length));
            hr = decoder->transform->ProcessInput(0, retry, 0);
            retry_buf->Release();
            retry->Release();
        }
    }
    if (FAILED(hr)) {
        set_hr("decoder rejected a frame", hr);
        return decoder->ready.empty() ? -1 : emit_frame(decoder, y, uv, width, height);
    }
    for (;;) {
        int pulled = pull(decoder, 0);
        if (pulled == 0) {
            break;
        }
        if (pulled < 0) {
            return decoder->ready.empty() ? -1 : emit_frame(decoder, y, uv, width, height);
        }
    }
    return emit_frame(decoder, y, uv, width, height);
}

void mf_close(MfDecoder* decoder) {
    if (decoder == nullptr) {
        return;
    }
    if (decoder->transform) {
        // End-of-stream and flush wait on the decode device. Releasing the
        // transform is enough when the viewer is dropping the share, and it
        // does not leave the picture thread stuck on the last frame.
        decoder->transform->Release();
    }
    if (decoder->stage) {
        decoder->stage->Release();
    }
    if (decoder->manager) {
        decoder->manager->Release();
    }
    if (decoder->context) {
        decoder->context->Release();
    }
    if (decoder->device) {
        decoder->device->Release();
    }
    if (decoder->com_started) {
        CoUninitialize();
    }
    delete decoder;
}

}
