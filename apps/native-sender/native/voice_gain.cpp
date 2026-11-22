#include "api/media_stream_interface.h"

#include <memory>

// livekit_ffi::MediaStreamTrack starts with a std::shared_ptr, then the
// scoped_refptr that holds the WebRTC track. That pointer is what SetVolume
// needs. The offset is the shared_ptr, which is two pointers on the ABIs we
// ship.
static_assert(sizeof(std::shared_ptr<int>) == 16, "voice volume reads the track at byte 16");

extern "C" void dcmax_set_voice_volume(void* media_track, double volume) {
    if (media_track == nullptr) {
        return;
    }
    if (volume < 0.0) {
        volume = 0.0;
    }
    if (volume > 10.0) {
        volume = 10.0;
    }
    auto* bytes = static_cast<unsigned char*>(media_track);
    auto* track = *reinterpret_cast<webrtc::MediaStreamTrackInterface**>(bytes + 16);
    if (track == nullptr) {
        return;
    }
    static_cast<webrtc::AudioTrackInterface*>(track)->SetVolume(volume);
}
