use crate::h264::{
    length_prefixed_contains_idr, length_prefixed_to_annexb, monotonic_unix_us, AccessUnit,
};
use anyhow::{bail, Result};
use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[repr(C)]
#[derive(Clone, Copy)]
struct CmTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

const TIME_VALID: u32 = 1;
const TIME_INDEFINITE: CmTime = CmTime {
    value: 0,
    timescale: 0,
    flags: TIME_VALID | (1 << 4),
    epoch: 0,
};
const TIME_INVALID: CmTime = CmTime {
    value: 0,
    timescale: 0,
    flags: 0,
    epoch: 0,
};
const H264: u32 = u32::from_be_bytes(*b"avc1");

#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    fn VTCompressionSessionCreate(
        allocator: *const c_void,
        width: i32,
        height: i32,
        codec_type: u32,
        encoder_specification: *const c_void,
        source_image_buffer_attributes: *const c_void,
        compressed_data_allocator: *const c_void,
        output_callback: Option<
            unsafe extern "C" fn(*mut c_void, *mut c_void, i32, u32, *mut c_void),
        >,
        output_callback_refcon: *mut c_void,
        compression_session_out: *mut *mut c_void,
    ) -> i32;
    fn VTCompressionSessionEncodeFrame(
        session: *mut c_void,
        image_buffer: *mut c_void,
        presentation_time_stamp: CmTime,
        duration: CmTime,
        frame_properties: *const c_void,
        source_frame_refcon: *mut c_void,
        info_flags_out: *mut u32,
    ) -> i32;
    fn VTCompressionSessionCompleteFrames(session: *mut c_void, complete_until: CmTime) -> i32;
    fn VTCompressionSessionInvalidate(session: *mut c_void);
    fn VTCompressionSessionPrepareToEncodeFrames(session: *mut c_void) -> i32;
    fn VTSessionSetProperty(session: *mut c_void, key: *const c_void, value: *const c_void) -> i32;
    fn VTSessionCopyProperty(
        session: *mut c_void,
        key: *const c_void,
        allocator: *const c_void,
        value_out: *mut *const c_void,
    ) -> i32;
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetDataBuffer(sample: *mut c_void) -> *mut c_void;
    fn CMSampleBufferGetFormatDescription(sample: *mut c_void) -> *mut c_void;
    fn CMSampleBufferGetPresentationTimeStamp(sample: *mut c_void) -> CmTime;
    fn CMSampleBufferGetSampleAttachmentsArray(sample: *mut c_void, create: u8) -> *const c_void;
    fn CMBlockBufferGetDataLength(block: *mut c_void) -> usize;
    fn CMBlockBufferCopyDataBytes(
        block: *mut c_void,
        offset: usize,
        length: usize,
        destination: *mut c_void,
    ) -> i32;
    fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        format: *mut c_void,
        index: usize,
        parameter_set_out: *mut *const u8,
        parameter_set_size_out: *mut usize,
        parameter_set_count_out: *mut usize,
        nal_unit_header_length_out: *mut i32,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(value: *const c_void);
    fn CFArrayGetCount(array: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
    fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
    fn CFBooleanGetValue(value: *const c_void) -> u8;
}

struct OutputState {
    tx: mpsc::Sender<AccessUnit>,
    dropped: Arc<AtomicU64>,
    force_keyframe: Arc<AtomicBool>,
    last_livekit_us: AtomicI64,
}

struct FrameMarker {
    captured_at: Instant,
    livekit_us: i64,
    ticket: u64,
    flight: Arc<Flight>,
}

/// One encode at a time. The ticket lets a late callback finish only the frame
/// it actually encoded, so a stalled encoder cannot unblock the next one early.
pub struct Flight {
    inflight: AtomicU64,
    tickets: AtomicU64,
    lock: Mutex<()>,
    done: Condvar,
}

pub struct VtSession {
    raw: *mut c_void,
    output: Option<Box<OutputState>>,
    alive: bool,
    applied_bitrate: u64,
    hardware_logged: bool,
}

unsafe impl Send for VtSession {}

impl Flight {
    pub fn new() -> Self {
        Self {
            inflight: AtomicU64::new(0),
            tickets: AtomicU64::new(0),
            lock: Mutex::new(()),
            done: Condvar::new(),
        }
    }

    pub fn begin(&self) -> u64 {
        let ticket = self.tickets.fetch_add(1, Ordering::Relaxed) + 1;
        self.inflight.store(ticket, Ordering::Release);
        ticket
    }

