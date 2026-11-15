//! Direct-picture rate. The number chosen in the app is the ceiling.
//! When frames are produced and do not leave, the encoder steps down so a
//! smaller picture keeps moving. It steps back up only after the pipe stays clear.

const FLOOR_BPS: u64 = 1_500_000;
const WARMUP_SECONDS: u32 = 2;
const CLEAR_BEFORE_CLIMB: u32 = 5;
const MIN_FPS: u32 = 15;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pace {
    pub bps: u64,
    pub fps: u32,
    pub changed: bool,
    pub reason: &'static str,
}

pub struct PictureAdapt {
    ceiling_bps: u64,
    floor_bps: u64,
    applied_bps: u64,
    target_fps: u32,
    applied_fps: u32,
    seen: u32,
    clear_seconds: u32,
    tight_seconds: u32,
}

impl PictureAdapt {
    pub fn new(bitrate_kbps: u32, target_fps: u32) -> Self {
        let ceiling_bps = u64::from(bitrate_kbps.max(1))
            .saturating_mul(1000)
            .clamp(800_000, 100_000_000);
        let target_fps = target_fps.clamp(MIN_FPS, 240);
        Self {
            ceiling_bps,
            floor_bps: FLOOR_BPS.min(ceiling_bps),
            applied_bps: ceiling_bps,
            target_fps,
            applied_fps: target_fps,
            seen: 0,
            clear_seconds: 0,
            tight_seconds: 0,
        }
    }

    pub fn pace(&self) -> Pace {
        Pace {
            bps: self.applied_bps,
            fps: self.applied_fps,
            changed: false,
            reason: "holding",
        }
    }

    /// One observation is about one second of the picture pipe.
    /// `encoded` is frames the encoder produced, `sent` is frames written
    /// toward the watcher, and `skipped` is frames the send queue refused.
    pub fn observe(&mut self, encoded: u64, sent: u64, skipped: u64) -> Pace {
        let previous_bps = self.applied_bps;
        let previous_fps = self.applied_fps;
        self.seen = self.seen.saturating_add(1);
        if self.seen <= WARMUP_SECONDS {
            return self.finish(previous_bps, previous_fps, "holding");
        }
        if encoded == 0 && sent == 0 && skipped == 0 {
            return self.finish(previous_bps, previous_fps, "holding");
        }
        let produced = encoded.max(sent.saturating_add(skipped));
        let congested = skipped >= 8 || (produced >= 20 && sent.saturating_mul(2) < produced);
        let clear = skipped == 0 && sent > 0 && sent.saturating_mul(2) >= produced;
        if congested {
            self.clear_seconds = 0;
            self.tight_seconds = self.tight_seconds.saturating_add(1);
            if self.applied_bps > self.floor_bps {
                let eased = self.applied_bps.saturating_mul(2) / 3;
                let eased = eased.max(self.floor_bps);
                self.applied_bps = eased;
                return self.finish(previous_bps, previous_fps, "easing");
            }
            if self.tight_seconds >= 2 && self.applied_fps > MIN_FPS {
                self.applied_fps = (self.applied_fps / 2).max(MIN_FPS);
                self.tight_seconds = 0;
                return self.finish(previous_bps, previous_fps, "slowing");
            }
            return self.finish(previous_bps, previous_fps, "holding");
        }
        self.tight_seconds = 0;
        if clear {
            self.clear_seconds = self.clear_seconds.saturating_add(1);
            if self.clear_seconds >= CLEAR_BEFORE_CLIMB {
                self.clear_seconds = 0;
                if self.applied_fps < self.target_fps {
                    self.applied_fps = self.applied_fps.saturating_mul(2).min(self.target_fps);
                    return self.finish(previous_bps, previous_fps, "climbing");
                }
                if self.applied_bps < self.ceiling_bps {
                    let step = (self.applied_bps / 6).max(1_000_000);
                    self.applied_bps = self.applied_bps.saturating_add(step).min(self.ceiling_bps);
                    return self.finish(previous_bps, previous_fps, "climbing");
                }
            }
        } else {
            self.clear_seconds = 0;
        }
        self.finish(previous_bps, previous_fps, "holding")
    }

