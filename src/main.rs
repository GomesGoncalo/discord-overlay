//! hypr-overlay-wl — phase 2: EGL/GLES2 hardware-accelerated layer-shell overlay.
//!
//! Transparent except for two rounded buttons (Mute/Deafen) and a drag handle.
//! Click-through everywhere else via wl_surface.set_input_region.
//! Discord IPC: set DISCORD_CLIENT_ID + DISCORD_CLIENT_SECRET to enable.

mod discord;

use std::ffi::c_void;
use std::num::NonZeroU32;
use std::sync::mpsc;

use sctk::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit as sctk;
use smithay_client_toolkit::shell::WaylandSurface;

use sctk::compositor::{CompositorHandler, CompositorState, Region};
use sctk::output::{OutputHandler, OutputState};
use sctk::reexports::client::globals::registry_queue_init;
use sctk::reexports::client::protocol::{wl_output, wl_pointer, wl_seat};
use sctk::reexports::client::Connection;
use sctk::reexports::client::Proxy;
use sctk::reexports::client::QueueHandle;
use sctk::registry::{ProvidesRegistryState, RegistryState};
use sctk::seat::keyboard::{KeyboardHandler, Modifiers};
use sctk::seat::pointer::BTN_LEFT;
use sctk::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use sctk::seat::{Capability, SeatHandler, SeatState};
use sctk::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use sctk::{
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, registry_handlers,
};

use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use glow::HasContext;
use khronos_egl as egl;
use wayland_egl::WlEglSurface;

// ─── GLSL shaders (GLES2 / #version 100) ─────────────────────────────────────

const VERT_SRC: &str = r"
attribute vec2 a_position;
attribute vec2 a_local;
varying vec2 v_local;
void main() {
    gl_Position = vec4(a_position, 0.0, 1.0);
    v_local = a_local;
}
";

/// Rounded-rectangle SDF shader used for button backgrounds and the drag handle.
const FRAG_SRC: &str = r"
precision mediump float;
varying vec2 v_local;
uniform vec4 u_color;
uniform vec2 u_size;
uniform float u_radius;

float sdRoundedBox(vec2 p, vec2 b, float r) {
    vec2 d = abs(p) - b + r;
    return length(max(d, 0.0)) + min(max(d.x, d.y), 0.0) - r;
}
void main() {
    vec2 p = (v_local - 0.5) * u_size;
    float d = sdRoundedBox(p, u_size * 0.5, u_radius);
    if (d > 1.0) discard;
    gl_FragColor = u_color;
}
";

/// Simple icon overlay shader — samples a white-on-transparent texture and
/// multiplies by an opacity uniform. Y is flipped because pixel buffers are
/// top-down but GL textures are bottom-up.
const ICON_FRAG_SRC: &str = r"
precision mediump float;
varying vec2 v_local;
uniform sampler2D u_texture;
uniform float u_opacity;
void main() {
    vec4 c = texture2D(u_texture, vec2(v_local.x, 1.0 - v_local.y));
    gl_FragColor = vec4(c.rgb, c.a * u_opacity);
}
";

/// Avatar shader — clips the quad to a circle using SDF.
const AVATAR_FRAG_SRC: &str = r"
precision mediump float;
varying vec2 v_local;
uniform sampler2D u_texture;
uniform float u_opacity;
void main() {
    vec2 uv = v_local - 0.5;
    float d = length(uv) - 0.5 + 0.015;
    float aa = 0.015;
    float a = 1.0 - smoothstep(-aa, aa, d);
    vec4 c = texture2D(u_texture, vec2(v_local.x, 1.0 - v_local.y));
    gl_FragColor = vec4(c.rgb, c.a * a * u_opacity);
}
";

// EGL_PLATFORM_WAYLAND_KHR (0x31D8) — part of EGL_KHR_platform_wayland
const EGL_PLATFORM_WAYLAND_KHR: egl::Enum = 0x31D8;

// ─── Procedural icon generation ──────────────────────────────────────────────

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn sdf_rrect(px: f32, py: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let dx = (px - cx).abs() - hw + r;
    let dy = (py - cy).abs() - hh + r;
    dx.max(0.0).hypot(dy.max(0.0)) + dx.min(0.0).max(dy.min(0.0)) - r
}

fn sdf_arc(px: f32, py: f32, cx: f32, cy: f32, r: f32, w: f32, bottom: bool) -> f32 {
    let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
    let ring = (d - r).abs() - w;
    if bottom {
        if py <= cy {
            ring
        } else {
            99.0
        }
    } else {
        if py >= cy {
            ring
        } else {
            99.0
        }
    }
}

fn rasterize(size: u32, sdf_fn: impl Fn(f32, f32) -> f32) -> Vec<u8> {
    let s = size as f32;
    let aa = 1.5 / s;
    let mut buf = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let px = x as f32 / s - 0.5;
            let py = y as f32 / s - 0.5;
            let d = sdf_fn(px, py);
            let a = (1.0 - smoothstep(-aa, aa, d)) * 255.0;
            let i = ((y * size + x) * 4) as usize;
            buf[i..i + 3].fill(255);
            buf[i + 3] = a as u8;
        }
    }
    buf
}

fn icon_mic(size: u32) -> Vec<u8> {
    rasterize(size, |px, py| {
        let body = sdf_rrect(px, py, 0.0, 0.08, 0.12, 0.22, 0.12);
        let stand = sdf_arc(px, py, 0.0, -0.08, 0.28, 0.03, true);
        let stem = sdf_rrect(px, py, 0.0, -0.36, 0.03, 0.055, 0.03);
        let base = sdf_rrect(px, py, 0.0, -0.42, 0.16, 0.03, 0.03);
        body.min(stand).min(stem).min(base)
    })
}

fn icon_headphone(size: u32) -> Vec<u8> {
    rasterize(size, |px, py| {
        let band = sdf_arc(px, py, 0.0, 0.0, 0.32, 0.04, false);
        let left = sdf_rrect(px, py, -0.32, -0.16, 0.09, 0.15, 0.07);
        let right = sdf_rrect(px, py, 0.32, -0.16, 0.09, 0.15, 0.07);
        band.min(left).min(right)
    })
}

fn icon_strikeout(size: u32) -> Vec<u8> {
    let s = size as f32;
    let aa = 1.5 / s;
    let mut buf = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let px = x as f32 / s - 0.5;
            let py = y as f32 / s - 0.5;
            let (ax, ay, bx, by) = (-0.40f32, -0.45f32, 0.40f32, 0.45f32);
            let (dx, dy) = (bx - ax, by - ay);
            let t = ((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy);
            let t = t.clamp(0.0, 1.0);
            let d = ((px - (ax + t * dx)).powi(2) + (py - (ay + t * dy)).powi(2)).sqrt() - 0.045;
            let a = (1.0 - smoothstep(-aa, aa, d)) * 230.0;
            let i = ((y * size + x) * 4) as usize;
            buf[i] = 20;
            buf[i + 1] = 10;
            buf[i + 2] = 10;
            buf[i + 3] = a as u8;
        }
    }
    buf
}

// ─── EGL + GL context ────────────────────────────────────────────────────────

struct EglContext {
    egl: egl::DynamicInstance<egl::EGL1_5>,
    egl_display: egl::Display,
    egl_surface: egl::Surface,
    _egl_context: egl::Context,
    wl_egl: WlEglSurface,
    gl: glow::Context,
    // Rounded-rect shader (background fills)
    program: glow::NativeProgram,
    vbo: glow::NativeBuffer,
    loc_color: glow::UniformLocation,
    loc_size: glow::UniformLocation,
    loc_radius: glow::UniformLocation,
    loc_pos: u32,
    loc_local: u32,
    // Icon overlay shader + textures
    icon_prog: glow::NativeProgram,
    icon_loc_opacity: glow::UniformLocation,
    tex_mic: glow::NativeTexture,
    tex_headphone: glow::NativeTexture,
    tex_strikeout: glow::NativeTexture,
    // Circular avatar shader
    avatar_prog: glow::NativeProgram,
    avatar_loc_opacity: glow::UniformLocation,
}