    pub fn finish(&self, ticket: u64) {
        if self
            .inflight
            .compare_exchange(ticket, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.done.notify_one();
        }
    }

    pub fn wake(&self) {
        self.done.notify_one();
    }

    /// Waits until the encode callback has finished the current frame.
    /// A callback that never arrives is abandoned after two seconds so capture
    /// can move on to a fresh frame.
    pub fn wait_until_idle(&self, stop: &AtomicBool) -> bool {
        let mut guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        let started = Instant::now();
        while self.inflight.load(Ordering::Acquire) != 0 {
            if stop.load(Ordering::Acquire) {
                return false;
            }
            if started.elapsed() >= Duration::from_secs(2) {
                let ticket = self.inflight.load(Ordering::Acquire);
                drop(guard);
                self.finish(ticket);
                return !stop.load(Ordering::Acquire);
            }
            let (next, _) = self
                .done
                .wait_timeout(guard, Duration::from_millis(50))
                .unwrap_or_else(|err| err.into_inner());
            guard = next;
        }
        !stop.load(Ordering::Acquire)
    }
}

impl VtSession {
    pub fn create(
        width: u32,
        height: u32,
        fps: u32,
        bitrate_bps: u64,
        gop: u32,
        tx: mpsc::Sender<AccessUnit>,
        dropped: Arc<AtomicU64>,
        force_keyframe: Arc<AtomicBool>,
    ) -> Result<Self> {
        let output = Box::new(OutputState {
            tx,
            dropped,
            force_keyframe,
            last_livekit_us: AtomicI64::new(0),
        });
        let refcon = Box::into_raw(output);
        let spec = cf_dict(&[(
            "EnableHardwareAcceleratedVideoEncoder",
            CFBoolean::true_value().as_CFType(),
        )]);
        let mut raw = std::ptr::null_mut();
        let status = unsafe {
            VTCompressionSessionCreate(
                std::ptr::null(),
                width as i32,
                height as i32,
                H264,
                spec.as_concrete_TypeRef() as *const c_void,
                std::ptr::null(),
                std::ptr::null(),
                Some(on_encoded_frame),
                refcon.cast(),
                &mut raw,
            )
        };
        if status != 0 || raw.is_null() {
            drop(unsafe { Box::from_raw(refcon) });
            bail!("VideoToolbox failed to create an H.264 encoder ({status})");
        }
        let mut session = Self {
            raw,
            output: Some(unsafe { Box::from_raw(refcon) }),
            alive: true,
            applied_bitrate: bitrate_bps,
            hardware_logged: false,
        };
        session.configure(fps, bitrate_bps, gop)?;
        let prepare = unsafe { VTCompressionSessionPrepareToEncodeFrames(raw) };
        if prepare != 0 {
            session.finish();
            bail!("VideoToolbox failed to prepare the encoder ({prepare})");
        }
        Ok(session)
    }

    pub fn encode(
        &mut self,
        pixel_buffer: *mut c_void,
        timestamp_us: i64,
        livekit_us: i64,
        force_keyframe: bool,
        bitrate_bps: u64,
        captured_at: Instant,
        flight: &Arc<Flight>,
        ticket: u64,
    ) -> Result<()> {
        if !self.alive || pixel_buffer.is_null() {
            flight.finish(ticket);
            return Ok(());
        }
        if bitrate_bps != 0 && bitrate_bps != self.applied_bitrate {
            self.apply_bitrate(bitrate_bps);
            self.applied_bitrate = bitrate_bps;
        }
        let properties = if force_keyframe {
            Some(cf_dict(&[(
                "ForceKeyFrame",
                CFBoolean::true_value().as_CFType(),
            )]))
        } else {
            None
        };
        let properties_ref = properties
            .as_ref()
            .map(|dict| dict.as_concrete_TypeRef() as *const c_void)
            .unwrap_or(std::ptr::null());
        let marker = Box::into_raw(Box::new(FrameMarker {
            captured_at,
            livekit_us,
            ticket,
            flight: Arc::clone(flight),
        }));
        let status = unsafe {
            VTCompressionSessionEncodeFrame(
                self.raw,
                pixel_buffer,
                us_time(timestamp_us),
                TIME_INVALID,
                properties_ref,
                marker.cast(),
                std::ptr::null_mut(),
            )
        };
        if status != 0 {
            drop(unsafe { Box::from_raw(marker) });
            flight.finish(ticket);
            bail!("VideoToolbox encode failed ({status})");
        }
        self.log_hardware_once();
        Ok(())
    }

