use anyhow::{Context, Result};
use scrap::{Capturer, Display};
use std::io::ErrorKind;
use std::sync::mpsc::{self, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

use super::CaptureBackend;
use crate::capture::{
    adapt_to_encoder_input_bgra, encode_frame_fast, CaptureTuning, CapturedFrame, ConverterAcc,
    EncodedFrame,
};
use crate::platform::windows_dxgi::probe_primary_adapter;

pub struct WindowsCaptureBackend;

#[derive(Debug, Clone, Copy)]
struct DesktopCaptureProbeStats {
    width: usize,
    height: usize,
    produced_frames: u64,
    would_block_events: u64,
    poll_attempts: u64,
    elapsed_ms: u128,
    achieved_fps: f64,
    avg_frame_bytes: usize,
    pipeline_sent_frames: u64,
    pipeline_dropped_frames: u64,
    pipeline_consumed_frames: u64,
    pipeline_consumed_bytes: usize,
    pipeline_avg_ingest_latency_ms: f64,
    encoder_converted_frames: u64,
    encoder_dropped_frames: u64,
    encoder_avg_conversion_latency_ms: f64,
    encoder_avg_end_to_end_latency_ms: f64,
    publisher_avg_latency_ms: f64,
    publisher_avg_payload_bytes: usize,
}

fn run_windows_desktop_capture_probe(tuning: CaptureTuning) -> Result<DesktopCaptureProbeStats> {
    let display = Display::primary().context("failed to resolve primary display")?;
    let width = display.width();
    let height = display.height();

    let start = Instant::now();
    let deadline = Duration::from_secs(tuning.probe_seconds);
    let (tx, rx) = mpsc::sync_channel::<CapturedFrame>(32);
    let (tx_enc_in, rx_enc_in) = mpsc::sync_channel::<crate::capture::EncoderInputFrame>(32);
    let (tx_pub, rx_pub) = mpsc::sync_channel::<EncodedFrame>(32);

    let adapter_worker = thread::spawn(move || {
        let mut converter_acc = ConverterAcc::new();
        let mut sent: u64 = 0;
        let mut dropped: u64 = 0;
        while let Ok(frame) = rx.recv() {
            let adapted = adapt_to_encoder_input_bgra(frame, &mut converter_acc);
            match tx_enc_in.try_send(adapted) {
                Ok(()) => sent += 1,
                Err(TrySendError::Full(_)) => dropped += 1,
                Err(TrySendError::Disconnected(_)) => {
                    dropped += 1;
                    break;
                }
            }
        }
        (converter_acc.finalize(), sent, dropped)
    });

    let encoder_worker = thread::spawn(move || {
        let mut encoded_frames: u64 = 0;
        let mut dropped: u64 = 0;
        let mut total_encode_latency_ms: f64 = 0.0;
        while let Ok(frame) = rx_enc_in.recv() {
            let encoded = encode_frame_fast(frame);
            total_encode_latency_ms += encoded.encoded_instant.elapsed().as_secs_f64() * 1000.0;
            match tx_pub.try_send(encoded) {
                Ok(()) => encoded_frames += 1,
                Err(TrySendError::Full(_)) => dropped += 1,
                Err(TrySendError::Disconnected(_)) => {
                    dropped += 1;
                    break;
                }
            }
        }
        let avg_encode_latency_ms = if encoded_frames > 0 {
            total_encode_latency_ms / encoded_frames as f64
        } else {
            0.0
        };
        (encoded_frames, dropped, avg_encode_latency_ms)
    });

    let publisher_worker = thread::spawn(move || {
        let mut consumed_frames: u64 = 0;
        let mut consumed_bytes: usize = 0;
        let mut total_latency_ms: f64 = 0.0;
        let mut total_payload_bytes: usize = 0;
        while let Ok(frame) = rx_pub.recv() {
            consumed_frames += 1;
            consumed_bytes += frame.width * frame.height * 4;
            total_payload_bytes += frame.payload.len();
            total_latency_ms += frame.capture_instant.elapsed().as_secs_f64() * 1000.0;
        }
        let avg_latency_ms = if consumed_frames > 0 {
            total_latency_ms / consumed_frames as f64
        } else {
            0.0
        };
        let avg_payload_bytes = if consumed_frames > 0 {
            total_payload_bytes / consumed_frames as usize
        } else {
            0
        };
        (consumed_frames, consumed_bytes, avg_latency_ms, avg_payload_bytes)
    });
    let mut capturer = Capturer::new(display).context("failed to initialize desktop capturer")?;
    let mut frames: u64 = 0;
    let mut would_block_events: u64 = 0;
    let mut poll_attempts: u64 = 0;
    let mut total_bytes: usize = 0;
    let mut pipeline_sent_frames: u64 = 0;
    let mut pipeline_dropped_frames: u64 = 0;
    let frame_interval = Duration::from_micros(1_000_000 / tuning.target_fps as u64);

    while start.elapsed() < deadline {
        poll_attempts += 1;
        match capturer.frame() {
            Ok(frame) => {
                frames += 1;
                total_bytes += frame.len();
                let captured = CapturedFrame {
                    width,
                    height,
                    bytes: frame.to_vec(),
                    capture_instant: Instant::now(),
                };
                match tx.try_send(captured) {
                    Ok(()) => {
                        pipeline_sent_frames += 1;
                    }
                    Err(TrySendError::Full(_)) => {
                        pipeline_dropped_frames += 1;
                    }
                    Err(TrySendError::Disconnected(_)) => {
                        pipeline_dropped_frames += 1;
                    }
                }
                // Target pacing after successful frame capture.
                thread::sleep(frame_interval);
            }
            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                would_block_events += 1;
                thread::sleep(Duration::from_millis(1));
            }
            Err(err) => {
                return Err(err).context("desktop frame capture failed");
            }
        }
    }

    let elapsed = start.elapsed();
    let elapsed_ms = elapsed.as_millis();
    let elapsed_secs = elapsed.as_secs_f64().max(0.001);
    let achieved_fps = frames as f64 / elapsed_secs;
    let avg_frame_bytes = if frames > 0 {
        total_bytes / frames as usize
    } else {
        0
    };
    drop(tx);
    let (converter_metrics, _adapter_sent, adapter_dropped) = adapter_worker
        .join()
        .map_err(|_| anyhow::anyhow!("adapter worker panicked"))?;
    let (_encoder_stage_converted_frames, encoder_dropped_frames, _encoder_internal_latency_ms) =
        encoder_worker
            .join()
            .map_err(|_| anyhow::anyhow!("encoder worker panicked"))?;
    let (
        pipeline_consumed_frames,
        pipeline_consumed_bytes,
        pipeline_avg_ingest_latency_ms,
        publisher_avg_payload_bytes,
    ) = publisher_worker
        .join()
        .map_err(|_| anyhow::anyhow!("publisher worker panicked"))?;
    let pipeline_avg_end_to_end_latency_ms = pipeline_avg_ingest_latency_ms;

    Ok(DesktopCaptureProbeStats {
        width,
        height,
        produced_frames: frames,
        would_block_events,
        poll_attempts,
        elapsed_ms,
        achieved_fps,
        avg_frame_bytes,
        pipeline_sent_frames,
        pipeline_dropped_frames,
        pipeline_consumed_frames,
        pipeline_consumed_bytes,
        pipeline_avg_ingest_latency_ms,
        encoder_converted_frames: converter_metrics.converted_frames,
        encoder_dropped_frames: converter_metrics.dropped_frames + adapter_dropped + encoder_dropped_frames,
        encoder_avg_conversion_latency_ms: converter_metrics.avg_conversion_latency_ms,
        encoder_avg_end_to_end_latency_ms: pipeline_avg_end_to_end_latency_ms,
        publisher_avg_latency_ms: pipeline_avg_end_to_end_latency_ms,
        publisher_avg_payload_bytes,
    })
}