impl EglContext {
    fn new(conn: &Connection, wl_surface: &WlSurface, width: i32, height: i32) -> Self {
        // Load libEGL.so.1 dynamically and require at least EGL 1.5
        let egl_inst = unsafe {
            egl::DynamicInstance::<egl::EGL1_5>::load_required()
                .expect("EGL 1.5 required — is libEGL.so.1 installed?")
        };

        // Pass the wl_display* to EGL.
        // We use eglGetDisplay(wl_display*) which Mesa has always supported for Wayland,
        // with eglGetPlatformDisplay as an optional stronger path.
        let display_ptr = conn.backend().display_ptr() as *mut c_void;
        assert!(
            !display_ptr.is_null(),
            "wl_display pointer is null — client_system not active?"
        );

        let egl_display = unsafe {
            egl_inst
                .get_platform_display(EGL_PLATFORM_WAYLAND_KHR, display_ptr, &[])
                .or_else(|_| {
                    egl_inst
                        .get_display(display_ptr)
                        .ok_or(khronos_egl::Error::BadDisplay)
                })
                .expect("failed to get EGL display for Wayland")
        };
        egl_inst.initialize(egl_display).expect("EGL init failed");

        // Choose an RGBA8 config with a full alpha channel for true transparency
        let cfg_attribs = [
            egl::SURFACE_TYPE,
            egl::WINDOW_BIT,
            egl::RENDERABLE_TYPE,
            egl::OPENGL_ES2_BIT,
            egl::RED_SIZE,
            8,
            egl::GREEN_SIZE,
            8,
            egl::BLUE_SIZE,
            8,
            egl::ALPHA_SIZE,
            8,
            egl::NONE,
        ];
        let config = egl_inst
            .choose_first_config(egl_display, &cfg_attribs)
            .expect("EGL choose_config failed")
            .expect("no suitable EGL config found");

        // Create GLES2 context
        egl_inst
            .bind_api(egl::OPENGL_ES_API)
            .expect("bind GLES2 API");
        let ctx_attribs = [egl::CONTEXT_CLIENT_VERSION, 2, egl::NONE];
        let egl_context = egl_inst
            .create_context(egl_display, config, None, &ctx_attribs)
            .expect("EGL create_context failed");

        // Create wl_egl_window from the layer surface's wl_surface ObjectId
        let wl_egl = WlEglSurface::new(wl_surface.id(), width, height)
            .expect("WlEglSurface creation failed");

        let egl_surface = unsafe {
            egl_inst
                .create_window_surface(egl_display, config, wl_egl.ptr() as *mut c_void, None)
                .expect("EGL create_window_surface failed")
        };

        egl_inst
            .make_current(
                egl_display,
                Some(egl_surface),
                Some(egl_surface),
                Some(egl_context),
            )
            .expect("EGL make_current failed");

        // Disable blocking vsync; Wayland frame pacing is handled by commit timing
        let _ = egl_inst.swap_interval(egl_display, 0);

        // Load GL function pointers via glow
        let gl = unsafe {
            glow::Context::from_loader_function(|sym| match egl_inst.get_proc_address(sym) {
                Some(f) => std::mem::transmute::<extern "system" fn(), *const c_void>(f),
                None => std::ptr::null(),
            })
        };

        // Enable alpha blending for transparent background
        unsafe {
            gl.enable(glow::BLEND);
            gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
        }

        // Compile and link the shader program
        let program = unsafe {
            let vs = gl.create_shader(glow::VERTEX_SHADER).unwrap();
            gl.shader_source(vs, VERT_SRC);
            gl.compile_shader(vs);
            assert!(
                gl.get_shader_compile_status(vs),
                "vertex shader: {}",
                gl.get_shader_info_log(vs)
            );

            let fs = gl.create_shader(glow::FRAGMENT_SHADER).unwrap();
            gl.shader_source(fs, FRAG_SRC);
            gl.compile_shader(fs);
            assert!(
                gl.get_shader_compile_status(fs),
                "fragment shader: {}",
                gl.get_shader_info_log(fs)
            );

            let prog = gl.create_program().unwrap();
            gl.attach_shader(prog, vs);
            gl.attach_shader(prog, fs);
            gl.link_program(prog);
            assert!(
                gl.get_program_link_status(prog),
                "link: {}",
                gl.get_program_info_log(prog)
            );
            gl.delete_shader(vs);
            gl.delete_shader(fs);
            prog
        };

        let loc_color = unsafe { gl.get_uniform_location(program, "u_color") }.unwrap();
        let loc_size = unsafe { gl.get_uniform_location(program, "u_size") }.unwrap();
        let loc_radius = unsafe { gl.get_uniform_location(program, "u_radius") }.unwrap();
        let loc_pos = unsafe { gl.get_attrib_location(program, "a_position") }.unwrap();
        let loc_local = unsafe { gl.get_attrib_location(program, "a_local") }.unwrap();
        let vbo = unsafe { gl.create_buffer().unwrap() };

        // ── Icon overlay shader ──────────────────────────────────────────────
        let icon_prog = unsafe {
            let vs = gl.create_shader(glow::VERTEX_SHADER).unwrap();
            gl.shader_source(vs, VERT_SRC);
            gl.compile_shader(vs);
            let fs = gl.create_shader(glow::FRAGMENT_SHADER).unwrap();
            gl.shader_source(fs, ICON_FRAG_SRC);
            gl.compile_shader(fs);
            assert!(
                gl.get_shader_compile_status(fs),
                "icon FS: {}",
                gl.get_shader_info_log(fs)
            );
            let prog = gl.create_program().unwrap();
            gl.attach_shader(prog, vs);
            gl.attach_shader(prog, fs);
            gl.link_program(prog);
            assert!(
                gl.get_program_link_status(prog),
                "icon link: {}",
                gl.get_program_info_log(prog)
            );
            gl.delete_shader(vs);
            gl.delete_shader(fs);
            prog
        };
        let icon_loc_opacity = unsafe { gl.get_uniform_location(icon_prog, "u_opacity").unwrap() };

        // ── Avatar circular-clip shader ──────────────────────────────────────
        let avatar_prog = unsafe {
            let vs = gl.create_shader(glow::VERTEX_SHADER).unwrap();
            gl.shader_source(vs, VERT_SRC);
            gl.compile_shader(vs);
            let fs = gl.create_shader(glow::FRAGMENT_SHADER).unwrap();
            gl.shader_source(fs, AVATAR_FRAG_SRC);
            gl.compile_shader(fs);
            assert!(
                gl.get_shader_compile_status(fs),
                "avatar FS: {}",
                gl.get_shader_info_log(fs)
            );
            let prog = gl.create_program().unwrap();
            gl.attach_shader(prog, vs);
            gl.attach_shader(prog, fs);
            gl.link_program(prog);
            assert!(
                gl.get_program_link_status(prog),
                "avatar link: {}",
                gl.get_program_info_log(prog)
            );
            gl.delete_shader(vs);
            gl.delete_shader(fs);
            prog
        };
        let avatar_loc_opacity =
            unsafe { gl.get_uniform_location(avatar_prog, "u_opacity").unwrap() };

        // ── Upload icon textures ─────────────────────────────────────────────
        let tex_mic = unsafe { upload_texture(&gl, &icon_mic(64), 64) };
        let tex_headphone = unsafe { upload_texture(&gl, &icon_headphone(64), 64) };
        let tex_strikeout = unsafe { upload_texture(&gl, &icon_strikeout(64), 64) };

        EglContext {
            egl: egl_inst,
            egl_display,
            egl_surface,
            _egl_context: egl_context,
            wl_egl,
            gl,
            program,
            vbo,
            loc_color,
            loc_size,
            loc_radius,
            loc_pos,
            loc_local,
            icon_prog,
            icon_loc_opacity,
            tex_mic,
            tex_headphone,
            tex_strikeout,
            avatar_prog,
            avatar_loc_opacity,
        }
    }

    fn resize(&self, width: i32, height: i32) {
        self.wl_egl.resize(width, height, 0, 0);
    }

