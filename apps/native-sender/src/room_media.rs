//! A LiveKit screen frame, turned into the picture the native window already draws.
//!
//! The room path subscribes to the screen track. WebRTC has already decoded it.
//! This copies that image into the same NV12 the direct decoder produces, so the
//! window does not grow a second presenter.

use livekit::webrtc::prelude::VideoBuffer;
use livekit::webrtc::video_frame::BoxVideoFrame;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(target_os = "macos")]
use screencapturekit::cm::{IOSurface, PlaneProperties};
#[cfg(target_os = "macos")]
use screencapturekit::cv::{CVPixelBuffer, CVPixelBufferLockFlags};

pub struct Shown {
    pub width: usize,
    pub height: usize,
    pub picture: crate::present_gl::Picture,
}

pub fn picture_from_frame(frame: &BoxVideoFrame) -> Option<Shown> {
    let buffer = frame.buffer.as_ref();
    let width = buffer.width() as usize;
    let height = buffer.height() as usize;
    if width < 2 || height < 2 {
        return None;
    }
    #[cfg(target_os = "macos")]
    if let Some(picture) = native_picture(buffer) {
        note_once("picture room: drawing the screen track from the decoder surface");
        return Some(Shown { width, height, picture });
    }
    if let Some(i420) = buffer.as_i420() {
        let (y_stride, u_stride, v_stride) = i420.strides();
        let (y, u, v) = i420.data();
        let (y, uv) = i420_to_nv12(
            y,
            u,
            v,
            y_stride as usize,
            u_stride as usize,
            v_stride as usize,
            width,
            height,
        )?;
        note_once("picture room: drawing the screen track from decoded video");
        return pack(width, height, y, uv);
    }
    if let Some(nv12) = buffer.as_nv12() {
        let (y_stride, uv_stride) = nv12.strides();
        let (y, uv) = nv12.data();
        let y = tight(y, y_stride as usize, width, height)?;
        let uv = tight(uv, uv_stride as usize, width, height / 2)?;
        note_once("picture room: drawing the screen track from decoded video");
        return pack(width, height, y, uv);
    }
    note_once(&format!(
        "picture room: a {:?} frame could not be drawn",
        buffer.buffer_type()
    ));
    None
}

fn note_once(line: &str) {
    static NOTED: AtomicBool = AtomicBool::new(false);
    if !NOTED.swap(true, Ordering::Relaxed) {
        crate::audio::session_log(line);
    }
}

pub fn i420_to_nv12(
    y: &[u8],
    u: &[u8],
    v: &[u8],
    y_stride: usize,
    u_stride: usize,
    v_stride: usize,
    width: usize,
    height: usize,
) -> Option<(Vec<u8>, Vec<u8>)> {
    if width < 2 || height < 2 || width % 2 != 0 || height % 2 != 0 {
        return None;
    }
    let y_out = tight(y, y_stride, width, height)?;
    let chroma_w = width / 2;
    let chroma_h = height / 2;
    let mut uv = vec![0u8; width * chroma_h];
    for row in 0..chroma_h {
        for col in 0..chroma_w {
            let u_index = row * u_stride + col;
            let v_index = row * v_stride + col;
            if u_index >= u.len() || v_index >= v.len() {
                return None;
            }
            let offset = row * width + col * 2;
            uv[offset] = u[u_index];
            uv[offset + 1] = v[v_index];
        }
    }
    Some((y_out, uv))
}

fn tight(src: &[u8], stride: usize, row_bytes: usize, rows: usize) -> Option<Vec<u8>> {
    if row_bytes == 0 || rows == 0 || stride < row_bytes {
        return None;
    }
    let mut out = vec![0u8; row_bytes * rows];
    copy_plane(out.as_mut_ptr(), row_bytes, src, stride, row_bytes, rows);
    Some(out)
}

fn copy_plane(dst: *mut u8, dst_stride: usize, src: &[u8], src_stride: usize, row_bytes: usize, rows: usize) {
    if dst.is_null() || row_bytes == 0 || dst_stride < row_bytes || src_stride < row_bytes {
        return;
    }
    for row in 0..rows {
        let start = row.saturating_mul(src_stride);
        let end = start.saturating_add(row_bytes);
        if end > src.len() {
            return;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(src[start..end].as_ptr(), dst.add(row * dst_stride), row_bytes);
        }
    }
}

#[cfg(target_os = "macos")]
fn pack(width: usize, height: usize, y: Vec<u8>, uv: Vec<u8>) -> Option<Shown> {
    let picture = surface_picture(&y, &uv, width, height)?;
    Some(Shown { width, height, picture })
}

