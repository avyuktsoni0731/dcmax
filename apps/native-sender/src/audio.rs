//! System audio that rides with the picture.
//!
//! The microphone stays on the LiveKit room. This path is the sound of the
//! screen itself: a game, a video, anything the sender is playing. Frames are
//! Opus at 48 kHz stereo, 20 ms each, on the same TCP connection as the picture.

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(target_os = "windows")]
use std::thread::{self, JoinHandle};
use std::time::Instant;
#[cfg(target_os = "windows")]
use std::time::Duration;

const RATE: u32 = 48_000;
const FRAME: usize = 960;
const STEREO: usize = FRAME * 2;

pub fn session_log(line: &str) {
    let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) else {
        return;
    };
    let path = std::path::PathBuf::from(home).join(".mycord.log");
    let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|time| time.as_secs())
        .unwrap_or(0);
    use std::io::Write;
    let _ = writeln!(file, "{secs} {line}");
}

struct OpusEnc {
    raw: *mut opusic_sys::OpusEncoder,
}

unsafe impl Send for OpusEnc {}

impl OpusEnc {
    fn new() -> Result<Self> {
        let mut error = 0;
        let raw = unsafe {
            opusic_sys::opus_encoder_create(
                RATE as i32,
                2,
                opusic_sys::OPUS_APPLICATION_RESTRICTED_LOWDELAY,
                &mut error,
            )
        };
        if raw.is_null() || error != opusic_sys::OPUS_OK {
            anyhow::bail!("opus encoder failed ({error})");
        }
        let status = unsafe {
            opusic_sys::opus_encoder_ctl(raw, opusic_sys::OPUS_SET_BITRATE_REQUEST, 128_000i32)
        };
        if status != opusic_sys::OPUS_OK {
            unsafe { opusic_sys::opus_encoder_destroy(raw) };
            anyhow::bail!("opus bitrate failed ({status})");
        }
        Ok(Self { raw })
    }

    fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>> {
        let mut output = vec![0u8; 4000];
        let written = unsafe {
            opusic_sys::opus_encode(
                self.raw,
                pcm.as_ptr(),
                (pcm.len() / 2) as i32,
                output.as_mut_ptr(),
                output.len() as i32,
            )
        };
        if written <= 0 {
            anyhow::bail!("opus encode failed ({written})");
        }
        output.truncate(written as usize);
        Ok(output)
    }
}

impl Drop for OpusEnc {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { opusic_sys::opus_encoder_destroy(self.raw) };
            self.raw = std::ptr::null_mut();
        }
    }
}

pub struct OpusDec {
    raw: *mut opusic_sys::OpusDecoder,
}

unsafe impl Send for OpusDec {}

impl OpusDec {
    fn new() -> Result<Self> {
        let mut error = 0;
        let raw = unsafe { opusic_sys::opus_decoder_create(RATE as i32, 2, &mut error) };
        if raw.is_null() || error != opusic_sys::OPUS_OK {
            anyhow::bail!("opus decoder failed ({error})");
        }
        Ok(Self { raw })
    }

    fn decode(&mut self, packet: &[u8]) -> Result<Vec<i16>> {
        let mut pcm = vec![0i16; 5760 * 2];
        let frames = unsafe {
            opusic_sys::opus_decode(
                self.raw,
                packet.as_ptr(),
                packet.len() as i32,
                pcm.as_mut_ptr(),
                5760,
                0,
            )
        };
        if frames <= 0 {
            anyhow::bail!("opus decode failed ({frames})");
        }
        pcm.truncate((frames as usize) * 2);
        Ok(pcm)
    }
}

impl Drop for OpusDec {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { opusic_sys::opus_decoder_destroy(self.raw) };
            self.raw = std::ptr::null_mut();
        }
    }
}

pub struct SystemAudio {
    encoder: OpusEnc,
    pending: Vec<i16>,
    phase: f64,
    prev: Option<[f32; 2]>,
    packet_us: i64,
}

impl SystemAudio {
    pub fn new() -> Result<Self> {
        Ok(Self {
            encoder: OpusEnc::new()?,
            pending: Vec::with_capacity(STEREO * 2),
            phase: 0.0,
            prev: None,
            packet_us: 0,
        })
    }