    /// Render one rounded rectangle. Coordinates are in logical pixels, origin = top-left.
    fn draw_rect(
        &self,
        px: f32,
        py: f32,
        pw: f32,
        ph: f32,
        surf_w: f32,
        surf_h: f32,
        color: [f32; 4],
        radius: f32,
    ) {
        // Convert pixel coords to NDC [-1, 1] (GL Y-up, screen Y-down → flip Y)
        let x0 = px / surf_w * 2.0 - 1.0;
        let x1 = (px + pw) / surf_w * 2.0 - 1.0;
        let y0 = 1.0 - py / surf_h * 2.0;
        let y1 = 1.0 - (py + ph) / surf_h * 2.0;

        // Triangle strip (TL, TR, BL, BR); each vertex: ndc_x, ndc_y, local_u, local_v
        let verts: [f32; 16] = [
            x0, y0, 0.0, 0.0, x1, y0, 1.0, 0.0, x0, y1, 0.0, 1.0, x1, y1, 1.0, 1.0,
        ];

        unsafe {
            let bytes = std::slice::from_raw_parts(verts.as_ptr() as *const u8, 64);
            self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            self.gl
                .buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::DYNAMIC_DRAW);

            self.gl.enable_vertex_attrib_array(self.loc_pos);
            self.gl.enable_vertex_attrib_array(self.loc_local);
            self.gl
                .vertex_attrib_pointer_f32(self.loc_pos, 2, glow::FLOAT, false, 16, 0);
            self.gl
                .vertex_attrib_pointer_f32(self.loc_local, 2, glow::FLOAT, false, 16, 8);

            self.gl.uniform_4_f32(
                Some(&self.loc_color),
                color[0],
                color[1],
                color[2],
                color[3],
            );
            self.gl.uniform_2_f32(Some(&self.loc_size), pw, ph);
            self.gl.uniform_1_f32(Some(&self.loc_radius), radius);

            self.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        }
    }

    fn swap(&self) {
        self.egl
            .swap_buffers(self.egl_display, self.egl_surface)
            .expect("EGL swap_buffers failed");
    }

    /// Render an icon texture over a quad (same coord system as draw_rect).
    fn draw_icon(
        &self,
        px: f32,
        py: f32,
        pw: f32,
        ph: f32,
        surf_w: f32,
        surf_h: f32,
        tex: glow::NativeTexture,
        opacity: f32,
    ) {
        let x0 = px / surf_w * 2.0 - 1.0;
        let x1 = (px + pw) / surf_w * 2.0 - 1.0;
        let y0 = 1.0 - py / surf_h * 2.0;
        let y1 = 1.0 - (py + ph) / surf_h * 2.0;
        let verts: [f32; 16] = [
            x0, y0, 0.0, 0.0, x1, y0, 1.0, 0.0, x0, y1, 0.0, 1.0, x1, y1, 1.0, 1.0,
        ];
        unsafe {
            self.gl.use_program(Some(self.icon_prog));
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            let bytes = std::slice::from_raw_parts(verts.as_ptr() as *const u8, 64);
            self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            self.gl
                .buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::DYNAMIC_DRAW);
            self.gl.enable_vertex_attrib_array(self.loc_pos);
            self.gl.enable_vertex_attrib_array(self.loc_local);
            self.gl
                .vertex_attrib_pointer_f32(self.loc_pos, 2, glow::FLOAT, false, 16, 0);
            self.gl
                .vertex_attrib_pointer_f32(self.loc_local, 2, glow::FLOAT, false, 16, 8);
            self.gl.uniform_1_f32(Some(&self.icon_loc_opacity), opacity);
            self.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            self.gl.use_program(Some(self.program));
        }
    }

    /// Render a circular-clipped avatar texture.
    fn draw_avatar(
        &self,
        px: f32,
        py: f32,
        size: f32,
        surf_w: f32,
        surf_h: f32,
        tex: glow::NativeTexture,
        opacity: f32,
    ) {
        let x0 = px / surf_w * 2.0 - 1.0;
        let x1 = (px + size) / surf_w * 2.0 - 1.0;
        let y0 = 1.0 - py / surf_h * 2.0;
        let y1 = 1.0 - (py + size) / surf_h * 2.0;
        let verts: [f32; 16] = [
            x0, y0, 0.0, 0.0, x1, y0, 1.0, 0.0, x0, y1, 0.0, 1.0, x1, y1, 1.0, 1.0,
        ];
        unsafe {
            self.gl.use_program(Some(self.avatar_prog));
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            let bytes = std::slice::from_raw_parts(verts.as_ptr() as *const u8, 64);
            self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            self.gl
                .buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::DYNAMIC_DRAW);
            self.gl.enable_vertex_attrib_array(self.loc_pos);
            self.gl.enable_vertex_attrib_array(self.loc_local);
            self.gl
                .vertex_attrib_pointer_f32(self.loc_pos, 2, glow::FLOAT, false, 16, 0);
            self.gl
                .vertex_attrib_pointer_f32(self.loc_local, 2, glow::FLOAT, false, 16, 8);
            self.gl
                .uniform_1_f32(Some(&self.avatar_loc_opacity), opacity);
            self.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            self.gl.use_program(Some(self.program));
        }
    }
}

/// Upload an RGBA pixel buffer as a GL TEXTURE_2D with linear filtering (non-square).
unsafe fn upload_texture_wh(
    gl: &glow::Context,
    pixels: &[u8],
    w: u32,
    h: u32,
) -> glow::NativeTexture {
    let tex = gl.create_texture().unwrap();
    gl.bind_texture(glow::TEXTURE_2D, Some(tex));
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_WRAP_S,
        glow::CLAMP_TO_EDGE as i32,
    );
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_WRAP_T,
        glow::CLAMP_TO_EDGE as i32,
    );
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_MIN_FILTER,
        glow::LINEAR as i32,
    );
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_MAG_FILTER,
        glow::LINEAR as i32,
    );
    gl.tex_image_2d(
        glow::TEXTURE_2D,
        0,
        glow::RGBA as i32,
        w as i32,
        h as i32,
        0,
        glow::RGBA,
        glow::UNSIGNED_BYTE,
        Some(pixels),
    );
    tex
}

/// Upload a square RGBA pixel buffer.
unsafe fn upload_texture(gl: &glow::Context, pixels: &[u8], size: u32) -> glow::NativeTexture {
    upload_texture_wh(gl, pixels, size, size)
}

// ─── Text rendering helpers ───────────────────────────────────────────────────

fn render_text_texture(font: &fontdue::Font, text: &str, px_size: f32) -> (Vec<u8>, u32, u32) {
    use fontdue::layout::{CoordinateSystem, Layout, LayoutSettings, TextStyle};
    let mut layout = Layout::new(CoordinateSystem::PositiveYDown);
    layout.reset(&LayoutSettings {
        x: 0.0,
        y: 0.0,
        max_width: Some(240.0),
        ..Default::default()
    });
    layout.append(&[font], &TextStyle::new(text, px_size, 0));
    let glyphs = layout.glyphs();
    if glyphs.is_empty() {
        return (vec![255u8; 4], 1, 1);
    }
    let text_w = glyphs
        .iter()
        .map(|g| (g.x + g.width as f32) as u32)
        .max()
        .unwrap_or(1)
        .max(1);
    let text_h = (px_size * 1.4).ceil() as u32;
    let mut pixels = vec![0u8; (text_w * text_h * 4) as usize];
    for g in glyphs {
        if g.char_data.is_whitespace() {
            continue;
        }
        let (metrics, bitmap) = font.rasterize(g.parent, px_size);
        for (i, &v) in bitmap.iter().enumerate() {
            let gx = g.x as i32 + (i % metrics.width) as i32;
            let gy = g.y as i32 + (i / metrics.width) as i32;
            if gx >= 0 && gy >= 0 && (gx as u32) < text_w && (gy as u32) < text_h {
                let idx = ((gy as u32 * text_w + gx as u32) * 4) as usize;
                pixels[idx] = 255;
                pixels[idx + 1] = 255;
                pixels[idx + 2] = 255;
                pixels[idx + 3] = v;
            }
        }
    }
    // Flip rows vertically so the buffer matches OpenGL's bottom-up V=0 convention,
    // which cancels out the `1.0 - v_local.y` Y-flip in the icon fragment shader.
    let row_bytes = (text_w * 4) as usize;
    for row in 0..text_h / 2 {
        let a = (row * text_w * 4) as usize;
        let b = ((text_h - 1 - row) * text_w * 4) as usize;
        for col in 0..row_bytes {
            pixels.swap(a + col, b + col);
        }
    }
    (pixels, text_w, text_h)
}

