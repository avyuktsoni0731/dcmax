use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;

#[derive(Debug, Clone, Copy)]
pub struct CaptureTuning {
    pub target_fps: u32,
    pub probe_seconds: u64,
    pub encoder_backend: EncoderBackend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderBackend {
    Fast,
    FfmpegLibx264,
    FfmpegH264Nvenc,
}

#[cfg_attr(target_os = "windows", allow(dead_code))]
#[derive(Debug, Clone, Copy)]
pub struct CaptureProbeStats {
    pub target_fps: u32,
    pub elapsed_ms: u128,
    pub produced_frames: u64,
    pub achieved_fps: f64,
    pub avg_frame_interval_ms: f64,
}

#[derive(Debug, Clone)]
pub struct CapturedFrame {
    pub width: usize,
    pub height: usize,
    pub bytes: Vec<u8>,
    pub capture_instant: Instant,
}

#[derive(Debug, Clone, Copy)]
pub enum PixelFormat {
    Bgra8,
    #[allow(dead_code)]
    Rgba8,
}

#[derive(Debug, Clone)]
pub struct EncoderInputFrame {
    pub width: usize,
    pub height: usize,
    pub bytes: Vec<u8>,
    #[allow(dead_code)]
    pub pixel_format: PixelFormat,
    pub capture_instant: Instant,
    #[allow(dead_code)]
    pub converted_instant: Instant,
}

#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub width: usize,
    pub height: usize,
    pub payload: Vec<u8>,
    pub capture_instant: Instant,
    pub encoded_instant: Instant,
}

#[derive(Debug, Clone)]
pub struct PipelineReport {
    pub backend: String,
    pub achieved_fps: f64,
    pub produced_frames: u64,
    pub dropped_frames: u64,
    pub avg_ingest_latency_ms: f64,
    pub avg_payload_bytes: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct EncoderAdapterMetrics {
    pub converted_frames: u64,
    pub dropped_frames: u64,
    pub avg_conversion_latency_ms: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct ConverterAcc {
    pub converted_frames: u64,
    pub dropped_frames: u64,
    pub total_conversion_latency_ms: f64,
}

impl ConverterAcc {
    pub fn new() -> Self {
        Self {
            converted_frames: 0,
            dropped_frames: 0,
            total_conversion_latency_ms: 0.0,
        }
    }

    pub fn finalize(self) -> EncoderAdapterMetrics {
        let avg_conversion_latency_ms = if self.converted_frames > 0 {
            self.total_conversion_latency_ms / self.converted_frames as f64
        } else {
            0.0
        };
        EncoderAdapterMetrics {
            converted_frames: self.converted_frames,
            dropped_frames: self.dropped_frames,
            avg_conversion_latency_ms,
        }
    }
}

pub fn adapt_to_encoder_input_bgra(frame: CapturedFrame, acc: &mut ConverterAcc) -> EncoderInputFrame {
    let start = Instant::now();
    // Current capture source already yields BGRA; pass-through copy for explicit stage separation.
    let out = EncoderInputFrame {
        width: frame.width,
        height: frame.height,
        bytes: frame.bytes,
        pixel_format: PixelFormat::Bgra8,
        capture_instant: frame.capture_instant,
        converted_instant: Instant::now(),
    };

    acc.converted_frames += 1;
    acc.total_conversion_latency_ms += start.elapsed().as_secs_f64() * 1000.0;
    out
}

pub fn encode_frame_fast(frame: EncoderInputFrame) -> EncodedFrame {
    // Placeholder encoder stage for pipeline bring-up. Replace with NVENC/AMF/QuickSync later.
    // Downsample bytes to emulate reduced encoded payload volume while preserving timing flow.
    let payload: Vec<u8> = frame.bytes.iter().step_by(16).copied().collect();
    EncodedFrame {
        width: frame.width,
        height: frame.height,
        payload,
        capture_instant: frame.capture_instant,
        encoded_instant: Instant::now(),
    }
}

#[cfg_attr(target_os = "windows", allow(dead_code))]
pub fn run_frame_pacing_probe(tuning: CaptureTuning) -> Result<CaptureProbeStats> {
    let frame_interval = Duration::from_micros(1_000_000 / tuning.target_fps as u64);
    let start = Instant::now();
    let mut next_tick = start;
    let mut frames: u64 = 0;

    while start.elapsed() < Duration::from_secs(tuning.probe_seconds) {
        next_tick += frame_interval;
        let now = Instant::now();
        if next_tick > now {
            thread::sleep(next_tick - now);
        } else {
            // If we are late, resync the scheduler to avoid accumulating lag.
            next_tick = Instant::now();
        }
        frames += 1;
    }

    let elapsed = start.elapsed();
    let elapsed_ms = elapsed.as_millis();
    let elapsed_secs = elapsed.as_secs_f64().max(0.001);
    let achieved_fps = frames as f64 / elapsed_secs;
    let avg_frame_interval_ms = 1000.0 / achieved_fps.max(0.0001);

    Ok(CaptureProbeStats {
        target_fps: tuning.target_fps,
        elapsed_ms,
        produced_frames: frames,
        achieved_fps,
        avg_frame_interval_ms,
    })
}