    /// Interleaved PCM in, zero or more Opus packets out.
    ///
    /// `first_sample_us` is the sender clock time of the first new sample.
    /// Each packet is 20 ms and gets its own timestamp, so the viewer can
    /// play it against the picture instead of whenever the packet arrives.
    pub fn push(
        &mut self,
        interleaved: &[f32],
        channels: usize,
        sample_rate: u32,
        first_sample_us: i64,
    ) -> Vec<(i64, Vec<u8>)> {
        if self.pending.is_empty() {
            self.packet_us = first_sample_us;
        }
        self.collect(interleaved, channels, sample_rate);
        let mut packets = Vec::new();
        while self.pending.len() >= STEREO {
            let frame: Vec<i16> = self.pending.drain(..STEREO).collect();
            match self.encoder.encode(&frame) {
                Ok(packet) => {
                    packets.push((self.packet_us, packet));
                    self.packet_us = self.packet_us.saturating_add(20_000);
                }
                Err(err) => {
                    eprintln!("system audio: opus encode failed: {err}");
                    break;
                }
            }
        }
        packets
    }

    fn collect(&mut self, interleaved: &[f32], channels: usize, sample_rate: u32) {
        if channels == 0 || sample_rate == 0 || interleaved.len() < channels {
            return;
        }
        let frames = interleaved.len() / channels;
        if sample_rate == RATE {
            for frame in 0..frames {
                let base = frame * channels;
                let left = interleaved[base];
                let right = if channels > 1 {
                    interleaved[base + 1]
                } else {
                    left
                };
                self.pending.push(float_to_i16(left));
                self.pending.push(float_to_i16(right));
            }
            return;
        }
        let mut stereo = Vec::with_capacity(frames + 1);
        if let Some(prev) = self.prev {
            stereo.push(prev);
        }
        for frame in 0..frames {
            let base = frame * channels;
            let left = interleaved[base];
            let right = if channels > 1 {
                interleaved[base + 1]
            } else {
                left
            };
            stereo.push([left, right]);
        }
        if stereo.len() < 2 {
            self.prev = stereo.last().copied().or(self.prev);
            return;
        }
        let step = f64::from(sample_rate) / f64::from(RATE);
        let last = stereo.len() - 1;
        let mut pos = self.phase;
        while pos < last as f64 {
            let index = pos as usize;
            let frac = (pos - index as f64) as f32;
            let left = stereo[index][0] + (stereo[index + 1][0] - stereo[index][0]) * frac;
            let right = stereo[index][1] + (stereo[index + 1][1] - stereo[index][1]) * frac;
            self.pending.push(float_to_i16(left));
            self.pending.push(float_to_i16(right));
            pos += step;
        }
        self.phase = (pos - last as f64).max(0.0);
        self.prev = Some(stereo[last]);
    }
}

pub fn decode_packet(decoder: &mut OpusDec, packet: &[u8]) -> Result<Vec<i16>> {
    decoder.decode(packet)
}

pub fn decoder() -> Result<OpusDec> {
    OpusDec::new()
}

fn float_to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * 32767.0) as i16
}

/// Sender-clock playout. A picture frame and the sound captured with it
/// share one timestamp, and this clock releases both 70 ms after the first
/// frame so a small arrival gap does not put them on different timelines.
pub struct PlayClock {
    state: Mutex<ClockState>,
}

struct ClockState {
    origin_remote: i64,
    origin_local: Instant,
    primed: bool,
}

const TARGET_US: i64 = 70_000;
const MAX_AHEAD_US: i64 = 140_000;

impl PlayClock {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ClockState {
                origin_remote: 0,
                origin_local: Instant::now(),
                primed: false,
            }),
        })
    }

    pub fn note_video(&self, timestamp_us: i64) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let now = Instant::now();
        if !state.primed {
            state.origin_remote = timestamp_us.saturating_sub(TARGET_US);
            state.origin_local = now;
            state.primed = true;
            return;
        }
        let playout = state.origin_remote + state.origin_local.elapsed().as_micros() as i64;
        let ahead = timestamp_us.saturating_sub(playout);
        if ahead > MAX_AHEAD_US {
            state.origin_remote = timestamp_us.saturating_sub(TARGET_US);
            state.origin_local = now;
        }
    }

    pub fn now_us(&self) -> Option<i64> {
        let state = self.state.lock().ok()?;
        if !state.primed {
            return None;
        }
        Some(state.origin_remote + state.origin_local.elapsed().as_micros() as i64)
    }

    pub fn reset(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.primed = false;
        }
    }
}

