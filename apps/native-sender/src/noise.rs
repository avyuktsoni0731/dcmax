//! Microphone noise suppression. This runs on the capture frames after WebRTC's
//! echo canceller. The model is RNNoise (via nnnoiseless): 10 ms, 48 kHz.
//! WebRTC's own suppressor stays off so the two do not process the same mic.
//!
//! The capture buffer is FloatS16, already in the range RNNoise expects
//! (-32768 to 32767). Scaling it again and writing the result back as a
//! fraction made every sample round to silence.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use nnnoiseless::DenoiseState;

static ENABLED: AtomicBool = AtomicBool::new(true);

struct Cleaner {
    states: Vec<Box<DenoiseState<'static>>>,
    frame: Vec<f32>,
    model_in: Vec<f32>,
    model_out: Vec<f32>,
    logged: bool,
}

impl Cleaner {
    fn new() -> Self {
        Self {
            states: vec![DenoiseState::new()],
            frame: vec![0.0; DenoiseState::FRAME_SIZE],
            model_in: vec![0.0; DenoiseState::FRAME_SIZE],
            model_out: vec![0.0; DenoiseState::FRAME_SIZE],
            logged: false,
        }
    }
}

fn cleaner() -> &'static Mutex<Cleaner> {
    static CLEANER: OnceLock<Mutex<Cleaner>> = OnceLock::new();
    CLEANER.get_or_init(|| Mutex::new(Cleaner::new()))
}

pub fn set_noise_suppression(on: bool) {
    let was = ENABLED.swap(on, Ordering::AcqRel);
    if on && !was {
        if let Ok(mut slot) = cleaner().lock() {
            slot.states.clear();
            slot.states.push(DenoiseState::new());
        }
    }
    if was != on {
        crate::audio::session_log(if on {
            "noise suppression on"
        } else {
            "noise suppression off"
        });
    }
}

pub fn install() {
    unsafe { dcmax_set_noise_processor(Some(denoise_capture)) }
}

extern "C" {
    fn dcmax_set_noise_processor(
        processor: Option<unsafe extern "C" fn(*const *mut f32, i32, i32, i32)>,
    );
}

unsafe extern "C" fn denoise_capture(
    channels: *const *mut f32,
    rate: i32,
    num_channels: i32,
    num_frames: i32,
) {
    if !ENABLED.load(Ordering::Relaxed) || channels.is_null() || num_channels <= 0 || num_frames <= 0 {
        return;
    }
    let frames = num_frames as usize;
    if frames > 4096 {
        return;
    }
    let mut logged_rate = None;
    let Ok(mut slot) = cleaner().lock() else {
        return;
    };
    for index in 0..num_channels {
        let samples = unsafe { *channels.add(index as usize) };
        if samples.is_null() {
            continue;
        }
        let samples = unsafe { std::slice::from_raw_parts_mut(samples, frames) };
        suppress(&mut slot, index as usize, samples);
    }
    if !slot.logged {
        slot.logged = true;
        logged_rate = Some((rate, num_channels));
    }
    drop(slot);
    if let Some((rate, channels)) = logged_rate {
        crate::audio::session_log(&format!(
            "noise suppression running at {rate} Hz, {channels} ch"
        ));
    }
}

fn suppress(cleaner: &mut Cleaner, channel: usize, samples: &mut [f32]) {
    while cleaner.states.len() <= channel {
        cleaner.states.push(DenoiseState::new());
    }
    let Cleaner {
        states,
        frame,
        model_in,
        model_out,
        ..
    } = cleaner;
    let state = &mut states[channel];
    if samples.len() == DenoiseState::FRAME_SIZE {
        denoise_frame(state, model_in, model_out, samples);
        return;
    }
    resample(samples, frame);
    denoise_frame(state, model_in, model_out, frame);
    resample(frame, samples);
}

fn denoise_frame(state: &mut DenoiseState, model_in: &mut [f32], model_out: &mut [f32], frame: &mut [f32]) {
    let width = DenoiseState::FRAME_SIZE.min(frame.len());
    for (dst, src) in model_in.iter_mut().zip(frame.iter().take(width)) {
        *dst = src.clamp(-32768.0, 32767.0);
    }
    state.process_frame(model_out, model_in);
    for (dst, src) in frame.iter_mut().take(width).zip(model_out.iter()) {
        *dst = src.clamp(-32768.0, 32767.0);
    }
}

fn resample(input: &[f32], output: &mut [f32]) {
    if input.is_empty() || output.is_empty() {
        return;
    }
    if input.len() == output.len() {
        output.copy_from_slice(input);
        return;
    }
    let last = input.len() - 1;
    let span = (output.len() - 1).max(1) as f32;
    for (index, sample) in output.iter_mut().enumerate() {
        let position = index as f32 * last as f32 / span;
        let left = position.floor() as usize;
        let frac = position - left as f32;
        let a = input[left.min(last)];
        let b = input[(left + 1).min(last)];
        *sample = a + (b - a) * frac;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_stays_quiet() {
        let mut cleaner = Cleaner::new();
        let mut samples = [0.0f32; DenoiseState::FRAME_SIZE];
        suppress(&mut cleaner, 0, &mut samples);
        assert!(samples.iter().all(|sample| sample.abs() < 1.0));
    }

    #[test]
    fn a_voice_sized_tone_is_not_rounded_away() {
        let mut cleaner = Cleaner::new();
        let mut last = [0.0f32; DenoiseState::FRAME_SIZE];
        for frame in 0..20 {
            for (index, sample) in last.iter_mut().enumerate() {
                let t = (frame * DenoiseState::FRAME_SIZE + index) as f32 / 48_000.0;
                *sample = (t * 180.0 * std::f32::consts::TAU).sin() * 4_000.0;
            }
            suppress(&mut cleaner, 0, &mut last);
        }
        let energy = last.iter().map(|sample| sample * sample).sum::<f32>() / last.len() as f32;
        assert!(energy.sqrt() > 200.0, "rms {energy}");
    }
}
