//! Picture rate. The number chosen in the app is what we try to send.
//!
//! The direct pipe steps down when frames are produced and do not leave, then
//! steps back up only after the pipe stays clear.
//!
//! The screen track keeps that chosen frame rate. LiveKit's estimate is a hint
//! about the path. A low hint does not become the encode rate: a static screen
//! makes the estimate fall, and copying it is what leaves the moving picture
//! soft. The bitrate steps down only while the receiver is losing the chain,
//! and it steps back toward the chosen number once those requests stop.

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

    /// The number chosen in the app. A lower choice clamps immediately.
    /// A higher choice stays the ceiling and the existing climb brings the
    /// encoder back up once the pipe is clear.
    pub fn set_ceiling(&mut self, bitrate_kbps: u32) {
        let ceiling = u64::from(bitrate_kbps.max(1))
            .saturating_mul(1000)
            .clamp(800_000, 100_000_000);
        self.ceiling_bps = ceiling;
        self.floor_bps = FLOOR_BPS.min(ceiling);
        if self.applied_bps > ceiling {
            self.applied_bps = ceiling;
        }
    }

    pub fn set_target_fps(&mut self, fps: u32) {
        self.target_fps = fps.clamp(MIN_FPS, 240);
        if self.applied_fps > self.target_fps {
            self.applied_fps = self.target_fps;
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

const ROOM_FLOOR_BPS: u64 = 800_000;
const ROOM_WARMUP_SECONDS: u32 = 2;
const ROOM_CLIMB_WAIT: u32 = 2;

/// Screen-track aim. The chosen bitrate and frame rate are what we send.
/// LiveKit's estimate is remembered and never applied on its own.
pub struct RoomAim {
    ceiling_bps: u64,
    applied_bps: u64,
    target_fps: u32,
    estimate_bps: Option<u64>,
    seen: u32,
    loss_seconds: u32,
    clean_seconds: u32,
    /// Clean seconds required before probing above a hint that is already below us.
    probe_wait: u32,
}

impl RoomAim {
    pub fn new(bitrate_kbps: u32, target_fps: u32) -> Self {
        let ceiling = clamp_room_bps(u64::from(bitrate_kbps.max(1)).saturating_mul(1000));
        Self {
            ceiling_bps: ceiling,
            applied_bps: ceiling,
            target_fps: target_fps.clamp(MIN_FPS, 240),
            estimate_bps: None,
            seen: 0,
            loss_seconds: 0,
            clean_seconds: 0,
            probe_wait: ROOM_CLIMB_WAIT,
        }
    }

    pub fn applied_bps(&self) -> u64 {
        self.applied_bps
    }

    pub fn fps(&self) -> u32 {
        self.target_fps
    }

    pub fn hint_bps(&self) -> Option<u64> {
        self.estimate_bps
    }

    pub fn hint_note(&self) -> String {
        match self.estimate_bps {
            Some(bps) => format!(" hint {}kbps", bps / 1000),
            None => String::new(),
        }
    }

    /// A lower choice clamps immediately. A higher choice becomes the rate
    /// only when we were already holding the previous choice. A rate that
    /// loss has already stepped down keeps climbing instead of jumping.
    pub fn set_ceiling(&mut self, bitrate_kbps: u32) -> bool {
        let ceiling = clamp_room_bps(u64::from(bitrate_kbps.max(1)).saturating_mul(1000));
        let was_holding = self.applied_bps >= self.ceiling_bps;
        let previous = self.applied_bps;
        self.ceiling_bps = ceiling;
        if self.estimate_bps.is_some_and(|hint| hint > ceiling) {
            self.estimate_bps = Some(ceiling);
        }
        if self.applied_bps > ceiling {
            self.applied_bps = ceiling;
        } else if was_holding && ceiling > self.applied_bps {
            self.applied_bps = ceiling;
            self.clean_seconds = 0;
        }
        self.applied_bps != previous
    }

    pub fn set_target_fps(&mut self, fps: u32) {
        self.target_fps = fps.clamp(MIN_FPS, 240);
    }

    pub fn note_estimate(&mut self, bps: u64) {
        let ceiling = self.ceiling_bps.max(ROOM_FLOOR_BPS);
        self.estimate_bps = Some(bps.clamp(ROOM_FLOOR_BPS, ceiling));
    }

    /// One observation is about one second. `keyframe_requests` counts how
    /// often the receiver lost the chain during that second.
    pub fn decide(&mut self, keyframe_requests: u32) -> Pace {
        let previous_bps = self.applied_bps;
        self.seen = self.seen.saturating_add(1);
        if self.seen <= ROOM_WARMUP_SECONDS {
            return self.finish(previous_bps, "holding");
        }
        let losing = keyframe_requests >= 2
            || (keyframe_requests >= 1 && self.loss_seconds > 0);
        if keyframe_requests == 0 {
            self.loss_seconds = 0;
        } else {
            self.loss_seconds = self.loss_seconds.saturating_add(1);
        }
        if losing {
            self.clean_seconds = 0;
            let (numer, denom) = if keyframe_requests >= 4 { (2, 3) } else { (4, 5) };
            self.applied_bps = (self.applied_bps.saturating_mul(numer) / denom).max(ROOM_FLOOR_BPS);
            self.applied_bps = self.applied_bps.min(self.ceiling_bps);
            if self
                .estimate_bps
                .is_some_and(|hint| hint.saturating_add(500_000) < previous_bps)
            {
                self.probe_wait = (self.probe_wait + 2).min(12);
            }
            return self.finish(previous_bps, "easing");
        }
        self.clean_seconds = self.clean_seconds.saturating_add(1);
        if let Some(hint) = self.estimate_bps {
            if hint > self.applied_bps.saturating_add(500_000) {
                self.applied_bps = climb_toward(self.applied_bps, hint.min(self.ceiling_bps));
                self.clean_seconds = 0;
                return self.finish(previous_bps, "climbing");
            }
        }
        if self.applied_bps >= self.ceiling_bps {
            return self.finish(previous_bps, "holding");
        }
        let hint_below = self
            .estimate_bps
            .is_some_and(|hint| hint.saturating_add(500_000) < self.applied_bps);
        let wait = if hint_below { self.probe_wait } else { ROOM_CLIMB_WAIT };
        if self.clean_seconds >= wait {
            self.clean_seconds = 0;
            self.applied_bps = probe_toward(self.applied_bps, self.ceiling_bps);
            if !hint_below {
                self.probe_wait = ROOM_CLIMB_WAIT;
            }
            return self.finish(previous_bps, "climbing");
        }
        self.finish(previous_bps, "holding")
    }

    fn finish(&self, previous_bps: u64, reason: &'static str) -> Pace {
        let changed = self.applied_bps != previous_bps;
        Pace {
            bps: self.applied_bps,
            fps: self.target_fps,
            changed,
            reason: if changed { reason } else { "holding" },
        }
    }
}

fn clamp_room_bps(bps: u64) -> u64 {
    bps.clamp(ROOM_FLOOR_BPS, 100_000_000)
}

fn climb_toward(applied: u64, target: u64) -> u64 {
    if applied >= target {
        return target;
    }
    let gap = target - applied;
    let step = (gap * 2 / 3).max(2_000_000);
    applied.saturating_add(step).min(target)
}

fn probe_toward(applied: u64, ceiling: u64) -> u64 {
    if applied >= ceiling {
        return ceiling;
    }
    let gap = ceiling - applied;
    let step = (gap / 4).max(2_000_000);
    applied.saturating_add(step).min(ceiling)
}

#[cfg(test)]
mod tests {
    use super::PictureAdapt;
    use super::RoomAim;

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

    #[test]
    fn a_low_link_hint_does_not_replace_the_chosen_rate() {
        let mut aim = RoomAim::new(20_000, 60);
        aim.note_estimate(4_000_000);
        for _ in 0..8 {
            let pace = aim.decide(0);
            assert_eq!(pace.bps, 20_000_000);
            assert_eq!(pace.fps, 60);
        }
    }

    #[test]
    fn the_opening_seconds_keep_the_target_while_the_watcher_attaches() {
        let mut aim = RoomAim::new(12_000, 60);
        aim.note_estimate(2_000_000);
        for _ in 0..2 {
            let pace = aim.decide(6);
            assert_eq!(pace.bps, 12_000_000);
            assert_eq!(pace.fps, 60);
            assert!(!pace.changed);
        }
    }

    #[test]
    fn repeated_lost_chains_ease_the_bitrate_and_keep_the_frame_rate() {
        let mut aim = RoomAim::new(20_000, 60);
        aim.decide(0);
        aim.decide(0);
        let mild = aim.decide(2);
        assert_eq!(mild.reason, "easing");
        assert_eq!(mild.bps, 16_000_000);
        assert_eq!(mild.fps, 60);
        let heavy = aim.decide(4);
        assert_eq!(heavy.reason, "easing");
        assert_eq!(heavy.bps, 10_666_666);
        assert_eq!(heavy.fps, 60);
    }

    #[test]
    fn one_keyframe_request_does_not_cut_and_a_second_second_does() {
        let mut aim = RoomAim::new(20_000, 60);
        aim.decide(0);
        aim.decide(0);
        let once = aim.decide(1);
        assert_eq!(once.bps, 20_000_000);
        let again = aim.decide(1);
        assert_eq!(again.reason, "easing");
        assert_eq!(again.bps, 16_000_000);
    }

    #[test]
    fn loss_can_reach_the_floor_without_giving_up_the_frame_rate() {
        let mut aim = RoomAim::new(20_000, 60);
        aim.decide(0);
        aim.decide(0);
        let mut pace = aim.decide(4);
        for _ in 0..20 {
            pace = aim.decide(4);
        }
        assert_eq!(pace.bps, 800_000);
        assert_eq!(pace.fps, 60);
        let held = aim.decide(4);
        assert_eq!(held.bps, 800_000);
        assert_eq!(held.reason, "holding");
    }

    #[test]
    fn a_clear_second_does_not_jump_back_to_the_target() {
        let mut aim = RoomAim::new(20_000, 60);
        aim.decide(0);
        aim.decide(0);
        let eased = aim.decide(4);
        let mut climbed = eased;
        for _ in 0..3 {
            climbed = aim.decide(0);
        }
        assert!(climbed.bps > eased.bps, "{}", climbed.bps);
        assert!(climbed.bps < 20_000_000, "{}", climbed.bps);
        assert_eq!(climbed.fps, 60);
    }

    #[test]
    fn a_hint_above_the_rate_we_are_sending_is_taken() {
        let mut aim = RoomAim::new(20_000, 60);
        aim.decide(0);
        aim.decide(0);
        let eased = aim.decide(4);
        aim.note_estimate(20_000_000);
        let climbed = aim.decide(0);
        assert_eq!(climbed.reason, "climbing");
        assert!(climbed.bps > eased.bps, "{}", climbed.bps);
        assert!(climbed.bps < 20_000_000, "{}", climbed.bps);
    }

    #[test]
    fn raising_the_choice_while_holding_it_aims_at_the_new_number() {
        let mut aim = RoomAim::new(12_000, 60);
        assert!(aim.set_ceiling(20_000));
        assert_eq!(aim.applied_bps(), 20_000_000);
        aim.set_target_fps(60);
        assert_eq!(aim.fps(), 60);
    }

    #[test]
    fn raising_the_choice_after_a_loss_does_not_jump() {
        let mut aim = RoomAim::new(12_000, 60);
        aim.decide(0);
        aim.decide(0);
        let eased = aim.decide(4);
        assert!(!aim.set_ceiling(20_000));
        assert_eq!(aim.applied_bps(), eased.bps);
    }

    #[test]
    fn a_lower_choice_clamps_immediately() {
        let mut aim = RoomAim::new(20_000, 60);
        assert!(aim.set_ceiling(8_000));
        assert_eq!(aim.applied_bps(), 8_000_000);
    }
}
