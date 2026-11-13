//! Hardware H.264 decode for the direct picture path. VideoToolbox returns a
//! CVPixelBuffer the existing Metal presenter can draw.

use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use screencapturekit::cv::CVPixelBuffer;
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

#[repr(C)]
#[derive(Clone, Copy)]
struct CmTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

#[repr(C)]
struct CallbackRecord {
    callback: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *mut c_void,
            i32,
            u32,
            *mut c_void,
            CmTime,
            CmTime,
        ),
    >,
    refcon: *mut c_void,
}

struct Output {
    image: Mutex<*mut c_void>,
}

pub struct DecodedFrame {
    pub buffer: CVPixelBuffer,
    pub full_range: bool,
}

pub struct Decoder {
    output: Box<Output>,
    session: *mut c_void,
    format: *mut c_void,
    sps: Vec<u8>,
    pps: Vec<u8>,
}

unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new() -> Self {
        Self {
            output: Box::new(Output {
                image: Mutex::new(ptr::null_mut()),
            }),
            session: ptr::null_mut(),
            format: ptr::null_mut(),
            sps: Vec::new(),
            pps: Vec::new(),
        }
    }

    pub fn decode(&mut self, annexb: &[u8]) -> Option<DecodedFrame> {
        let units = nal_units(annexb);
        let mut sps = None;
        let mut pps = None;
        let mut vcl = Vec::new();
        let mut keyframe = false;
        for nal in units {
            let kind = nal.first().copied().unwrap_or(0) & 0x1f;
            match kind {
                7 => sps = Some(nal.to_vec()),
                8 => pps = Some(nal.to_vec()),
                5 => {
                    keyframe = true;
                    vcl.push(nal);
                }
                1..=4 => vcl.push(nal),
                _ => {}
            }
        }
        if let (Some(sps), Some(pps)) = (sps, pps) {
            if sps != self.sps || pps != self.pps {
                self.rebuild(&sps, &pps);
            }
        }
        if self.session.is_null() || vcl.is_empty() || (self.sps.is_empty() && !keyframe) {
            return None;
        }
        let mut sample_bytes = Vec::new();
        for nal in vcl {
            sample_bytes.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            sample_bytes.extend_from_slice(nal);
        }
        let image = unsafe { self.decode_sample(&sample_bytes) }?;
        let buffer = unsafe { CVPixelBuffer::from_raw(image) }?;
        let format = buffer.pixel_format();
        let limited = u32::from_be_bytes(*b"420v");
        let full = u32::from_be_bytes(*b"420f");
        if format != limited && format != full {
            note_once(&format!("watch picture: decoder produced format {format:#x}"));
            return None;
        }
        Some(DecodedFrame {
            buffer,
            full_range: format == full,
        })
    }

    fn rebuild(&mut self, sps: &[u8], pps: &[u8]) {
        self.sps = sps.to_vec();
        self.pps = pps.to_vec();
        self.close_session();
        unsafe {
            let pointers = [sps.as_ptr(), pps.as_ptr()];
            let sizes = [sps.len(), pps.len()];
            let mut format = ptr::null_mut();
            let created = CMVideoFormatDescriptionCreateFromH264ParameterSets(
                ptr::null(),
                2,
                pointers.as_ptr(),
                sizes.as_ptr(),
                4,
                &mut format,
            );
            if created != 0 || format.is_null() {
                note_once(&format!("watch picture: parameter sets were rejected ({created})"));
                return;
            }
            let attrs = image_attributes();
            let mut session = ptr::null_mut();
            let record = CallbackRecord {
                callback: Some(on_decoded),
                refcon: &mut *self.output as *mut Output as *mut c_void,
            };
            let opened = VTDecompressionSessionCreate(
                ptr::null(),
                format,
                ptr::null(),
                attrs.as_concrete_TypeRef() as *const c_void,
                &record,
                &mut session,
            );
            if opened != 0 || session.is_null() {
                CFRelease(format);
                note_once(&format!("watch picture: decoder did not start ({opened})"));
                return;
            }
            self.format = format;
            self.session = session;
        }
    }

    unsafe fn decode_sample(&mut self, bytes: &[u8]) -> Option<*mut c_void> {
        let mut block = ptr::null_mut();
        let allocated = CMBlockBufferCreateWithMemoryBlock(
            ptr::null(),
            ptr::null_mut(),
            bytes.len(),
            ptr::null(),
            ptr::null(),
            0,
            bytes.len(),
            1,
            &mut block,
        );
        if allocated != 0 || block.is_null() {
            note_once(&format!("watch picture: sample buffer was rejected ({allocated})"));
            return None;
        }
        let copied = CMBlockBufferReplaceDataBytes(bytes.as_ptr().cast(), block, 0, bytes.len());
        if copied != 0 {
            CFRelease(block);
            return None;
        }
        let size = bytes.len();
        let mut sample = ptr::null_mut();
        let made = CMSampleBufferCreate(
            ptr::null(),
            block,
            1,
            ptr::null(),
            ptr::null_mut(),
            self.format,
            1,
            0,
            ptr::null(),
            1,
            &size,
            &mut sample,
        );
        if made != 0 || sample.is_null() {
            CFRelease(block);
            note_once(&format!("watch picture: sample was rejected ({made})"));
            return None;
        }
        {
            let mut slot = self.output.image.lock().unwrap_or_else(|err| err.into_inner());
            if !slot.is_null() {
                CFRelease(*slot);
                *slot = ptr::null_mut();
            }
        }
        let mut info = 0u32;
        let status = VTDecompressionSessionDecodeFrame(self.session, sample, 0, ptr::null_mut(), &mut info);
        CFRelease(sample);
        CFRelease(block);
        if status != 0 {
            note_once(&format!("watch picture: decode failed ({status})"));
            return None;
        }
        let mut slot = self.output.image.lock().unwrap_or_else(|err| err.into_inner());
        let image = *slot;
        *slot = ptr::null_mut();
        if image.is_null() {
            None
        } else {
            Some(image)
        }
    }

    fn close_session(&mut self) {
        unsafe {
            if !self.session.is_null() {
                VTDecompressionSessionInvalidate(self.session);
                CFRelease(self.session);
                self.session = ptr::null_mut();
            }
            if !self.format.is_null() {
                CFRelease(self.format);
                self.format = ptr::null_mut();
            }
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.close_session();
        if let Ok(mut slot) = self.output.image.lock() {
            if !slot.is_null() {
                unsafe { CFRelease(*slot) };
                *slot = ptr::null_mut();
            }
        }
    }
}

