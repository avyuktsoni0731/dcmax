#pragma once

#include <memory>

namespace webrtc {
class CustomProcessing;
}

namespace livekit_ffi {

// Capture post-processor. Echo cancellation still runs inside WebRTC;
// this stage sees the mic after that and can replace the built-in suppressor.
std::unique_ptr<webrtc::CustomProcessing> create_noise_suppression();

}  // namespace livekit_ffi

// `channels` is planar, one pointer per channel. Samples are float in [-1, 1].
using DcmaxNoiseFn = void (*)(float* const* channels, int sample_rate, int num_channels, int num_frames);

extern "C" void dcmax_set_noise_processor(DcmaxNoiseFn fn);