    pub fn finish(&mut self) {
        if self.raw.is_null() {
            return;
        }
        self.alive = false;
        unsafe {
            VTCompressionSessionCompleteFrames(self.raw, TIME_INDEFINITE);
            VTCompressionSessionInvalidate(self.raw);
            CFRelease(self.raw);
        }
        self.raw = std::ptr::null_mut();
        self.output.take();
    }

    fn configure(&mut self, fps: u32, bitrate_bps: u64, gop: u32) -> Result<()> {
        self.set_bool("RealTime", true);
        self.set_bool("AllowFrameReordering", false);
        self.set_bool("PrioritizeEncodingSpeedOverQuality", true);
        self.set_i32("MaxKeyFrameInterval", gop as i32);
        self.set_f64("MaxKeyFrameIntervalDuration", 2.0);
        self.set_f64("ExpectedFrameRate", f64::from(fps));
        self.set_string("ProfileLevel", "H264_High_AutoLevel");
        self.set_string("H264EntropyMode", "CABAC");
        self.apply_bitrate(bitrate_bps);
        Ok(())
    }

    fn apply_bitrate(&mut self, bitrate_bps: u64) {
        self.set_i32("AverageBitRate", bitrate_bps.min(i32::MAX as u64) as i32);
        // A half-second cap makes VideoToolbox delay or crush a keyframe to stay
        // inside the window. One second is the same interval the rate governor uses.
        let window_seconds = 1.0_f64;
        let bytes = ((bitrate_bps as f64 / 8.0) * window_seconds)
            .round()
            .max(1.0) as i64;
        let limits = CFArray::from_CFTypes(&[
            CFNumber::from(bytes).as_CFType(),
            CFNumber::from(window_seconds).as_CFType(),
        ]);
        self.set_cf("DataRateLimits", &limits);
    }

    fn log_hardware_once(&mut self) {
        if self.hardware_logged {
            return;
        }
        self.hardware_logged = true;
        let key = CFString::new("UsingHardwareAcceleratedVideoEncoder");
        let mut value = std::ptr::null();
        let status = unsafe {
            VTSessionCopyProperty(
                self.raw,
                key.as_concrete_TypeRef() as *const c_void,
                std::ptr::null(),
                &mut value,
            )
        };
        let hardware = status == 0 && !value.is_null() && unsafe { CFBooleanGetValue(value) } != 0;
        if !value.is_null() {
            unsafe { CFRelease(value) };
        }
        println!(
            "videotoolbox hardware encoder: {}",
            if hardware { "yes" } else { "no" }
        );
    }

    fn set_bool(&mut self, key: &str, value: bool) {
        let boolean = if value {
            CFBoolean::true_value()
        } else {
            CFBoolean::false_value()
        };
        self.set_cf(key, &boolean);
    }

    fn set_i32(&mut self, key: &str, value: i32) {
        self.set_cf(key, &CFNumber::from(value));
    }

    fn set_f64(&mut self, key: &str, value: f64) {
        self.set_cf(key, &CFNumber::from(value));
    }

    fn set_string(&mut self, key: &str, value: &str) {
        self.set_cf(key, &CFString::new(value));
    }

    fn set_cf(&mut self, key: &str, value: &impl TCFType) {
        if !self.alive || self.raw.is_null() {
            return;
        }
        let name = CFString::new(key);
        let status = unsafe {
            VTSessionSetProperty(
                self.raw,
                name.as_concrete_TypeRef() as *const c_void,
                value.as_CFTypeRef(),
            )
        };
        if status != 0 {
            eprintln!("videotoolbox ignored {key} ({status})");
        }
    }
}

impl Drop for VtSession {
    fn drop(&mut self) {
        self.finish();
    }
}

fn cf_dict(pairs: &[(&str, CFType)]) -> CFDictionary<CFString, CFType> {
    let owned: Vec<(CFString, CFType)> = pairs
        .iter()
        .map(|(key, value)| (CFString::new(key), value.clone()))
        .collect();
    CFDictionary::from_CFType_pairs(&owned)
}

fn us_time(timestamp_us: i64) -> CmTime {
    CmTime {
        value: timestamp_us,
        timescale: 1_000_000,
        flags: TIME_VALID,
        epoch: 0,
    }
}

