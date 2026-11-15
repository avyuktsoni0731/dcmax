//! Windows picture decode. Media Foundation uses the GPU decoder when the
//! machine has one, and a software Media Foundation decoder otherwise.
//! openh264 is only the last resort. Both paths hand NV12 to the same shader
//! that expands studio swing, which is the range our encoders produce.

use crate::present_gl::Picture;
use std::os::raw::{c_char, c_int};

#[repr(C)]
struct MfDecoder {
    _private: [u8; 0],
}

extern "C" {
    fn mf_open() -> *mut MfDecoder;
    fn mf_last_error() -> *const c_char;
    fn mf_hardware(decoder: *mut MfDecoder) -> c_int;
    fn mf_take_note(decoder: *mut MfDecoder, out: *mut c_char, cap: c_int) -> c_int;
    fn mf_decode(
        decoder: *mut MfDecoder,
        data: *const u8,
        length: c_int,
        y: *mut *const u8,
        uv: *mut *const u8,
        width: *mut c_int,
        height: *mut c_int,
    ) -> c_int;
    fn mf_close(decoder: *mut MfDecoder);
}

pub struct Decoder {
    media: *mut MfDecoder,
    software: Option<openh264::decoder::Decoder>,
    noted: bool,
}

impl Decoder {
    pub fn new() -> Self {
        let media = unsafe { mf_open() };
        let software = if media.is_null() {
            let reason = unsafe { std::ffi::CStr::from_ptr(mf_last_error()) }.to_string_lossy();
            crate::audio::session_log(&format!("watch picture: media foundation unavailable ({reason})"));
            openh264::decoder::Decoder::new().ok()
        } else {
            None
        };
        Self {
            media,
            software,
            noted: false,
        }
    }

    pub fn decode(&mut self, payload: &[u8]) -> Option<Picture> {
        if !self.media.is_null() {
            if let Some(picture) = self.decode_media(payload) {
                return Some(picture);
            }
            if self.media.is_null() {
                return self.decode_software(payload);
            }
            return None;
        }
        self.decode_software(payload)
    }

    fn decode_media(&mut self, payload: &[u8]) -> Option<Picture> {
        let mut y_ptr = std::ptr::null();
        let mut uv_ptr = std::ptr::null();
        let mut width = 0;
        let mut height = 0;
        let status = unsafe {
            mf_decode(
                self.media,
                payload.as_ptr(),
                payload.len() as c_int,
                &mut y_ptr,
                &mut uv_ptr,
                &mut width,
                &mut height,
            )
        };
        let mut note = [0u8; 96];
        if unsafe { mf_take_note(self.media, note.as_mut_ptr() as *mut c_char, note.len() as c_int) } == 1 {
            let text = std::ffi::CStr::from_bytes_until_nul(&note)
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "gpu readback failed".to_string());
            crate::audio::session_log(&format!("watch picture: {text}"));
        }
        if status < 0 {
            let reason = unsafe { std::ffi::CStr::from_ptr(mf_last_error()) }.to_string_lossy();
            crate::audio::session_log(&format!("watch picture: hardware decode stopped ({reason})"));
            unsafe { mf_close(self.media) };
            self.media = std::ptr::null_mut();
            self.software = openh264::decoder::Decoder::new().ok();
            return None;
        }
        if status == 0 || y_ptr.is_null() || uv_ptr.is_null() || width < 2 || height < 2 {
            return None;
        }
        let y_bytes = (width as usize).saturating_mul(height as usize);
        let uv_bytes = y_bytes / 2;
        if y_bytes == 0 || uv_bytes == 0 {
            return None;
        }
        let y = unsafe { std::slice::from_raw_parts(y_ptr, y_bytes) }.to_vec();
        let uv = unsafe { std::slice::from_raw_parts(uv_ptr, uv_bytes) }.to_vec();
        if !self.noted {
            self.noted = true;
            let kind = if unsafe { mf_hardware(self.media) } == 1 {
                "hardware"
            } else {
                "media foundation"
            };
            crate::audio::session_log(&format!("watch picture: {kind} decode"));
        }
        Some(Picture::Nv12Bytes {
            width,
            height,
            y,
            uv,
            full_range: false,
        })
    }

    fn decode_software(&mut self, payload: &[u8]) -> Option<Picture> {
        use openh264::formats::YUVSource;
        let yuv = self.software.as_mut()?.decode(payload).ok()??;
        let (width, height) = yuv.dimensions();
        if width == 0 || height == 0 || width > 7680 || height > 4320 || width % 2 != 0 || height % 2 != 0 {
            return None;
        }
        let picture = i420_to_nv12(width, height, yuv.y(), yuv.strides().0, yuv.u(), yuv.strides().1, yuv.v(), yuv.strides().2)?;
        if !self.noted {
            self.noted = true;
            crate::audio::session_log("watch picture: software decode");
        }
        Some(picture)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        if !self.media.is_null() {
            unsafe { mf_close(self.media) };
            self.media = std::ptr::null_mut();
        }
    }
}

fn i420_to_nv12(
    width: usize,
    height: usize,
    y_plane: &[u8],
    y_stride: usize,
    u_plane: &[u8],
    u_stride: usize,
    v_plane: &[u8],
    v_stride: usize,
) -> Option<Picture> {
    let y_bytes = width.checked_mul(height)?;
    let chroma_rows = height / 2;
    let chroma_cols = width / 2;
    let mut y = vec![0u8; y_bytes];
    let mut uv = vec![0u8; y_bytes / 2];
    if y_stride < width || u_stride < chroma_cols || v_stride < chroma_cols {
        return None;
    }
    for row in 0..height {
        let start = row * y_stride;
        y[row * width..(row + 1) * width].copy_from_slice(&y_plane[start..start + width]);
    }
    for row in 0..chroma_rows {
        let u_row = &u_plane[row * u_stride..row * u_stride + chroma_cols];
        let v_row = &v_plane[row * v_stride..row * v_stride + chroma_cols];
        let dest = &mut uv[row * width..(row + 1) * width];
        for col in 0..chroma_cols {
            dest[col * 2] = u_row[col];
            dest[col * 2 + 1] = v_row[col];
        }
    }
    Some(Picture::Nv12Bytes {
        width: width as i32,
        height: height as i32,
        y,
        uv,
        full_range: false,
    })
}
