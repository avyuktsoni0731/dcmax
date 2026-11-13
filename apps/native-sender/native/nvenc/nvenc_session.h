#pragma once

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct NvencSession NvencSession;

// Opens an H.264 session on the D3D11 device that owns the capture textures.
// Returns null on failure; nvenc_open_error() is then valid.
NvencSession* nvenc_open(void* device, int width, int height, int fps, int bitrate_bps, int gop);

const char* nvenc_open_error(void);
const char* nvenc_last_error(NvencSession* session);

// Copies the newest BGRA frame into the session. Safe to call from the capture callback.
int nvenc_blit(NvencSession* session, void* texture);

// Encodes the newest copied frame. Writes Annex-B bytes into out.
// Returns 0 on success. is_key is 1 for an IDR or I frame.
int nvenc_encode(
    NvencSession* session,
    int force_idr,
    uint8_t* out,
    int out_cap,
    int* out_len,
    int* is_key);

// Changes CBR bitrate without resetting the session or forcing an IDR.
int nvenc_set_bitrate(NvencSession* session, int bitrate_bps);

void nvenc_close(NvencSession* session);

#ifdef __cplusplus
}
#endif