struct AudioChunk {
    timestamp_us: i64,
    samples: Vec<f32>,
    cursor: usize,
}

struct OutputQueue {
    chunks: VecDeque<AudioChunk>,
    timed: bool,
}

/// Plays 48 kHz stereo packets on the default output. `mute` replaces the
/// sound with silence and drops anything already queued.
pub struct AudioPlayer {
    queue: Arc<Mutex<OutputQueue>>,
    mute: Arc<AtomicBool>,
    _stream: Stream,
}

impl AudioPlayer {
    pub fn start(mute: Arc<AtomicBool>) -> Result<Self> {
        Self::open(mute, None)
    }

    /// Play each packet when the shared picture clock reaches its timestamp.
    pub fn start_synced(mute: Arc<AtomicBool>, clock: Arc<PlayClock>) -> Result<Self> {
        Self::open(mute, Some(clock))
    }

    fn open(mute: Arc<AtomicBool>, clock: Option<Arc<PlayClock>>) -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .context("this Mac has no audio output")?;
        let wanted = StreamConfig {
            channels: 2,
            sample_rate: cpal::SampleRate(RATE),
            buffer_size: cpal::BufferSize::Default,
        };
        let queue = Arc::new(Mutex::new(OutputQueue {
            chunks: VecDeque::new(),
            timed: clock.is_some(),
        }));
        let stream = match device.default_output_config() {
            Ok(config) if config.sample_format() == SampleFormat::F32 => {
                let fallback = clock.clone();
                build_f32(
                    &device,
                    &wanted,
                    Arc::clone(&queue),
                    Arc::clone(&mute),
                    clock.clone(),
                )
                .or_else(|_| {
                    build_f32(&device, &config.into(), Arc::clone(&queue), Arc::clone(&mute), fallback)
                })?
            }
            Ok(config) => {
                anyhow::bail!(
                    "audio output format {} is unsupported",
                    config.sample_format()
                )
            }
            Err(err) => anyhow::bail!("audio output: {err}"),
        };
        stream.play().context("failed to start audio playback")?;
        println!("watch audio: opus 48 kHz");
        Ok(Self {
            queue,
            mute,
            _stream: stream,
        })
    }

    pub fn push(&self, pcm: &[i16]) {
        self.enqueue(pcm, 0);
    }

    pub fn push_at(&self, pcm: &[i16], timestamp_us: i64) {
        self.enqueue(pcm, timestamp_us);
    }

    fn enqueue(&self, pcm: &[i16], timestamp_us: i64) {
        if self.mute.load(Ordering::Relaxed) {
            if let Ok(mut queue) = self.queue.lock() {
                queue.chunks.clear();
            }
            return;
        }
        let mut queue = match self.queue.lock() {
            Ok(queue) => queue,
            Err(_) => return,
        };
        let mut samples = Vec::with_capacity(pcm.len());
        for sample in pcm {
            samples.push(f32::from(*sample) / 32768.0);
        }
        queue.chunks.push_back(AudioChunk {
            timestamp_us,
            samples,
            cursor: 0,
        });
        let mut duration = 0i64;
        for chunk in &queue.chunks {
            duration += frames_to_us(((chunk.samples.len() - chunk.cursor) / 2) as i64);
        }
        while duration > 200_000 && queue.chunks.len() > 1 {
            if let Some(dropped) = queue.chunks.pop_front() {
                duration -= frames_to_us(((dropped.samples.len() - dropped.cursor) / 2) as i64);
            }
        }
    }
}

fn frames_to_us(frames: i64) -> i64 {
    frames.saturating_mul(1_000_000) / i64::from(RATE)
}