fn image_attributes() -> CFDictionary<CFString, CFType> {
    let surface = CFDictionary::<CFString, CFType>::from_CFType_pairs(&[]);
    cf_dict(&[
        ("PixelFormatType", CFNumber::from(0x3432_3076_i32).as_CFType()),
        ("IOSurfaceProperties", surface.as_CFType()),
        ("MetalCompatibility", CFBoolean::true_value().as_CFType()),
    ])
}

fn cf_dict(pairs: &[(&str, CFType)]) -> CFDictionary<CFString, CFType> {
    let owned: Vec<(CFString, CFType)> = pairs
        .iter()
        .map(|(key, value)| (CFString::new(key), value.clone()))
        .collect();
    CFDictionary::from_CFType_pairs(&owned)
}

fn nal_units(data: &[u8]) -> Vec<&[u8]> {
    let mut units = Vec::new();
    let mut cursor = 0;
    let mut body = None;
    while cursor + 3 < data.len() {
        let code = if cursor + 4 <= data.len()
            && data[cursor] == 0
            && data[cursor + 1] == 0
            && data[cursor + 2] == 0
            && data[cursor + 3] == 1
        {
            4
        } else if data[cursor] == 0 && data[cursor + 1] == 0 && data[cursor + 2] == 1 {
            3
        } else {
            cursor += 1;
            continue;
        };
        if let Some(start) = body {
            if start < cursor {
                units.push(&data[start..cursor]);
            }
        }
        body = Some(cursor + code);
        cursor += code;
    }
    if let Some(start) = body {
        if start < data.len() {
            units.push(&data[start..]);
        }
    }
    units
}

fn note_once(message: &str) {
    static SEEN: AtomicBool = AtomicBool::new(false);
    if SEEN.swap(true, Ordering::Relaxed) {
        return;
    }
    eprintln!("{message}");
}

unsafe extern "C" fn on_decoded(
    refcon: *mut c_void,
    _source: *mut c_void,
    status: i32,
    _flags: u32,
    image: *mut c_void,
    _pts: CmTime,
    _duration: CmTime,
) {
    if refcon.is_null() {
        return;
    }
    let output = &*(refcon as *const Output);
    let Ok(mut slot) = output.image.lock() else {
        return;
    };
    if !slot.is_null() {
        CFRelease(*slot);
        *slot = ptr::null_mut();
    }
    if status == 0 && !image.is_null() {
        CFRetain(image);
        *slot = image;
    }
}

#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    fn VTDecompressionSessionCreate(
        allocator: *const c_void,
        format: *mut c_void,
        decoder_spec: *const c_void,
        destination: *const c_void,
        callback: *const CallbackRecord,
        session_out: *mut *mut c_void,
    ) -> i32;
    fn VTDecompressionSessionDecodeFrame(
        session: *mut c_void,
        sample: *mut c_void,
        flags: u32,
        source: *mut c_void,
        info_out: *mut u32,
    ) -> i32;
    fn VTDecompressionSessionInvalidate(session: *mut c_void);
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        allocator: *const c_void,
        count: usize,
        pointers: *const *const u8,
        sizes: *const usize,
        nal_header_length: i32,
        format_out: *mut *mut c_void,
    ) -> i32;
    fn CMBlockBufferCreateWithMemoryBlock(
        structure_allocator: *const c_void,
        memory: *mut c_void,
        block_length: usize,
        block_allocator: *const c_void,
        custom: *const c_void,
        offset: usize,
        data_length: usize,
        flags: u32,
        block_out: *mut *mut c_void,
    ) -> i32;
    fn CMBlockBufferReplaceDataBytes(
        source: *const c_void,
        block: *mut c_void,
        offset: usize,
        length: usize,
    ) -> i32;
    fn CMSampleBufferCreate(
        allocator: *const c_void,
        data: *mut c_void,
        data_ready: u8,
        ready_callback: *const c_void,
        ready_refcon: *mut c_void,
        format: *mut c_void,
        sample_count: i64,
        timing_count: i64,
        timing: *const c_void,
        size_count: i64,
        sizes: *const usize,
        sample_out: *mut *mut c_void,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(value: *const c_void);
    fn CFRetain(value: *const c_void) -> *const c_void;
}