fn load_system_font() -> Option<fontdue::Font> {
    let paths = [
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/Hack-Regular.ttf",
    ];
    for p in &paths {
        if let Ok(data) = std::fs::read(p) {
            if let Ok(font) =
                fontdue::Font::from_bytes(data.as_slice(), fontdue::FontSettings::default())
            {
                eprintln!("Loaded font: {p}");
                return Some(font);
            }
        }
    }
    None
}

// ─── Participant state ────────────────────────────────────────────────────────

struct ParticipantState {
    user_id: String,
    display_name: String,
    muted: bool,
    deafened: bool,
    speaking_until: Option<std::time::Instant>,
    /// 0.0 = invisible (enter start / leave end), 1.0 = fully visible.
    anim: f32,
    /// True when this participant is animating out before removal.
    leaving: bool,
}

// ─── App ─────────────────────────────────────────────────────────────────────

fn main() {
    env_logger::init();
    println!("Starting hypr-overlay-wl (EGL/GLES2)");

    let conn = Connection::connect_to_env().expect("Wayland connection failed");
    let (globals, event_queue) = registry_queue_init(&conn).expect("registry init failed");
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor not available");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("layer shell not available");

    let surface = compositor.create_surface(&qh);
    let layer =
        layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("hypr_overlay"), None);

    layer.set_anchor(Anchor::TOP | Anchor::RIGHT);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer.set_size(360, 64);
    layer.set_exclusive_zone(-1);
    layer.commit();

    let egl_ctx = EglContext::new(&conn, layer.wl_surface(), 360, 64);

    // Set up calloop event loop — integrates Wayland + Discord channel
    let mut event_loop: EventLoop<'static, App> = EventLoop::try_new().unwrap();
    let loop_handle = event_loop.handle();

    WaylandSource::new(conn, event_queue)
        .insert(loop_handle.clone())
        .unwrap();

    // Optional Discord IPC — enabled when both env vars are set
    let (discord_ev_tx, discord_ev_channel) = calloop::channel::channel::<discord::DiscordEvent>();
    loop_handle
        .insert_source(discord_ev_channel, |event, _, app| {
            if let calloop::channel::Event::Msg(ev) = event {
                if app.handle_discord_event(ev) {
                    app.draw();
                }
            }
        })
        .unwrap();

    // Combined animation + speaking-expiry tick
    loop_handle
        .insert_source(
            calloop::timer::Timer::from_duration(std::time::Duration::from_millis(16)),
            |_, _, app| {
                let now = std::time::Instant::now();
                let mut needs_redraw = false;

                // Animate participant rows (~180ms to fully appear/disappear)
                let anim_speed = 0.016_f32 / 0.18;
                let mut to_remove: Vec<String> = Vec::new();
                for p in &mut app.participants {
                    if p.leaving {
                        p.anim = (p.anim - anim_speed).max(0.0);
                        needs_redraw = true;
                        if p.anim <= 0.0 {
                            to_remove.push(p.user_id.clone());
                        }
                    } else if p.anim < 1.0 {
                        p.anim = (p.anim + anim_speed).min(1.0);
                        needs_redraw = true;
                    }
                }
                // Remove fully-animated-out participants and clean up textures
                for uid in &to_remove {
                    if let Some(pos) = app.participants.iter().position(|p| &p.user_id == uid) {
                        let name = app.participants[pos].display_name.clone();
                        eprintln!("[overlay] {name} removed from list");
                        app.participants.remove(pos);
                        if let Some((tex, _, _)) = app.name_textures.remove(uid) {
                            unsafe {
                                app.egl.gl.delete_texture(tex);
                            }
                        }
                        if let Some(tex) = app.avatar_textures.remove(uid) {
                            unsafe {
                                app.egl.gl.delete_texture(tex);
                            }
                        }
                    }
                }
                if !to_remove.is_empty() {
                    // Clamp scroll offset when participants are removed
                    let max_offset = app.participants.len().saturating_sub(app.max_visible_rows);
                    app.scroll_offset = app.scroll_offset.min(max_offset);
                    let extra = if app.participants.len() > app.max_visible_rows { 20 } else { 0 };
                    let new_h = 64 + app.visible_row_count() as u32 * 48 + extra;
                    app.resize_overlay(new_h);
                    needs_redraw = true;
                }

                // Expire speaking rings
                let any_expired = app
                    .participants
                    .iter()
                    .any(|p| p.speaking_until.map(|t| t <= now).unwrap_or(false));
                if any_expired {
                    for p in &mut app.participants {
                        if p.speaking_until.map(|t| t <= now).unwrap_or(false) {
                            p.speaking_until = None;
                        }
                    }
                    needs_redraw = true;
                }

                // Animate idle alpha (~250ms transition)
                let target_alpha = if app.in_channel { 1.0_f32 } else { 0.3_f32 };
                if (app.idle_alpha - target_alpha).abs() > 0.005 {
                    let speed = 0.016 / 0.25;
                    if app.idle_alpha < target_alpha {
                        app.idle_alpha = (app.idle_alpha + speed).min(target_alpha);
                    } else {
                        app.idle_alpha = (app.idle_alpha - speed).max(target_alpha);
                    }
                    needs_redraw = true;
                }

                // Update session timer texture every second
                if let Some(joined_at) = app.channel_joined_at {
                    let elapsed = joined_at.elapsed().as_secs() as u32;
                    if elapsed != app.last_timer_secs {
                        app.last_timer_secs = elapsed;
                        let h = elapsed / 3600;
                        let m = (elapsed % 3600) / 60;
                        let s = elapsed % 60;
                        let label = if h > 0 {
                            format!("{h}:{m:02}:{s:02}")
                        } else {
                            format!("{m}:{s:02}")
                        };
                        if let Some((tex, _, _)) = app.timer_tex.take() {
                            unsafe { app.egl.gl.delete_texture(tex); }
                        }
                        let new_tex = app.render_text_tex(&label, 13.0);
                        app.timer_tex = new_tex;
                        needs_redraw = true;
                    }
                }

                if needs_redraw {
                    app.draw();
                }

                // Run at 16ms when animating, 500ms when idle or just tracking timer
                let target_alpha = if app.in_channel { 1.0_f32 } else { 0.3_f32 };
                let animating = app.participants.iter().any(|p| p.anim < 1.0 || p.leaving)
                    || (app.idle_alpha - target_alpha).abs() > 0.005;
                let next = if animating {
                    std::time::Duration::from_millis(16)
                } else {
                    // 500ms is fine for the session timer (≤500ms display lag per second)
                    std::time::Duration::from_millis(500)
                };
                calloop::timer::TimeoutAction::ToDuration(next)
            },
        )
        .unwrap();

    let discord_cmd_tx = match (
        std::env::var("DISCORD_CLIENT_ID"),
        std::env::var("DISCORD_CLIENT_SECRET"),
    ) {
        (Ok(id), Ok(secret)) => {
            let (tx, rx) = mpsc::sync_channel(32);
            discord::spawn(
                discord::Config {
                    client_id: id,
                    client_secret: secret,
                },
                discord_ev_tx,
                rx,
            );
            println!("Discord IPC enabled — waiting for connection...");
            Some(tx)
        }
        _ => {
            println!(
                "Discord IPC disabled (set DISCORD_CLIENT_ID + DISCORD_CLIENT_SECRET to enable)"
            );
            None
        }
    };

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        layer,
        egl: egl_ctx,
        width: 360,
        height: 64,
        dragging: false,
        last_pointer: (0, 0),
        drag_base_pos: (0, 0),
        drag_output: None,
        margins: (12, 12, 0, 0),
        anchor: Anchor::TOP | Anchor::RIGHT,
        modifiers: Modifiers::default(),
        exit: false,
        discord_cmd_tx,
        discord_mute: false,
        discord_deaf: false,
        opacity: std::env::var("OVERLAY_OPACITY")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(0.9)
            .clamp(0.1, 1.0),
        participants: vec![],
        avatar_textures: std::collections::HashMap::new(),
        name_textures: std::collections::HashMap::new(),
        font: load_system_font(),
        channel_name: None,
        channel_name_tex: None,
        in_channel: false,
        idle_alpha: 0.3,
        channel_joined_at: None,
        timer_tex: None,
        last_timer_secs: u32::MAX,
        scroll_offset: 0,
        max_visible_rows: 5,
        scroll_indicator_tex: None,
        last_scroll_state: (usize::MAX, usize::MAX),
        last_pointer_y: 0.0,
    };

    let loop_signal = event_loop.get_signal();
    event_loop
        .run(None, &mut app, move |app| {
            if app.exit {
                loop_signal.stop();
            }
        })
        .expect("event loop error");

    println!("hypr-overlay exiting");
}

