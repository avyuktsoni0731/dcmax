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

/// Packed RGBA for the app window. VideoToolbox decode is fast; a pixel-by-pixel
/// copy was not, and the picture fell further behind every frame.
pub fn rgba(buffer: &CVPixelBuffer, full_range: bool) -> Option<(u32, u32, Vec<u8>)> {
    let width = u32::try_from(buffer.width()).ok()?;
    let height = u32::try_from(buffer.height()).ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    let guard = buffer.lock_read_only().ok()?;
    let y = plane(&guard, 0, height as usize)?;
    let uv = plane(&guard, 1, ((height as usize) + 1) / 2)?;
    let stride_y = u32::try_from(guard.bytes_per_row_of_plane(0)).ok()?;
    let stride_uv = u32::try_from(guard.bytes_per_row_of_plane(1)).ok()?;
    let rgba = nv12_to_rgba(y, stride_y, uv, stride_uv, width, height, full_range)?;
    Some((width, height, rgba))
}

fn nv12_to_rgba(
    y: &[u8],
    stride_y: u32,
    uv: &[u8],
    stride_uv: u32,
    width: u32,
    height: u32,
    full_range: bool,
) -> Option<Vec<u8>> {
    if width % 2 != 0 || height % 2 != 0 {
        return Some(rgba_scalar(y, stride_y, uv, stride_uv, width, height, full_range));
    }
    let mut rgba = vec![0u8; (width as usize) * (height as usize) * 4];
    let info = conversion(full_range)?;
    let src_y = VImageBuffer {
        data: y.as_ptr() as *mut u8,
        height: height as usize,
        width: width as usize,
        row_bytes: stride_y as usize,
    };
    let src_uv = VImageBuffer {
        data: uv.as_ptr() as *mut u8,
        height: (height / 2) as usize,
        width: (width / 2) as usize,
        row_bytes: stride_uv as usize,
    };
    let dest = VImageBuffer {
        data: rgba.as_mut_ptr(),
        height: height as usize,
        width: width as usize,
        row_bytes: (width as usize) * 4,
    };
    // ARGB component order is A=0, R=1, G=2, B=3. This writes R, G, B, A.
    let permute = [1u8, 2, 3, 0];
    let status = unsafe {
        vImageConvert_420Yp8_CbCr8ToARGB8888(
            &src_y,
            &src_uv,
            &dest,
            info,
            permute.as_ptr(),
            255,
            0,
        )
    };
    if status == 0 {
        Some(rgba)
    } else {
        Some(rgba_scalar(y, stride_y, uv, stride_uv, width, height, full_range))
    }
}

fn conversion(full_range: bool) -> Option<&'static VImageYpCbCrToARGB> {
    use std::sync::OnceLock;
    static LIMITED: OnceLock<Option<VImageYpCbCrToARGB>> = OnceLock::new();
    static FULL: OnceLock<Option<VImageYpCbCrToARGB>> = OnceLock::new();
    let slot = if full_range { &FULL } else { &LIMITED };
    slot.get_or_init(|| {
        let range = if full_range {
            VImageYpCbCrPixelRange {
                yp_bias: 0,
                cbcr_bias: 128,
                yp_range_max: 255,
                cbcr_range_max: 255,
                yp_max: 255,
                yp_min: 1,
                cbcr_max: 255,
                cbcr_min: 0,
            }
        } else {
            VImageYpCbCrPixelRange {
                yp_bias: 16,
                cbcr_bias: 128,
                yp_range_max: 235,
                cbcr_range_max: 240,
                yp_max: 255,
                yp_min: 0,
                cbcr_max: 255,
                cbcr_min: 1,
            }
        };
        let mut info = VImageYpCbCrToARGB { opaque: [0; 128] };
        let status = unsafe {
            vImageConvert_YpCbCrToARGB_GenerateConversion(
                kvImage_YpCbCrToARGBMatrix_ITU_R_709_2,
                &range,
                &mut info,
                4,
                0,
                0,
            )
        };
        if status == 0 {
            Some(info)
        } else {
            None
        }
    })
    .as_ref()
}