    fn finish(&self, previous_bps: u64, previous_fps: u32, reason: &'static str) -> Pace {
        let changed = self.applied_bps != previous_bps || self.applied_fps != previous_fps;
        Pace {
            bps: self.applied_bps,
            fps: self.applied_fps,
            changed,
            reason: if changed { reason } else { "holding" },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PictureAdapt;

    #[test]
    fn the_opening_seconds_keep_the_chosen_cap() {
        let mut adapt = PictureAdapt::new(20_000, 60);
        for _ in 0..2 {
            let pace = adapt.observe(57, 0, 57);
            assert_eq!(pace.bps, 20_000_000);
            assert!(!pace.changed);
        }
    }

    #[test]
    fn a_full_queue_cuts_the_bitrate_before_the_frame_rate() {
        let mut adapt = PictureAdapt::new(20_000, 60);
        adapt.observe(57, 57, 0);
        adapt.observe(57, 57, 0);
        let first = adapt.observe(57, 0, 57);
        assert_eq!(first.reason, "easing");
        assert_eq!(first.bps, 13_333_333);
        assert_eq!(first.fps, 60);
        let second = adapt.observe(57, 0, 57);
        assert_eq!(second.bps, 8_888_888);
        assert_eq!(second.fps, 60);
    }

    #[test]
    fn the_floor_holds_and_then_the_frame_rate_drops() {
        let mut adapt = PictureAdapt::new(20_000, 60);
        adapt.observe(0, 0, 0);
        adapt.observe(0, 0, 0);
        let mut pace = adapt.pace();
        for _ in 0..12 {
            pace = adapt.observe(57, 0, 57);
        }
        assert_eq!(pace.bps, 1_500_000);
        assert!(pace.fps < 60, "{}", pace.fps);
        let mut slowed = pace;
        for _ in 0..4 {
            slowed = adapt.observe(30, 0, 30);
            if slowed.fps == 15 {
                break;
            }
        }
        assert_eq!(slowed.fps, 15);
        assert_eq!(slowed.bps, 1_500_000);
        let held = adapt.observe(15, 0, 15);
        assert_eq!(held.fps, 15);
        assert_eq!(held.reason, "holding");
    }

    #[test]
    fn a_clear_pipe_climbs_the_frame_rate_before_the_bitrate() {
        let mut adapt = PictureAdapt::new(20_000, 60);
        adapt.observe(0, 0, 0);
        adapt.observe(0, 0, 0);
        for _ in 0..12 {
            adapt.observe(57, 0, 57);
        }
        assert!(adapt.pace().fps < 60);
        let low = adapt.pace();
        let mut restored = low;
        for _ in 0..6 {
            restored = adapt.observe(low.fps as u64, low.fps as u64, 0);
            if restored.fps > low.fps {
                break;
            }
        }
        assert!(restored.fps > low.fps, "{}", restored.fps);
        assert_eq!(restored.bps, low.bps);
        assert_ne!(restored.reason, "easing");
    }

    #[test]
    fn a_clear_pipe_does_not_jump_back_to_the_ceiling() {
        let mut adapt = PictureAdapt::new(20_000, 60);
        adapt.observe(57, 57, 0);
        adapt.observe(57, 57, 0);
        let eased = adapt.observe(57, 0, 57);
        let mut climbed = eased;
        for _ in 0..5 {
            climbed = adapt.observe(57, 57, 0);
        }
        assert!(climbed.bps > eased.bps, "{}", climbed.bps);
        assert!(climbed.bps < 20_000_000, "{}", climbed.bps);
    }

    #[test]
    fn a_few_skipped_frames_do_not_cut_a_healthy_second() {
        let mut adapt = PictureAdapt::new(20_000, 60);
        adapt.observe(57, 57, 0);
        adapt.observe(57, 57, 0);
        let pace = adapt.observe(57, 54, 3);
        assert_eq!(pace.bps, 20_000_000);
        assert_eq!(pace.reason, "holding");
    }
}