struct App {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    layer: LayerSurface,
    egl: EglContext,
    width: u32,
    height: u32,
    dragging: bool,
    last_pointer: (i32, i32),
    drag_base_pos: (i32, i32),
    drag_output: Option<wl_output::WlOutput>,
    margins: (i32, i32, i32, i32),
    anchor: Anchor,
    modifiers: Modifiers,
    exit: bool,
    // Discord state
    discord_cmd_tx: Option<mpsc::SyncSender<discord::DiscordCommand>>,
    discord_mute: bool,
    discord_deaf: bool,
    // Overlay opacity (0.1 – 1.0), adjustable with scroll wheel
    opacity: f32,
    // Voice participants
    participants: Vec<ParticipantState>,
    avatar_textures: std::collections::HashMap<String, glow::NativeTexture>,
    name_textures: std::collections::HashMap<String, (glow::NativeTexture, u32, u32)>,
    font: Option<fontdue::Font>,
    // Channel name display
    channel_name: Option<String>,
    channel_name_tex: Option<(glow::NativeTexture, u32, u32)>,
    // Idle fade state
    in_channel: bool,
    idle_alpha: f32,
    // Session duration timer
    channel_joined_at: Option<std::time::Instant>,
    timer_tex: Option<(glow::NativeTexture, u32, u32)>,
    last_timer_secs: u32,
    // Scrollable participant list
    scroll_offset: usize,
    max_visible_rows: usize,
    scroll_indicator_tex: Option<(glow::NativeTexture, u32, u32)>,
    last_scroll_state: (usize, usize),
    // Pointer Y position for scroll vs opacity decision
    last_pointer_y: f64,
}

// ─── Trait implementations ───────────────────────────────────────────────────

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: u32) {}
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = true;
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let (w, h) = configure.new_size;
        if w != 0 {
            self.width = NonZeroU32::new(w).map_or(self.width, |v| v.get());
        }
        if h != 0 {
            self.height = NonZeroU32::new(h).map_or(self.height, |v| v.get());
        }
        self.egl.resize(self.width as i32, self.height as i32);
        self.draw();
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            let _ = self.seat_state.get_pointer(qh, &seat);
        }
        if capability == Capability::Keyboard {
            let _ = self.seat_state.get_keyboard::<_, App>(qh, &seat, None);
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        _: Capability,
    ) {
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        use PointerEventKind::*;
        for event in events {
            if event.surface.id() != self.layer.wl_surface().id() {
                continue;
            }
            match event.kind {
                Enter { .. } | Leave { .. } => {
                    if let Enter { .. } = event.kind {
                        self.last_pointer_y = event.position.1;
                    }
                }

                Motion { .. } => {
                    self.last_pointer_y = event.position.1;
                    if self.dragging {
                        let (x, y) = (event.position.0 as i32, event.position.1 as i32);

                        // Reconstruct absolute pointer position from expected surface pos +
                        // surface-local event coordinates, then subtract grab offset.
                        let ptr_screen_x = self.drag_base_pos.0 + x;
                        let ptr_screen_y = self.drag_base_pos.1 + y;
                        let new_x = ptr_screen_x - self.last_pointer.0;
                        let new_y = ptr_screen_y - self.last_pointer.1;
                        self.drag_base_pos = (new_x, new_y);

                        if let Some(out) = self.drag_output.clone() {
                            if let Some(info) = self.output_state.info(&out) {
                                let out_pos = info.logical_position.unwrap_or((0, 0));
                                let out_scale = info.scale_factor.max(1);
                                let left = (new_x - out_pos.0 * out_scale) / out_scale;
                                let top = (new_y - out_pos.1 * out_scale) / out_scale;
                                self.layer.set_anchor(Anchor::TOP | Anchor::LEFT);
                                self.layer.set_margin(top, 0, 0, left);
                                self.draw(); // egl.swap() commits margin changes + buffer
                            }
                        }
                    }
                }

                Press { button, .. } => {
                    let (x, y) = (event.position.0 as i32, event.position.1 as i32);

                    let (bx, by, bw, bh) = button_rects(self.width, 64);
                    if x >= bx && x < bx + bw && y >= by && y < by + bh {
                        // Deafen button
                        if let Some(ref tx) = self.discord_cmd_tx {
                            let _ = tx.send(discord::DiscordCommand::SetDeaf(!self.discord_deaf));
                        }
                    }
                    let (bx2, by2, bw2, bh2) = button2_rects(self.width, 64);
                    if x >= bx2 && x < bx2 + bw2 && y >= by2 && y < by2 + bh2 {
                        // Mute button
                        if let Some(ref tx) = self.discord_cmd_tx {
                            let _ = tx.send(discord::DiscordCommand::SetMute(!self.discord_mute));
                        }
                    }

                    let (hx, hy, hw, hh) = drag_handle_rects(self.width, 64);
                    if button == BTN_LEFT && x >= hx && x < hx + hw && y >= hy && y < hy + hh {
                        // Compute absolute surface position for drag reference
                        let outputs: Vec<_> = self
                            .layer
                            .wl_surface()
                            .data::<sctk::compositor::SurfaceData>()
                            .map(|d| d.outputs().collect())
                            .unwrap_or_default();
                        if let Some(output) = outputs.first() {
                            let out = output.clone();
                            if let Some(info) = self.output_state.info(&out) {
                                let out_pos = info.logical_position.unwrap_or((0, 0));
                                let out_size = info.logical_size.unwrap_or((1920, 1080));
                                let out_scale = info.scale_factor;

                                let (top, right, bottom, left) = self.margins;
                                let surf_scale =
                                    self.layer
                                        .wl_surface()
                                        .data::<sctk::compositor::SurfaceData>()
                                        .map(|d| d.scale_factor())
                                        .unwrap_or(1) as i32;
                                let surf_w = self.width as i32 * surf_scale;
                                let surf_h = self.height as i32 * surf_scale;

                                let out_pos_px = (out_pos.0 * out_scale, out_pos.1 * out_scale);
                                let out_size_px = (out_size.0 * out_scale, out_size.1 * out_scale);

                                let abs_left = if self.anchor.contains(Anchor::RIGHT) {
                                    out_pos_px.0 + out_size_px.0 - right - surf_w
                                } else {
                                    out_pos_px.0 + left
                                };
                                let abs_top = if self.anchor.contains(Anchor::BOTTOM) {
                                    out_pos_px.1 + out_size_px.1 - bottom - surf_h
                                } else {
                                    out_pos_px.1 + top
                                };

                                self.anchor = Anchor::TOP | Anchor::LEFT;
                                self.layer.set_anchor(self.anchor);
                                let top_m = (abs_top - out_pos_px.1) / out_scale;
                                let left_m = (abs_left - out_pos_px.0) / out_scale;
                                self.layer.set_margin(top_m, 0, 0, left_m);
                                self.drag_base_pos = (abs_left, abs_top);
                                self.drag_output = Some(out);
                            }
                        }
                        self.dragging = true;
                        self.last_pointer = (x, y);
                        self.draw();
                    }
                }

                Release { button, .. } => {
                    if button == BTN_LEFT {
                        self.dragging = false;
                    }
                }
                Axis { vertical, .. } => {
                    let delta = -vertical.absolute as f32 * 0.02;
                    if self.last_pointer_y > 64.0 && !self.participants.is_empty() {
                        // Scroll participant list: wheel-down increases offset (show further down)
                        if delta < 0.0 {
                            let max_offset = self.participants.len().saturating_sub(self.max_visible_rows);
                            self.scroll_offset = (self.scroll_offset + 1).min(max_offset);
                        } else {
                            self.scroll_offset = self.scroll_offset.saturating_sub(1);
                        }
                    } else {
                        // Control bar: adjust overlay opacity
                        self.opacity = (self.opacity + delta).clamp(0.1, 1.0);
                    }
                    self.draw();
                }
            }
        }
    }
}

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &sctk::reexports::client::protocol::wl_keyboard::WlKeyboard,
        _: &WlSurface,
        _: u32,
        _: &[u32],
        _: &[sctk::seat::keyboard::Keysym],
    ) {
    }
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &sctk::reexports::client::protocol::wl_keyboard::WlKeyboard,
        _: &WlSurface,
        _: u32,
    ) {
    }
    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &sctk::reexports::client::protocol::wl_keyboard::WlKeyboard,
        _: u32,
        _: sctk::seat::keyboard::KeyEvent,
    ) {
    }
    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &sctk::reexports::client::protocol::wl_keyboard::WlKeyboard,
        _: u32,
        _: sctk::seat::keyboard::KeyEvent,
    ) {
    }
    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &sctk::reexports::client::protocol::wl_keyboard::WlKeyboard,
        _: u32,
        _: sctk::seat::keyboard::KeyEvent,
    ) {
    }
    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &sctk::reexports::client::protocol::wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: sctk::seat::keyboard::Modifiers,
        _: sctk::seat::keyboard::RawModifiers,
        _: u32,
    ) {
        self.modifiers = modifiers;
    }
    fn update_repeat_info(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &sctk::reexports::client::protocol::wl_keyboard::WlKeyboard,
        _: sctk::seat::keyboard::RepeatInfo,
    ) {
    }
}