fn build_f32(
    device: &cpal::Device,
    config: &StreamConfig,
    queue: Arc<Mutex<OutputQueue>>,
    mute: Arc<AtomicBool>,
    clock: Option<Arc<PlayClock>>,
) -> Result<Stream> {
    let channels = config.channels as usize;
    let stream = device
        .build_output_stream(
            config,
            move |output: &mut [f32], _| {
                if mute.load(Ordering::Relaxed) {
                    output.fill(0.0);
                    if let Ok(mut queue) = queue.lock() {
                        queue.chunks.clear();
                    }
                    return;
                }
                let mut queue = match queue.lock() {
                    Ok(queue) => queue,
                    Err(_) => {
                        output.fill(0.0);
                        return;
                    }
                };
                let playout = if queue.timed {
                    clock.as_ref().and_then(|clock| clock.now_us())
                } else {
                    Some(i64::MAX)
                };
                let Some(playout) = playout else {
                    output.fill(0.0);
                    return;
                };
                for frame in output.chunks_mut(channels.max(1)) {
                    let (left, right) = take_sample(&mut queue, playout);
                    if let Some(slot) = frame.first_mut() {
                        *slot = left;
                    }
                    if frame.len() > 1 {
                        frame[1] = right;
                    }
                    for extra in frame.iter_mut().skip(2) {
                        *extra = 0.0;
                    }
                }
            },
            |err| eprintln!("watch audio: {err}"),
            None,
        )
        .map_err(|err| anyhow::anyhow!("audio output stream: {err}"))?;
    Ok(stream)
}

fn take_sample(queue: &mut OutputQueue, playout: i64) -> (f32, f32) {
    if !queue.timed {
        let Some(chunk) = queue.chunks.front_mut() else {
            return (0.0, 0.0);
        };
        let left = chunk.samples.get(chunk.cursor).copied().unwrap_or(0.0);
        let right = chunk.samples.get(chunk.cursor + 1).copied().unwrap_or(left);
        chunk.cursor += 2;
        if chunk.cursor >= chunk.samples.len() {
            queue.chunks.pop_front();
        }
        return (left, right);
    }
    while let Some(chunk) = queue.chunks.front() {
        let frames_left = (chunk.samples.len().saturating_sub(chunk.cursor)) / 2;
        if frames_left == 0 {
            queue.chunks.pop_front();
            continue;
        }
        let head_us = chunk.timestamp_us + frames_to_us((chunk.cursor / 2) as i64);
        let end_us = head_us + frames_to_us(frames_left as i64);
        if end_us + 5_000 < playout.saturating_sub(30_000) {
            queue.chunks.pop_front();
            continue;
        }
        if head_us > playout || frames_left == 0 {
            return (0.0, 0.0);
        }
        break;
    }
    let Some(chunk) = queue.chunks.front_mut() else {
        return (0.0, 0.0);
    };
    let left = chunk.samples.get(chunk.cursor).copied().unwrap_or(0.0);
    let right = chunk.samples.get(chunk.cursor + 1).copied().unwrap_or(left);
    chunk.cursor += 2;
    if chunk.cursor >= chunk.samples.len() {
        queue.chunks.pop_front();
    }
    (left, right)
}

/// Reads PCM out of a Windows render endpoint in loopback, so the picture
/// carries the desktop sound rather than the microphone.
#[cfg(target_os = "windows")]
pub struct DesktopAudio {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

#[cfg(target_os = "windows")]
impl DesktopAudio {
    pub fn start(tx: tokio::sync::mpsc::Sender<EncodedAudio>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("system-audio".to_string())
            .spawn(move || {
                if let Err(err) = capture_loopback(tx.clone(), &stop_thread) {
                    session_log(&format!("system audio: {err:#}"));
                }
                while !stop_thread.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(200));
                }
                drop(tx);
            })
            .ok();
        Self { stop, thread }
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(target_os = "windows")]
impl Drop for DesktopAudio {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Debug)]
pub struct EncodedAudio {
    pub payload: Vec<u8>,
    pub timestamp_us: i64,
}

