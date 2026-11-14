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
#[cfg(target_os = "windows")]
use std::time::Duration;

const RATE: u32 = 48_000;
const FRAME: usize = 960;
const STEREO: usize = FRAME * 2;

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
}

impl SystemAudio {
    pub fn new() -> Result<Self> {
        Ok(Self {
            encoder: OpusEnc::new()?,
            pending: Vec::with_capacity(STEREO * 2),
            phase: 0.0,
            prev: None,
        })
    }

    /// Interleaved PCM in, zero or more Opus packets out.
    pub fn push(&mut self, interleaved: &[f32], channels: usize, sample_rate: u32) -> Vec<Vec<u8>> {
        self.collect(interleaved, channels, sample_rate);
        let mut packets = Vec::new();
        while self.pending.len() >= STEREO {
            let frame: Vec<i16> = self.pending.drain(..STEREO).collect();
            match self.encoder.encode(&frame) {
                Ok(packet) => packets.push(packet),
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

/// Plays 48 kHz stereo packets on the default output. `mute` replaces the
/// sound with silence and drops anything already queued.
pub struct AudioPlayer {
    queue: Arc<Mutex<VecDeque<f32>>>,
    mute: Arc<AtomicBool>,
    _stream: Stream,
}

impl AudioPlayer {
    pub fn start(mute: Arc<AtomicBool>) -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .context("this Mac has no audio output")?;
        let wanted = StreamConfig {
            channels: 2,
            sample_rate: cpal::SampleRate(RATE),
            buffer_size: cpal::BufferSize::Default,
        };
        let queue = Arc::new(Mutex::new(VecDeque::<f32>::new()));
        let stream = match device.default_output_config() {
            Ok(config) if config.sample_format() == SampleFormat::F32 => {
                build_f32(&device, &wanted, Arc::clone(&queue), Arc::clone(&mute))
                    .or_else(|_| build_f32(&device, &config.into(), Arc::clone(&queue), Arc::clone(&mute)))?
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
        if self.mute.load(Ordering::Relaxed) {
            if let Ok(mut queue) = self.queue.lock() {
                queue.clear();
            }
            return;
        }
        let mut queue = match self.queue.lock() {
            Ok(queue) => queue,
            Err(_) => return,
        };
        for sample in pcm {
            queue.push_back(f32::from(*sample) / 32768.0);
        }
        let cap = RATE as usize * 2;
        while queue.len() > cap {
            queue.pop_front();
        }
    }
}

fn build_f32(
    device: &cpal::Device,
    config: &StreamConfig,
    queue: Arc<Mutex<VecDeque<f32>>>,
    mute: Arc<AtomicBool>,
) -> Result<Stream> {
    let channels = config.channels as usize;
    let stream = device
        .build_output_stream(
            config,
            move |output: &mut [f32], _| {
                if mute.load(Ordering::Relaxed) {
                    output.fill(0.0);
                    if let Ok(mut queue) = queue.lock() {
                        queue.clear();
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
                for frame in output.chunks_mut(channels.max(1)) {
                    let left = queue.pop_front().unwrap_or(0.0);
                    let right = queue.pop_front().unwrap_or(left);
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
                    eprintln!("system audio: {err:#}");
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
        let float = is_float(format);
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
        println!("system audio: wasapi loopback {rate} Hz {channels} ch {bits}-bit");
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
                    for packet in codec.push(&scratch, channels, rate) {
                        let timestamp_us = crate::h264::unix_us_now();
                        if tx
                            .try_send(EncodedAudio {
                                payload: packet,
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
    // SubFormat GUID Data1 sits 8 bytes after the WAVEFORMATEX (Samples + mask).
    let header = format as *const windows::Win32::Media::Audio::WAVEFORMATEX as *const u8;
    let data1 = unsafe { std::ptr::read_unaligned(header.add(std::mem::size_of::<windows::Win32::Media::Audio::WAVEFORMATEX>() + 8) as *const u32) };
    data1 == WAVE_FORMAT_IEEE_FLOAT as u32
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
        let packets = audio.push(&samples, 2, 48_000);
        assert_eq!(packets.len(), 1, "20 ms of audio is one Opus packet");
        let mut decoder = decoder().unwrap();
        let pcm = decode_packet(&mut decoder, &packets[0]).unwrap();
        let energy: f64 = pcm.iter().map(|sample| f64::from(*sample).powi(2)).sum();
        assert!(energy > 1_000_000.0, "decoded tone was silent ({energy})");
    }
}