// ─── Delegate macros ─────────────────────────────────────────────────────────

delegate_compositor!(App);
delegate_output!(App);
delegate_seat!(App);
delegate_pointer!(App);
delegate_layer!(App);
delegate_registry!(App);
delegate_keyboard!(App);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

// ─── Layout helpers ──────────────────────────────────────────────────────────

fn button_rects(w: u32, h: u32) -> (i32, i32, i32, i32) {
    let bw = 64;
    let bh = (h as i32 - 16).max(1);
    (w as i32 - bw - 8, 8, bw, bh)
}
fn button2_rects(w: u32, h: u32) -> (i32, i32, i32, i32) {
    let bw = 64;
    let bh = (h as i32 - 16).max(1);
    (w as i32 - bw - 8 - bw - 8, 8, bw, bh)
}
fn drag_handle_rects(_w: u32, h: u32) -> (i32, i32, i32, i32) {
    (8, 8, 24, (h as i32 - 16).max(1))
}

// ─── Rendering ───────────────────────────────────────────────────────────────

impl App {
    fn visible_row_count(&self) -> usize {
        self.participants.len().min(self.max_visible_rows)
    }

    /// Rasterise `text` at `px_size` and upload it as a GL texture.
    fn render_text_tex(&self, text: &str, px_size: f32) -> Option<(glow::NativeTexture, u32, u32)> {
        let font = self.font.as_ref()?;
        let (pixels, w, h) = render_text_texture(font, text, px_size);
        if w > 0 && h > 0 {
            let tex = unsafe { upload_texture_wh(&self.egl.gl, &pixels, w, h) };
            Some((tex, w, h))
        } else {
            None
        }
    }

    fn resize_overlay(&mut self, new_h: u32) {
        if self.height == new_h {
            return;
        }
        eprintln!(
            "[overlay] resize {} → {} px tall ({} participant rows)",
            self.height,
            new_h,
            self.participants.len()
        );
        self.height = new_h;
        self.egl.resize(self.width as i32, new_h as i32);
        self.layer.set_size(self.width, new_h);
    }

    fn make_name_texture(&mut self, user_id: &str, name: &str) {
        if let Some(font) = &self.font {
            let (pixels, w, h) = render_text_texture(font, name, 16.0);
            if w > 0 && h > 0 {
                let tex = unsafe { upload_texture_wh(&self.egl.gl, &pixels, w, h) };
                self.name_textures.insert(user_id.to_string(), (tex, w, h));
            }
        }
    }