#[cfg(target_os = "windows")]
fn capture_loopback(
    tx: tokio::sync::mpsc::Sender<EncodedAudio>,
    stop: &AtomicBool,
) -> Result<()> {
    use windows::core::Interface;
    use windows::Win32::Media::Audio::{
        eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator,
        AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_S_BUFFER_EMPTY,
        MMDeviceEnumerator,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED,
    };

    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .context("audio device enumerator")?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .context("default speakers")?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None).context("audio client")?;
        let mix = client.GetMixFormat().context("mix format")?;
        let format = &*mix;
        let rate = format.nSamplesPerSec;
        let channels = format.nChannels as usize;
        let bits = format.wBitsPerSample;
        let block = format.nBlockAlign as usize;
        let mut float = is_float(format);
        if !float && bits == 32 {
            // The shared mix format is 32-bit float. The extensible GUID sits at
            // byte 24; reading it with the padded Rust struct size used to miss
            // it and throw every sample away.
            float = true;
        }
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
                200_000,
                0,
                mix,
                None,
            )
            .context("loopback initialize")?;
        CoTaskMemFree(Some(mix.cast()));
        let capture: IAudioCaptureClient = client.GetService().context("capture client")?;
        client.Start().context("loopback start")?;
        session_log(&format!(
            "system audio: wasapi loopback {rate} Hz {channels} ch {bits}-bit float={float}"
        ));
        let mut codec = SystemAudio::new()?;
        let mut scratch = Vec::new();
        while !stop.load(Ordering::Acquire) {
            let mut data = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            match capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None) {
                Ok(()) if frames > 0 && !data.is_null() => {
                    let silent = flags & 0x2 != 0;
                    let bytes = frames as usize * block.max(1);
                    let slice = std::slice::from_raw_parts(data, bytes);
                    scratch.clear();
                    if silent {
                        scratch.resize(frames as usize * channels.max(1), 0.0);
                    } else if float && bits == 32 {
                        for chunk in slice.chunks_exact(4) {
                            scratch.push(f32::from_le_bytes(chunk.try_into().unwrap()));
                        }
                    } else if !float && bits == 16 {
                        for chunk in slice.chunks_exact(2) {
                            let sample = i16::from_le_bytes(chunk.try_into().unwrap());
                            scratch.push(f32::from(sample) / 32768.0);
                        }
                    }
                    let _ = capture.ReleaseBuffer(frames);
                    // The buffer ends as we read it. Stamp the first sample
                    // back by the buffer's own length so it lines up with the
                    // frame captured at the same moment.
                    let now = crate::h264::unix_us_now();
                    let duration_us = (frames as i64).saturating_mul(1_000_000) / rate.max(1) as i64;
                    let start_us = now.saturating_sub(duration_us);
                    for (timestamp_us, payload) in codec.push(&scratch, channels, rate, start_us) {
                        if tx
                            .try_send(EncodedAudio {
                                payload,
                                timestamp_us,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }
                Ok(()) => {
                    let _ = capture.ReleaseBuffer(frames);
                    thread::sleep(Duration::from_millis(5));
                }
                Err(err) if err.code() == AUDCLNT_S_BUFFER_EMPTY => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(err) => {
                    client.Stop().ok();
                    anyhow::bail!("loopback read: {err}");
                }
            }
        }
        client.Stop().ok();
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn is_float(format: &windows::Win32::Media::Audio::WAVEFORMATEX) -> bool {
    const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
    const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
    if format.wFormatTag == WAVE_FORMAT_IEEE_FLOAT {
        return true;
    }
    if format.wFormatTag != WAVE_FORMAT_EXTENSIBLE as u16 || format.cbSize < 22 {
        return false;
    }
    // WAVEFORMATEXTENSIBLE: samples (2) + channel mask (4), then the GUID.
    // Data1 of a float subtype is 3, at byte 24. sizeof(WAVEFORMATEX) in Rust
    // is padded, so adding it lands past the GUID.
    let header = format as *const windows::Win32::Media::Audio::WAVEFORMATEX as *const u8;
    let data1 = unsafe { std::ptr::read_unaligned(header.add(24) as *const u32) };
    data1 == u32::from(WAVE_FORMAT_IEEE_FLOAT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opus_roundtrip_keeps_a_tone() {
        let mut audio = SystemAudio::new().unwrap();
        let mut samples = vec![0f32; 960 * 2];
        for frame in 0..960 {
            let tone = (frame as f32 / 48_000.0 * 440.0 * std::f32::consts::TAU).sin() * 0.4;
            samples[frame * 2] = tone;
            samples[frame * 2 + 1] = tone;
        }
        let packets = audio.push(&samples, 2, 48_000, 1_000_000);
        assert_eq!(packets.len(), 1, "20 ms of audio is one Opus packet");
        assert_eq!(packets[0].0, 1_000_000);
        let mut decoder = decoder().unwrap();
        let pcm = decode_packet(&mut decoder, &packets[0].1).unwrap();
        let energy: f64 = pcm.iter().map(|sample| f64::from(*sample).powi(2)).sum();
        assert!(energy > 1_000_000.0, "decoded tone was silent ({energy})");
    }
}
