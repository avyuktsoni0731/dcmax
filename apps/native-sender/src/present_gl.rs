//! Draws the watched picture inside the window without copying it through egui.
//!
//! On macOS the decoder already produced an IOSurface. OpenGL samples that
//! surface in place, which is what keeps a 60 fps picture on a 60 Hz display.
//! Other platforms upload the decoded pixels straight to a texture.

use eframe::glow;
use eframe::glow::HasContext;
use std::cell::RefCell;

#[cfg(target_os = "macos")]
use screencapturekit::cv::CVPixelBuffer;

pub enum Picture {
    #[cfg(target_os = "macos")]
    Nv12 {
        buffer: CVPixelBuffer,
        full_range: bool,
    },
    Rgba {
        width: i32,
        height: i32,
        pixels: Vec<u8>,
    },
}

pub fn draw(gl: &glow::Context, picture: &Picture) {
    STATE.with(|slot| {
        let mut state = slot.borrow_mut();
        match picture {
            #[cfg(target_os = "macos")]
            Picture::Nv12 { buffer, full_range } => draw_nv12(gl, &mut state, buffer, *full_range),
            Picture::Rgba { width, height, pixels } => {
                draw_rgba(gl, &mut state, *width, *height, pixels, picture as *const Picture as usize)
            }
        }
    });
}

struct State {
    nv12: Option<Program>,
    rgba: Option<Program>,
    uploaded: usize,
    noted: bool,
}

#[derive(Clone, Copy)]
struct Program {
    program: glow::Program,
    y: glow::Texture,
    c: glow::Texture,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    rect: bool,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State {
        nv12: None,
        rgba: None,
        uploaded: 0,
        noted: false,
    });
}

fn draw_rgba(gl: &glow::Context, state: &mut State, width: i32, height: i32, pixels: &[u8], id: usize) {
    if width <= 0 || height <= 0 || pixels.len() != (width as usize) * (height as usize) * 4 {
        return;
    }
    let Some(program) = program(gl, &mut state.rgba, RGBA_FRAG, false) else {
        return;
    };
    unsafe {
        if state.uploaded != id {
            gl.bind_texture(glow::TEXTURE_2D, Some(program.y));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                width,
                height,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(pixels)),
            );
            state.uploaded = id;
        }
        paint(gl, &program, width as f32, height as f32, false);
    }
}

#[cfg(target_os = "macos")]
fn draw_nv12(gl: &glow::Context, state: &mut State, buffer: &CVPixelBuffer, full_range: bool) {
    let width = buffer.width() as i32;
    let height = buffer.height() as i32;
    if width <= 0 || height <= 0 {
        return;
    }
    let Some(program) = program(gl, &mut state.nv12, NV12_FRAG, true) else {
        return;
    };
    let surface = unsafe { CVPixelBufferGetIOSurface(buffer.as_ptr()) };
    if surface.is_null() {
        note_once("watch picture: decoded frame has no gpu surface");
        return;
    }
    unsafe {
        gl.bind_texture(glow::TEXTURE_RECTANGLE, Some(program.y));
        let y_ok = CGLTexImageIOSurface2D(
            CGLGetCurrentContext(),
            glow::TEXTURE_RECTANGLE,
            glow::R8,
            width,
            height,
            glow::RED,
            glow::UNSIGNED_BYTE,
            surface,
            0,
        );
        gl.bind_texture(glow::TEXTURE_RECTANGLE, Some(program.c));
        let c_ok = CGLTexImageIOSurface2D(
            CGLGetCurrentContext(),
            glow::TEXTURE_RECTANGLE,
            glow::RG8,
            width / 2,
            height / 2,
            glow::RG,
            glow::UNSIGNED_BYTE,
            surface,
            1,
        );
        if y_ok != 0 || c_ok != 0 {
            note_once(&format!("watch picture: gpu surface was rejected ({y_ok}, {c_ok})"));
            return;
        }
        paint(gl, &program, width as f32, height as f32, full_range);
        if !state.noted {
            state.noted = true;
            crate::audio::session_log("watch picture: showing on the gpu");
        }
    }
}

fn program(gl: &glow::Context, slot: &mut Option<Program>, fragment: &str, rect: bool) -> Option<Program> {
    if let Some(program) = *slot {
        return Some(program);
    }
    let program = Program::new(gl, fragment, rect)?;
    *slot = Some(program);
    Some(program)
}

unsafe fn paint(gl: &glow::Context, program: &Program, width: f32, height: f32, full_range: bool) {
    gl.use_program(Some(program.program));
    gl.bind_vertex_array(Some(program.vao));
    gl.active_texture(glow::TEXTURE0);
    let y_target = if program.rect { glow::TEXTURE_RECTANGLE } else { glow::TEXTURE_2D };
    gl.bind_texture(y_target, Some(program.y));
    gl.active_texture(glow::TEXTURE1);
    gl.bind_texture(y_target, Some(program.c));
    if let Some(location) = gl.get_uniform_location(program.program, "y_tex") {
        gl.uniform_1_i32(Some(&location), 0);
    }
    if let Some(location) = gl.get_uniform_location(program.program, "c_tex") {
        gl.uniform_1_i32(Some(&location), 1);
    }
    if let Some(location) = gl.get_uniform_location(program.program, "size") {
        gl.uniform_2_f32(Some(&location), width, height);
    }
    if let Some(location) = gl.get_uniform_location(program.program, "full_range") {
        gl.uniform_1_f32(Some(&location), if full_range { 1.0 } else { 0.0 });
    }
    gl.disable(glow::BLEND);
    gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
    let _keep_buffer = program.vbo;
    gl.bind_vertex_array(None);
    gl.use_program(None);
}