    fn handle_discord_event(&mut self, event: discord::DiscordEvent) -> bool {
        match event {
            discord::DiscordEvent::Ready { username } => {
                println!("Discord connected as {username}");
                false
            }
            discord::DiscordEvent::VoiceSettings { mute, deaf } => {
                let changed = self.discord_mute != mute || self.discord_deaf != deaf;
                self.discord_mute = mute;
                self.discord_deaf = deaf;
                changed
            }
            discord::DiscordEvent::VoiceParticipants {
                participants: parts,
                channel_name,
            } => {
                // Update in_channel state and channel name texture
                let was_in_channel = self.in_channel;
                self.in_channel = channel_name.is_some();
                // Start session timer when newly joining a channel
                if self.in_channel && !was_in_channel {
                    self.channel_joined_at = Some(std::time::Instant::now());
                    self.last_timer_secs = u32::MAX; // force texture regeneration
                } else if !self.in_channel {
                    self.channel_joined_at = None;
                    if let Some((tex, _, _)) = self.timer_tex.take() {
                        unsafe { self.egl.gl.delete_texture(tex); }
                    }
                }
                if self.channel_name != channel_name {
                    if let Some((tex, _, _)) = self.channel_name_tex.take() {
                        unsafe {
                            self.egl.gl.delete_texture(tex);
                        }
                    }
                    self.channel_name = channel_name.clone();
                    if let Some(ref name) = channel_name {
                        if let Some(font) = &self.font {
                            let (pixels, w, h) = render_text_texture(font, name, 14.0);
                            if w > 0 && h > 0 {
                                let tex = unsafe { upload_texture_wh(&self.egl.gl, &pixels, w, h) };
                                self.channel_name_tex = Some((tex, w, h));
                            }
                        }
                    }
                }
                for (_, (tex, _, _)) in self.name_textures.drain() {
                    unsafe {
                        self.egl.gl.delete_texture(tex);
                    }
                }
                self.participants = parts
                    .iter()
                    .map(|p| ParticipantState {
                        user_id: p.user_id.clone(),
                        display_name: p.nick.clone().unwrap_or_else(|| p.username.clone()),
                        muted: p.muted,
                        deafened: p.deafened,
                        speaking_until: None,
                        anim: 1.0, // already visible on initial load
                        leaving: false,
                    })
                    .collect();
                let user_names: Vec<(String, String)> = self
                    .participants
                    .iter()
                    .map(|p| (p.user_id.clone(), p.display_name.clone()))
                    .collect();
                for (uid, name) in user_names {
                    self.make_name_texture(&uid, &name);
                }
                // Reset scroll when participant list is fully replaced
                self.scroll_offset = 0;
                if let Some((tex, _, _)) = self.scroll_indicator_tex.take() {
                    unsafe { self.egl.gl.delete_texture(tex); }
                }
                self.last_scroll_state = (usize::MAX, usize::MAX);
                let extra = if self.participants.len() > self.max_visible_rows { 20 } else { 0 };
                let new_h = 64 + self.visible_row_count() as u32 * 48 + extra;
                self.resize_overlay(new_h);
                true
            }
            discord::DiscordEvent::UserJoined(p) => {
                // Ignore if already in list (e.g. duplicate event)
                if self.participants.iter().any(|e| e.user_id == p.user_id) {
                    return false;
                }
                eprintln!(
                    "[overlay] {} joined the channel",
                    p.nick.as_deref().unwrap_or(&p.username)
                );
                let uid = p.user_id.clone();
                let name = p.nick.clone().unwrap_or_else(|| p.username.clone());
                self.participants.push(ParticipantState {
                    user_id: uid.clone(),
                    display_name: name.clone(),
                    muted: p.muted,
                    deafened: p.deafened,
                    speaking_until: None,
                    anim: 0.0, // start invisible, animate in
                    leaving: false,
                });
                self.make_name_texture(&uid, &name);
                let extra = if self.participants.len() > self.max_visible_rows { 20 } else { 0 };
                let new_h = 64 + self.visible_row_count() as u32 * 48 + extra;
                self.resize_overlay(new_h);
                true
            }
            discord::DiscordEvent::UserLeft { user_id } => {
                if let Some(p) = self
                    .participants
                    .iter_mut()
                    .find(|p| p.user_id == user_id && !p.leaving)
                {
                    eprintln!(
                        "[overlay] {} leaving channel (animating out)",
                        p.display_name
                    );
                    p.leaving = true;
                    return true; // trigger a redraw to start the animation
                }
                false
            }
            discord::DiscordEvent::ParticipantStateUpdate {
                user_id,
                muted,
                deafened,
            } => {
                if let Some(p) = self.participants.iter_mut().find(|p| p.user_id == user_id) {
                    let changed = p.muted != muted || p.deafened != deafened;
                    p.muted = muted;
                    p.deafened = deafened;
                    return changed;
                }
                false
            }
            discord::DiscordEvent::SpeakingUpdate { user_id, speaking } => {
                if let Some(p) = self.participants.iter_mut().find(|p| p.user_id == user_id) {
                    p.speaking_until = if speaking {
                        // Ring stays for 1.5s after last SPEAKING_START.
                        // Discord fires SPEAKING_START ~every 1s while active,
                        // so the ring clears ~0.5s after the user goes quiet.
                        Some(std::time::Instant::now() + std::time::Duration::from_millis(1500))
                    } else {
                        None
                    };
                    return true;
                }
                false
            }
            discord::DiscordEvent::AvatarLoaded {
                user_id,
                rgba,
                size,
            } => {
                let tex = unsafe { upload_texture_wh(&self.egl.gl, &rgba, size, size) };
                self.avatar_textures.insert(user_id, tex);
                true
            }
            discord::DiscordEvent::Disconnected => {
                // Clear all voice-channel state so the UI resets to idle.
                self.in_channel = false;
                self.channel_name = None;
                if let Some((tex, _, _)) = self.channel_name_tex.take() {
                    unsafe {
                        self.egl.gl.delete_texture(tex);
                    }
                }
                // Clear session timer
                self.channel_joined_at = None;
                if let Some((tex, _, _)) = self.timer_tex.take() {
                    unsafe { self.egl.gl.delete_texture(tex); }
                }
                // Clear scroll indicator
                if let Some((tex, _, _)) = self.scroll_indicator_tex.take() {
                    unsafe { self.egl.gl.delete_texture(tex); }
                }
                self.scroll_offset = 0;
                self.last_scroll_state = (usize::MAX, usize::MAX);
                for (_, tex) in self.avatar_textures.drain() {
                    unsafe {
                        self.egl.gl.delete_texture(tex);
                    }
                }
                for (_, (tex, _, _)) in self.name_textures.drain() {
                    unsafe {
                        self.egl.gl.delete_texture(tex);
                    }
                }
                self.participants.clear();
                self.discord_mute = false;
                self.discord_deaf = false;
                self.idle_alpha = 0.3;
                self.resize_overlay(64);
                true
            }
        }
    }