impl CaptureBackend for WindowsCaptureBackend {
    fn name(&self) -> &'static str {
        "windows"
    }

    fn bootstrap_capture_pipeline(&self, dry_run: bool, tuning: CaptureTuning) -> Result<()> {
        let adapter_probe = probe_primary_adapter()?;
        println!(
            "[windows] dxgi adapter: name='{}' vendor_id={} vram={}MB shared={}MB",
            adapter_probe.adapter_name,
            adapter_probe.vendor_id,
            adapter_probe.dedicated_video_memory_mb,
            adapter_probe.shared_system_memory_mb
        );

        if dry_run {
            println!("[windows] dry-run capture bootstrap: DXGI + WASAPI pipeline placeholder");
            return Ok(());
        }

        println!(
            "[windows] running desktop capture probe at {} fps for {}s",
            tuning.target_fps, tuning.probe_seconds
        );
        let stats = run_windows_desktop_capture_probe(tuning)?;
        println!(
            "[windows] probe done: frames={} elapsed={}ms achieved_fps={:.2} resolution={}x{} avg_frame_bytes={} polls={} would_block={} pipeline_sent={} pipeline_dropped={} pipeline_consumed={} pipeline_consumed_bytes={} pipeline_avg_ingest_latency_ms={:.2} encoder_converted={} encoder_dropped={} encoder_avg_conversion_latency_ms={:.3} encoder_avg_end_to_end_latency_ms={:.2} publisher_avg_latency_ms={:.2} publisher_avg_payload_bytes={}",
            stats.produced_frames,
            stats.elapsed_ms,
            stats.achieved_fps,
            stats.width,
            stats.height,
            stats.avg_frame_bytes,
            stats.poll_attempts,
            stats.would_block_events,
            stats.pipeline_sent_frames,
            stats.pipeline_dropped_frames,
            stats.pipeline_consumed_frames,
            stats.pipeline_consumed_bytes,
            stats.pipeline_avg_ingest_latency_ms,
            stats.encoder_converted_frames,
            stats.encoder_dropped_frames,
            stats.encoder_avg_conversion_latency_ms,
            stats.encoder_avg_end_to_end_latency_ms,
            stats.publisher_avg_latency_ms,
            stats.publisher_avg_payload_bytes
        );
        if stats.achieved_fps < (tuning.target_fps as f64 * 0.5) {
            println!(
                "[windows] note: low new-frame rate can happen if the desktop is mostly static. Move windows/video during probe to measure true active-motion fps."
            );
        }
        println!("[windows] next milestone: route captured frames into encoder/publisher pipeline");
        Ok(())
    }

    fn diagnostics_hint(&self) -> &'static str {
        "Use latest GPU drivers, keep display refresh >= 60Hz, and disable Windows power saver for stable capture pacing."
    }
}