fn rgba_scalar(
    y: &[u8],
    stride_y: u32,
    uv: &[u8],
    stride_uv: u32,
    width: u32,
    height: u32,
    full_range: bool,
) -> Vec<u8> {
    let mut rgba = vec![0u8; (width as usize) * (height as usize) * 4];
    for row in 0..height {
        for col in 0..width {
            let luma = y
                .get((row * stride_y + col) as usize)
                .copied()
                .unwrap_or(16);
            let chroma = ((row / 2) * stride_uv + (col / 2) * 2) as usize;
            let chroma_u = uv.get(chroma).copied().unwrap_or(128);
            let chroma_v = uv.get(chroma + 1).copied().unwrap_or(128);
            let (r, g, b) = yuv(luma, chroma_u, chroma_v, full_range);
            let at = ((row * width + col) * 4) as usize;
            rgba[at] = r;
            rgba[at + 1] = g;
            rgba[at + 2] = b;
            rgba[at + 3] = 255;
        }
    }
    rgba
}

fn plane<'a>(
    guard: &'a screencapturekit::cv::CVPixelBufferLockGuard<'a>,
    index: usize,
    rows: usize,
) -> Option<&'a [u8]> {
    let ptr = guard.base_address_of_plane(index)?;
    if ptr.is_null() {
        return None;
    }
    let stride = guard.bytes_per_row_of_plane(index);
    if stride == 0 {
        return None;
    }
    let len = stride.checked_mul(rows)?;
    Some(unsafe { std::slice::from_raw_parts(ptr, len) })
}

fn yuv(y: u8, u: u8, v: u8, full_range: bool) -> (u8, u8, u8) {
    let (c, d, e) = if full_range {
        (i32::from(y), i32::from(u) - 128, i32::from(v) - 128)
    } else {
        (i32::from(y) - 16, i32::from(u) - 128, i32::from(v) - 128)
    };
    let (r, g, b) = if full_range {
        (
            ((c << 8) + 359 * e + 128) >> 8,
            ((c << 8) - 88 * d - 183 * e + 128) >> 8,
            ((c << 8) + 454 * d + 128) >> 8,
        )
    } else {
        (
            (298 * c + 409 * e + 128) >> 8,
            (298 * c - 100 * d - 208 * e + 128) >> 8,
            (298 * c + 516 * d + 128) >> 8,
        )
    };
    (
        r.clamp(0, 255) as u8,
        g.clamp(0, 255) as u8,
        b.clamp(0, 255) as u8,
    )
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

#[repr(C)]
struct VImageBuffer {
    data: *mut u8,
    height: usize,
    width: usize,
    row_bytes: usize,
}

#[repr(C)]
struct VImageYpCbCrPixelRange {
    yp_bias: i32,
    cbcr_bias: i32,
    yp_range_max: i32,
    cbcr_range_max: i32,
    yp_max: i32,
    yp_min: i32,
    cbcr_max: i32,
    cbcr_min: i32,
}

#[repr(C, align(16))]
struct VImageYpCbCrToARGB {
    opaque: [u8; 128],
}

#[link(name = "Accelerate", kind = "framework")]
extern "C" {
    static kvImage_YpCbCrToARGBMatrix_ITU_R_709_2: *const u8;
    fn vImageConvert_YpCbCrToARGB_GenerateConversion(
        matrix: *const u8,
        pixel_range: *const VImageYpCbCrPixelRange,
        out_info: *mut VImageYpCbCrToARGB,
        in_type: u32,
        out_type: u32,
        flags: u32,
    ) -> i32;
    fn vImageConvert_420Yp8_CbCr8ToARGB8888(
        src_y: *const VImageBuffer,
        src_cbcr: *const VImageBuffer,
        dest: *const VImageBuffer,
        info: *const VImageYpCbCrToARGB,
        permute_map: *const u8,
        alpha: u8,
        flags: u32,
    ) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn nv12_conversion_is_fast_enough_for_60fps() {
        let width = 1920u32;
        let height = 1080u32;
        let y = vec![235u8; (width * height) as usize];
        let uv = vec![128u8; (width * height / 2) as usize];
        let started = Instant::now();
        let mut rgba = Vec::new();
        for _ in 0..8 {
            rgba = nv12_to_rgba(&y, width, &uv, width, width, height, false).expect("convert");
        }
        let each = started.elapsed() / 8;
        assert_eq!(rgba.len(), (width * height * 4) as usize);
        assert!(rgba[0] > 200 && rgba[1] > 200 && rgba[2] > 200, "limited-range white");
        assert!(
            each.as_millis() < 12,
            "1080p conversion took {each:?}, which cannot hold 60 fps"
        );
    }
}