    fn draw(&mut self) {
        let op = self.opacity * self.idle_alpha;
        let (sw, sh) = (self.width as f32, self.height as f32);
        let (bx, by, bw, bh) = button_rects(self.width, 64);
        let (bx2, by2, bw2, bh2) = button2_rects(self.width, 64);
        let (hx, hy, hw, hh) = drag_handle_rects(self.width, 64);

        unsafe {
            self.egl
                .gl
                .viewport(0, 0, self.width as i32, self.height as i32);
            self.egl.gl.clear_color(0.0, 0.0, 0.0, 0.0);
            self.egl.gl.clear(glow::COLOR_BUFFER_BIT);
            self.egl.gl.use_program(Some(self.egl.program));
        }

        // Drag handle — blue pill
        self.egl.draw_rect(
            hx as f32,
            hy as f32,
            hw as f32,
            hh as f32,
            sw,
            sh,
            [0.25, 0.45, 1.0, 0.75 * op],
            (hw as f32 * 0.5).min(10.0),
        );

        // Channel name — rendered between drag handle and mute button
        // When a timer is also shown, push name to upper half of the bar.
        if let Some((tex, tw, th)) = self.channel_name_tex {
            let name_x = (hx + hw) as f32 + 8.0;
            let name_y = if self.timer_tex.is_some() {
                8.0
            } else {
                (64.0 - th as f32) * 0.5
            };
            let max_w = (bx2 as f32 - name_x - 8.0).max(0.0);
            let draw_w = (tw as f32).min(max_w);
            if draw_w > 0.0 {
                self.egl
                    .draw_icon(name_x, name_y, draw_w, th as f32, sw, sh, tex, op);
            }
        }

        // Session duration timer
        if let Some((tex, tw, th)) = self.timer_tex {
            let name_x = (hx + hw) as f32 + 8.0;
            let max_w = (bx2 as f32 - name_x - 8.0).max(0.0);
            let draw_w = (tw as f32).min(max_w);
            let timer_y = if self.channel_name_tex.is_some() {
                8.0 + 22.0 // below channel name (channel name is ~20px tall at 14px font)
            } else {
                (64.0 - th as f32) * 0.5
            };
            if draw_w > 0.0 {
                self.egl
                    .draw_icon(name_x, timer_y, draw_w, th as f32, sw, sh, tex, op * 0.7);
            }
        }

        // Mute button background
        // When deafened, mic is implicitly muted too
        let effectively_muted = self.discord_mute || self.discord_deaf;
        let mute_base = if effectively_muted {
            [0.75, 0.15, 0.15, 0.88 * op]
        } else {
            [0.12, 0.68, 0.28, 0.82 * op]
        };
        self.egl.draw_rect(
            bx2 as f32, by2 as f32, bw2 as f32, bh2 as f32, sw, sh, mute_base, 10.0,
        );

        // Deafen button background
        let deaf_base = if self.discord_deaf {
            [0.75, 0.15, 0.15, 0.88 * op]
        } else {
            [0.18, 0.36, 0.82, 0.82 * op]
        };
        self.egl.draw_rect(
            bx as f32, by as f32, bw as f32, bh as f32, sw, sh, deaf_base, 10.0,
        );

        // Icon overlays — mic on mute button, headphone on deafen button
        let pad = 6.0f32;
        let mic_tex = self.egl.tex_mic;
        let hp_tex = self.egl.tex_headphone;
        let strike_tex = self.egl.tex_strikeout;
        self.egl.draw_icon(
            bx2 as f32 + pad,
            by2 as f32 + pad,
            bw2 as f32 - 2.0 * pad,
            bh2 as f32 - 2.0 * pad,
            sw,
            sh,
            mic_tex,
            op,
        );
        if effectively_muted {
            self.egl.draw_icon(
                bx2 as f32 + pad,
                by2 as f32 + pad,
                bw2 as f32 - 2.0 * pad,
                bh2 as f32 - 2.0 * pad,
                sw,
                sh,
                strike_tex,
                op,
            );
        }
        self.egl.draw_icon(
            bx as f32 + pad,
            by as f32 + pad,
            bw as f32 - 2.0 * pad,
            bh as f32 - 2.0 * pad,
            sw,
            sh,
            hp_tex,
            op,
        );
        if self.discord_deaf {
            self.egl.draw_icon(
                bx as f32 + pad,
                by as f32 + pad,
                bw as f32 - 2.0 * pad,
                bh as f32 - 2.0 * pad,
                sw,
                sh,
                strike_tex,
                op,
            );
        }

        // Restrict input to interactive areas only (rest is click-through)
        let region = Region::new(&self.compositor).expect("create region");
        region.add(bx, by, bw, bh);
        region.add(bx2, by2, bw2, bh2);
        region.add(hx, hy, hw, hh);
        self.layer.set_input_region(Some(region.wl_region()));

        // ── Participant rows (below the 64px control bar) ─────────────────────
        let row_h = 48u32;

        // Update scroll indicator texture if the scroll state changed
        let scroll_state = (self.scroll_offset, self.participants.len());
        if self.participants.len() > self.max_visible_rows {
            if scroll_state != self.last_scroll_state {
                self.last_scroll_state = scroll_state;
                if let Some((tex, _, _)) = self.scroll_indicator_tex.take() {
                    unsafe { self.egl.gl.delete_texture(tex); }
                }
                let above = self.scroll_offset;
                let below = self.participants.len()
                    .saturating_sub(self.scroll_offset + self.max_visible_rows);
                let label = match (above > 0, below > 0) {
                    (true, true)  => format!("↑{}  ↓{}", above, below),
                    (true, false) => format!("↑{} more above", above),
                    (false, true) => format!("↓{} more below", below),
                    _             => String::new(),
                };
                if !label.is_empty() {
                    let new_tex = self.render_text_tex(&label, 11.0);
                    self.scroll_indicator_tex = new_tex;
                }
            }
        } else if self.scroll_indicator_tex.is_some() {
            if let Some((tex, _, _)) = self.scroll_indicator_tex.take() {
                unsafe { self.egl.gl.delete_texture(tex); }
            }
            self.last_scroll_state = (usize::MAX, usize::MAX);
        }

        // Collect visible participant data to avoid re-borrowing self in the loop
        let visible: Vec<(usize, f32, bool, bool, bool, String)> = self
            .participants
            .iter()
            .enumerate()
            .skip(self.scroll_offset)
            .take(self.max_visible_rows)
            .map(|(abs_idx, p)| {
                let speaking = p
                    .speaking_until
                    .map(|t| t > std::time::Instant::now())
                    .unwrap_or(false);
                (abs_idx, p.anim, p.muted, p.deafened, speaking, p.user_id.clone())
            })
            .collect();

        for (slot, (abs_idx, anim, muted, deafened, speaking, user_id)) in
            visible.iter().enumerate()
        {
            let _ = abs_idx; // abs_idx available for future use; slot drives layout
            let slide_offset = (1.0 - anim) * row_h as f32 * 0.35;
            let row_y_f = 64.0_f32 + slot as f32 * row_h as f32 + slide_offset;
            let row_anim_op = op * anim;

            let av_size = 32f32;
            let av_x = 12f32;
            let av_y = row_y_f + (row_h as f32 - av_size) * 0.5;

            // Semi-transparent row background
            self.egl.draw_rect(
                4.0,
                row_y_f + 4.0,
                sw - 8.0,
                row_h as f32 - 8.0,
                sw,
                sh,
                [0.1, 0.1, 0.12, 0.6 * row_anim_op],
                8.0,
            );

            // Speaking ring — active while speaking_until is in the future
            if *speaking {
                let ring = 3.0f32;
                self.egl.draw_rect(
                    av_x - ring,
                    av_y - ring,
                    av_size + ring * 2.0,
                    av_size + ring * 2.0,
                    sw,
                    sh,
                    [0.2, 0.85, 0.35, 0.9 * row_anim_op],
                    (av_size + ring * 2.0) * 0.5,
                );
            }

            // Avatar
            if let Some(&tex) = self.avatar_textures.get(user_id) {
                self.egl
                    .draw_avatar(av_x, av_y, av_size, sw, sh, tex, row_anim_op);
            } else {
                let hue = user_id.bytes().fold(0u32, |a, b| a.wrapping_add(b as u32));
                let colors = [
                    [0.6f32, 0.2, 0.8],
                    [0.2, 0.6, 0.8],
                    [0.8, 0.4, 0.2],
                    [0.3, 0.7, 0.4],
                ];
                let c = colors[(hue % 4) as usize];
                self.egl.draw_rect(
                    av_x,
                    av_y,
                    av_size,
                    av_size,
                    sw,
                    sh,
                    [c[0], c[1], c[2], 0.85 * row_anim_op],
                    av_size * 0.5,
                );
            }

            // Name text
            let icon_sz = 16.0f32;
            let icon_gap = 4.0f32;
            let icons_w = icon_sz * 2.0 + icon_gap + 8.0;
            let name_x = av_x + av_size + 8.0;
            let name_w_max = sw - name_x - icons_w;
            if let Some(&(tex, tw, th)) = self.name_textures.get(user_id) {
                let draw_w = (tw as f32).min(name_w_max);
                let draw_h = th as f32;
                let name_y = row_y_f + (row_h as f32 - draw_h) * 0.5;
                self.egl
                    .draw_icon(name_x, name_y, draw_w, draw_h, sw, sh, tex, row_anim_op);
            }

            // Per-participant mute/deaf icons (right side of row)
            let mic_tex = self.egl.tex_mic;
            let hp_tex = self.egl.tex_headphone;
            let strike_tex = self.egl.tex_strikeout;
            let icon_y = row_y_f + (row_h as f32 - icon_sz) * 0.5;
            let mic_x = sw - icon_sz * 2.0 - icon_gap - 8.0;
            let hp_x = sw - icon_sz - 8.0;
            let mic_op = if *muted { row_anim_op * 0.9 } else { row_anim_op * 0.35 };
            if *muted {
                self.egl.draw_rect(
                    mic_x - 2.0,
                    icon_y - 2.0,
                    icon_sz + 4.0,
                    icon_sz + 4.0,
                    sw,
                    sh,
                    [0.7, 0.15, 0.15, 0.6 * row_anim_op],
                    4.0,
                );
            }
            self.egl
                .draw_icon(mic_x, icon_y, icon_sz, icon_sz, sw, sh, mic_tex, mic_op);
            if *muted {
                self.egl.draw_icon(
                    mic_x, icon_y, icon_sz, icon_sz, sw, sh, strike_tex,
                    row_anim_op * 0.85,
                );
            }
            let hp_op = if *deafened { row_anim_op * 0.9 } else { row_anim_op * 0.35 };
            if *deafened {
                self.egl.draw_rect(
                    hp_x - 2.0,
                    icon_y - 2.0,
                    icon_sz + 4.0,
                    icon_sz + 4.0,
                    sw,
                    sh,
                    [0.7, 0.15, 0.15, 0.6 * row_anim_op],
                    4.0,
                );
            }
            self.egl
                .draw_icon(hp_x, icon_y, icon_sz, icon_sz, sw, sh, hp_tex, hp_op);
            if *deafened {
                self.egl.draw_icon(
                    hp_x, icon_y, icon_sz, icon_sz, sw, sh, strike_tex,
                    row_anim_op * 0.85,
                );
            }
        }

        // Scroll indicator strip (only when there are more participants than visible rows)
        if self.participants.len() > self.max_visible_rows {
            let indicator_y = 64.0 + self.visible_row_count() as f32 * row_h as f32;
            self.egl.draw_rect(
                0.0, indicator_y, sw, 20.0, sw, sh,
                [0.15, 0.15, 0.18, op * 0.9],
                0.0,
            );
            if let Some((tex, tw, th)) = self.scroll_indicator_tex {
                let tx = (sw - tw as f32) * 0.5;
                let ty = indicator_y + (20.0 - th as f32) * 0.5;
                self.egl.draw_icon(tx, ty, tw as f32, th as f32, sw, sh, tex, op * 0.7);
            }
        }

        self.egl.swap();
    }
}
