use anyhow::Result;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_void};

extern "C" {
    fn dcmax_presenter_new(ns_view: *mut c_void, error: *mut c_char, error_cap: c_int) -> *mut c_void;
    fn dcmax_presenter_resize(presenter: *mut c_void);
    fn dcmax_presenter_draw_nv12(
        presenter: *mut c_void,
        pixel_buffer: *mut c_void,
        full_range: c_int,
        cursor: *const f32,
    ) -> c_int;
    fn dcmax_presenter_draw_bgra(
        presenter: *mut c_void,
        bytes: *const u8,
        width: u32,
        height: u32,
        stride: u32,
        cursor: *const f32,
    ) -> c_int;
    fn dcmax_presenter_free(presenter: *mut c_void);
    fn dcmax_vsync_start(callback: extern "C" fn(*mut c_void), context: *mut c_void) -> c_int;
    fn dcmax_vsync_stop();
}

pub struct Presenter(*mut c_void);

impl Presenter {
    pub fn attach(ns_view: *mut c_void) -> Result<Self> {
        let mut error = [0u8; 512];
        let raw = unsafe {
            dcmax_presenter_new(
                ns_view,
                error.as_mut_ptr().cast::<c_char>(),
                error.len() as c_int,
            )
        };
        if raw.is_null() {
            let message = CStr::from_bytes_until_nul(&error)
                .ok()
                .and_then(|text| text.to_str().ok())
                .filter(|text| !text.is_empty())
                .unwrap_or("Metal presenter failed");
            anyhow::bail!("{message}");
        }
        Ok(Self(raw))
    }

    pub fn draw_nv12(&self, pixel_buffer: *mut c_void, full_range: bool, pointer: Pointer) -> i32 {
        let cursor = pointer.bytes();
        unsafe {
            dcmax_presenter_resize(self.0);
            dcmax_presenter_draw_nv12(self.0, pixel_buffer, i32::from(full_range), cursor.as_ptr())
        }
    }

    pub fn draw_bgra(&self, pixels: &[u32], width: u32, height: u32, pointer: Pointer) -> i32 {
        let Some(stride) = width.checked_mul(4) else {
            return -1;
        };
        let cursor = pointer.bytes();
        unsafe {
            dcmax_presenter_resize(self.0);
            dcmax_presenter_draw_bgra(
                self.0,
                pixels.as_ptr().cast::<u8>(),
                width,
                height,
                stride,
                cursor.as_ptr(),
            )
        }
    }
}

#[derive(Clone, Copy)]
pub struct Pointer {
    pub visible: bool,
    pub x: f32,
    pub y: f32,
}

impl Pointer {
    fn bytes(self) -> [f32; 5] {
        [
            self.x,
            self.y,
            if self.visible { 1.0 } else { 0.0 },
            0.0,
            0.0,
        ]
    }
}

impl Drop for Presenter {
    fn drop(&mut self) {
        unsafe { dcmax_presenter_free(self.0) }
    }
}

pub fn start_vsync(callback: extern "C" fn(*mut c_void), context: *mut c_void) -> Result<()> {
    let status = unsafe { dcmax_vsync_start(callback, context) };
    if status != 0 {
        anyhow::bail!("display link failed to start");
    }
    Ok(())
}

pub fn stop_vsync() {
    unsafe { dcmax_vsync_stop() }
}