unsafe extern "C" fn on_encoded_frame(
    refcon: *mut c_void,
    source: *mut c_void,
    status: i32,
    _flags: u32,
    sample: *mut c_void,
) {
    let (captured_at, livekit_us) = if source.is_null() {
        (Instant::now(), 0)
    } else {
        let marker = unsafe { Box::from_raw(source.cast::<FrameMarker>()) };
        marker.flight.finish(marker.ticket);
        (marker.captured_at, marker.livekit_us)
    };
    if refcon.is_null() || status != 0 || sample.is_null() {
        return;
    }
    let Some(mut unit) = access_unit_from_sample(sample) else {
        return;
    };
    unit.captured_at = captured_at;
    let state = unsafe { &*refcon.cast::<OutputState>() };
    if livekit_us > 0 {
        let mut last = state.last_livekit_us.load(Ordering::Relaxed);
        let stamped = monotonic_unix_us(&mut last, livekit_us);
        state.last_livekit_us.store(stamped, Ordering::Relaxed);
        unit.timestamp_us = Some(stamped);
    }
    // Blocking here stalls the hardware encoder, and the encode thread then
    // produces nothing until the publish side catches up. A dropped delta
    // breaks the reference chain, so the next encode is a keyframe.
    if let Err(tokio::sync::mpsc::error::TrySendError::Full(_)) = state.tx.try_send(unit) {
        state.dropped.fetch_add(1, Ordering::Relaxed);
        state.force_keyframe.store(true, Ordering::Release);
    }
}

fn access_unit_from_sample(sample: *mut c_void) -> Option<AccessUnit> {
    let block = unsafe { CMSampleBufferGetDataBuffer(sample) };
    if block.is_null() {
        return None;
    }
    let length = unsafe { CMBlockBufferGetDataLength(block) };
    if length == 0 {
        return None;
    }
    let mut payload = vec![0u8; length];
    let copied = unsafe {
        CMBlockBufferCopyDataBytes(block, 0, length, payload.as_mut_ptr().cast::<c_void>())
    };
    if copied != 0 {
        return None;
    }
    let format = unsafe { CMSampleBufferGetFormatDescription(sample) };
    let (sets, nal_length_size) = parameter_sets(format);
    let keyframe =
        is_sync_sample(sample) || length_prefixed_contains_idr(&payload, nal_length_size);
    let annexb = length_prefixed_to_annexb(&payload, nal_length_size, &sets, keyframe);
    if annexb.is_empty() {
        return None;
    }
    let timestamp = unsafe { CMSampleBufferGetPresentationTimeStamp(sample) };
    let timestamp_us = if timestamp.flags & TIME_VALID != 0 && timestamp.timescale > 0 {
        Some(
            i64::try_from(timestamp.value as i128 * 1_000_000 / i128::from(timestamp.timescale))
                .unwrap_or(timestamp.value),
        )
    } else {
        None
    };
    Some(AccessUnit {
        payload: annexb,
        keyframe,
        read_at: Instant::now(),
        captured_at: Instant::now(),
        timestamp_us,
    })
}

fn parameter_sets(format: *mut c_void) -> (Vec<Vec<u8>>, usize) {
    if format.is_null() {
        return (Vec::new(), 4);
    }
    let mut sets = Vec::new();
    let mut nal_length_size = 4usize;
    for index in 0..8 {
        let mut pointer: *const u8 = std::ptr::null();
        let mut size = 0usize;
        let mut count = 0usize;
        let mut header_len = 0i32;
        let status = unsafe {
            CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                format,
                index,
                &mut pointer,
                &mut size,
                &mut count,
                &mut header_len,
            )
        };
        if status != 0 || pointer.is_null() || size == 0 {
            break;
        }
        if (1..=4).contains(&header_len) {
            nal_length_size = header_len as usize;
        }
        sets.push(unsafe { std::slice::from_raw_parts(pointer, size) }.to_vec());
        if count > 0 && index + 1 >= count {
            break;
        }
    }
    (sets, nal_length_size)
}

fn is_sync_sample(sample: *mut c_void) -> bool {
    let attachments = unsafe { CMSampleBufferGetSampleAttachmentsArray(sample, 0) };
    if attachments.is_null() || unsafe { CFArrayGetCount(attachments) } <= 0 {
        return true;
    }
    let dict = unsafe { CFArrayGetValueAtIndex(attachments, 0) };
    if dict.is_null() {
        return true;
    }
    let key = not_sync_key();
    let value = unsafe { CFDictionaryGetValue(dict, key.as_CFTypeRef()) };
    value.is_null() || unsafe { CFBooleanGetValue(value) } == 0
}

fn not_sync_key() -> CFString {
    CFString::new("NotSync")
}
