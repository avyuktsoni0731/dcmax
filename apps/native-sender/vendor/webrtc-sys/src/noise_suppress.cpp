#include "livekit/noise_suppress.h"

#include <atomic>
#include <memory>
#include <string>

#include "api/audio/audio_processing.h"
#include "modules/audio_processing/audio_buffer.h"

namespace {

std::atomic<DcmaxNoiseFn> g_noise{nullptr};

class NoiseSuppression final : public webrtc::CustomProcessing {
 public:
  void Initialize(int sample_rate_hz, int /*num_channels*/) override { rate_ = sample_rate_hz; }

  void Process(webrtc::AudioBuffer* audio) override {
    DcmaxNoiseFn fn = g_noise.load(std::memory_order_acquire);
    if (fn == nullptr || audio == nullptr) {
      return;
    }
    const int channels = static_cast<int>(audio->num_channels());
    const int frames = static_cast<int>(audio->num_frames());
    if (channels <= 0 || frames <= 0 || audio->channels() == nullptr) {
      return;
    }
    const int rate = rate_ > 0 ? rate_ : frames * 100;
    fn(audio->channels(), rate, channels, frames);
  }

  std::string ToString() const override { return "dcmax noise suppression"; }

 private:
  int rate_ = 0;
};

}  // namespace

namespace livekit_ffi {

std::unique_ptr<webrtc::CustomProcessing> create_noise_suppression() {
  return std::make_unique<NoiseSuppression>();
}

}  // namespace livekit_ffi

extern "C" void dcmax_set_noise_processor(DcmaxNoiseFn fn) {
  g_noise.store(fn, std::memory_order_release);
}