impl Program {
    fn new(gl: &glow::Context, fragment: &str, rect: bool) -> Option<Self> {
        unsafe {
            let program = gl.create_program().ok()?;
            let vertex = shader(gl, glow::VERTEX_SHADER, if rect { VERT_RECT } else { VERT_RGBA })?;
            let fragment = shader(gl, glow::FRAGMENT_SHADER, fragment)?;
            gl.attach_shader(program, vertex);
            gl.attach_shader(program, fragment);
            gl.bind_attrib_location(program, 0, "pos");
            gl.link_program(program);
            gl.delete_shader(vertex);
            gl.delete_shader(fragment);
            if !gl.get_program_link_status(program) {
                note_once(&format!("watch picture: {}", gl.get_program_info_log(program)));
                gl.delete_program(program);
                return None;
            }
            let y = gl.create_texture().ok()?;
            let c = gl.create_texture().ok()?;
            let target = if rect { glow::TEXTURE_RECTANGLE } else { glow::TEXTURE_2D };
            for texture in [y, c] {
                gl.bind_texture(target, Some(texture));
                gl.tex_parameter_i32(target, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
                gl.tex_parameter_i32(target, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
                gl.tex_parameter_i32(target, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
                gl.tex_parameter_i32(target, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
            }
            let vao = gl.create_vertex_array().ok()?;
            let vbo = gl.create_buffer().ok()?;
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            let quad: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytemuck_quad(&quad), glow::STATIC_DRAW);
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
            gl.bind_vertex_array(None);
            Some(Self { program, y, c, vao, vbo, rect })
        }
    }
}

fn bytemuck_quad(quad: &[f32; 8]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(quad.as_ptr().cast(), 32) }
}

fn shader(gl: &glow::Context, kind: u32, source: &str) -> Option<glow::Shader> {
    unsafe {
        let shader = gl.create_shader(kind).ok()?;
        gl.shader_source(shader, source);
        gl.compile_shader(shader);
        if gl.get_shader_compile_status(shader) {
            Some(shader)
        } else {
            note_once(&format!("watch picture: {}", gl.get_shader_info_log(shader)));
            gl.delete_shader(shader);
            None
        }
    }
}

fn note_once(message: &str) {
    thread_local! {
        static SEEN: RefCell<bool> = const { RefCell::new(false) };
    }
    let fresh = SEEN.with(|seen| {
        let mut seen = seen.borrow_mut();
        let fresh = !*seen;
        *seen = true;
        fresh
    });
    if fresh {
        crate::audio::session_log(message);
    }
}

const VERT_RECT: &str = "#version 150
in vec2 pos;
uniform vec2 size;
out vec2 uv;
void main() {
    uv = vec2(pos.x * 0.5 + 0.5, 0.5 - pos.y * 0.5) * size;
    gl_Position = vec4(pos, 0.0, 1.0);
}";

const VERT_RGBA: &str = "#version 150
in vec2 pos;
out vec2 uv;
void main() {
    uv = vec2(pos.x * 0.5 + 0.5, 0.5 - pos.y * 0.5);
    gl_Position = vec4(pos, 0.0, 1.0);
}";

const NV12_FRAG: &str = "#version 150
uniform sampler2DRect y_tex;
uniform sampler2DRect c_tex;
uniform float full_range;
in vec2 uv;
out vec4 color;
void main() {
    float y = texture(y_tex, uv).r;
    vec2 c = texture(c_tex, uv * 0.5).rg - vec2(0.5);
    if (full_range < 0.5) {
        y = (y - 0.062745) * 1.164384;
    }
    float r = y + 1.792741 * c.y;
    float g = y - 0.213249 * c.x - 0.532909 * c.y;
    float b = y + 2.112402 * c.x;
    color = vec4(clamp(vec3(r, g, b), 0.0, 1.0), 1.0);
}";

const RGBA_FRAG: &str = "#version 150
uniform sampler2D y_tex;
in vec2 uv;
out vec4 color;
void main() {
    color = texture(y_tex, uv);
}";

#[cfg(target_os = "macos")]
#[link(name = "OpenGL", kind = "framework")]
extern "C" {
    fn CGLGetCurrentContext() -> *mut std::ffi::c_void;
    fn CGLTexImageIOSurface2D(
        ctx: *mut std::ffi::c_void,
        target: u32,
        internal_format: u32,
        width: i32,
        height: i32,
        format: u32,
        ty: u32,
        io_surface: *mut std::ffi::c_void,
        plane: u32,
    ) -> i32;
}

#[cfg(target_os = "macos")]
#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn CVPixelBufferGetIOSurface(pixel_buffer: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
}