#[cfg(target_os = "windows")]
fn pack(width: usize, height: usize, y: Vec<u8>, uv: Vec<u8>) -> Option<Shown> {
    Some(Shown {
        width,
        height,
        picture: crate::present_gl::Picture::Nv12Bytes {
            width: width as i32,
            height: height as i32,
            y,
            uv,
            full_range: false,
        },
    })
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn pack(width: usize, height: usize, y: Vec<u8>, uv: Vec<u8>) -> Option<Shown> {
    let _ = (width, height, y, uv);
    None
}

#[cfg(target_os = "macos")]
fn native_picture(buffer: &dyn VideoBuffer) -> Option<crate::present_gl::Picture> {
    let native = buffer.as_native()?;
    let pixel = unsafe { CVPixelBuffer::from_raw_borrowed(native.get_cv_pixel_buffer()) }?;
    let format = pixel.pixel_format();
    let limited = u32::from_be_bytes(*b"420v");
    let full = u32::from_be_bytes(*b"420f");
    if format != limited && format != full {
        return None;
    }
    if pixel.io_surface().is_none() {
        return None;
    }
    Some(crate::present_gl::Picture::Nv12 {
        buffer: pixel,
        full_range: format == full,
    })
}

#[cfg(target_os = "macos")]
fn surface_picture(y: &[u8], uv: &[u8], width: usize, height: usize) -> Option<crate::present_gl::Picture> {
    let y_bpr = align16(width);
    let uv_bpr = align16(width);
    let y_size = y_bpr * height;
    let uv_size = uv_bpr * (height / 2);
    let planes = [
        PlaneProperties {
            width,
            height,
            bytes_per_row: y_bpr,
            bytes_per_element: 1,
            offset: 0,
            size: y_size,
        },
        PlaneProperties {
            width: width / 2,
            height: height / 2,
            bytes_per_row: uv_bpr,
            bytes_per_element: 2,
            offset: y_size,
            size: uv_size,
        },
    ];
    let surface = IOSurface::create_with_properties(
        width,
        height,
        u32::from_be_bytes(*b"420v"),
        1,
        y_bpr,
        y_size + uv_size,
        Some(&planes),
    )?;
    let buffer = CVPixelBuffer::create_with_io_surface(&surface).ok()?;
    write_planes(&buffer, y, width, uv, width, width, height)?;
    Some(crate::present_gl::Picture::Nv12 {
        buffer,
        full_range: false,
    })
}

#[cfg(target_os = "macos")]
fn write_planes(
    buffer: &CVPixelBuffer,
    y: &[u8],
    y_stride: usize,
    uv: &[u8],
    uv_stride: usize,
    width: usize,
    height: usize,
) -> Option<()> {
    let mut guard = buffer.lock(CVPixelBufferLockFlags::NONE).ok()?;
    let y_dst = guard.base_address_of_plane_mut(0)?;
    let uv_dst = guard.base_address_of_plane_mut(1)?;
    let y_pitch = guard.bytes_per_row_of_plane(0);
    let uv_pitch = guard.bytes_per_row_of_plane(1);
    copy_plane(y_dst, y_pitch, y, y_stride, width, height);
    copy_plane(uv_dst, uv_pitch, uv, uv_stride, width, height / 2);
    Some(())
}

#[cfg(target_os = "macos")]
fn align16(value: usize) -> usize {
    value.saturating_add(15) & !15
}

#[cfg(test)]
mod tests {
    use super::i420_to_nv12;

    #[test]
    fn i420_becomes_tight_nv12() {
        let y = [1u8, 2, 3, 4];
        let u = [5u8];
        let v = [6u8];
        let (y_out, uv) = i420_to_nv12(&y, &u, &v, 2, 1, 1, 2, 2).unwrap();
        assert_eq!(y_out, vec![1, 2, 3, 4]);
        assert_eq!(uv, vec![5, 6]);
    }

    #[test]
    fn a_stride_does_not_leak_into_the_packed_row() {
        let y = [1u8, 2, 9, 3, 4, 9];
        let u = [5u8, 8];
        let v = [6u8, 8];
        let (y_out, uv) = i420_to_nv12(&y, &u, &v, 3, 2, 2, 2, 2).unwrap();
        assert_eq!(y_out, vec![1, 2, 3, 4]);
        assert_eq!(uv, vec![5, 6]);
    }
}
