use std::ffi::c_void;
use tracing::info;

use glow::HasContext;
use khronos_egl as egl;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::Connection;
use sctk::reexports::client::Proxy;
use smithay_client_toolkit as sctk;
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

/// Avatar shader — clips the quad to a circle using SDF, with optional greyscale desaturation.
const AVATAR_FRAG_SRC: &str = r"
precision mediump float;
varying vec2 v_local;
uniform sampler2D u_texture;
uniform float u_opacity;
uniform float u_desaturate;
void main() {
    vec2 uv = v_local - 0.5;
    float d = length(uv) - 0.5 + 0.015;
    float aa = 0.015;
    float a = 1.0 - smoothstep(-aa, aa, d);
    vec4 color = texture2D(u_texture, vec2(v_local.x, 1.0 - v_local.y));
    float luma = dot(color.rgb, vec3(0.299, 0.587, 0.114));
    color.rgb = mix(color.rgb, vec3(luma), u_desaturate);
    gl_FragColor = vec4(color.rgb, color.a * a * u_opacity);
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
    } else if py >= cy {
        ring
    } else {
        99.0
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

pub struct EglContext {
    egl: egl::DynamicInstance<egl::EGL1_5>,
    egl_display: egl::Display,
    egl_surface: egl::Surface,
    _egl_context: egl::Context,
    wl_egl: WlEglSurface,
    pub gl: glow::Context,
    // Rounded-rect shader (background fills)
    pub program: glow::NativeProgram,
    vbo: glow::NativeBuffer,
    loc_color: glow::UniformLocation,
    loc_size: glow::UniformLocation,
    loc_radius: glow::UniformLocation,
    loc_pos: u32,
    loc_local: u32,
    // Icon overlay shader + textures
    icon_prog: glow::NativeProgram,
    icon_loc_opacity: glow::UniformLocation,
    pub tex_mic: glow::NativeTexture,
    pub tex_headphone: glow::NativeTexture,
    pub tex_strikeout: glow::NativeTexture,
    // Circular avatar shader
    avatar_prog: glow::NativeProgram,
    avatar_loc_opacity: glow::UniformLocation,
}

impl EglContext {
    pub fn new(conn: &Connection, wl_surface: &WlSurface, width: i32, height: i32) -> Self {
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
                Some(f) => f as *const c_void,
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

    pub fn resize(&self, width: i32, height: i32) {
        self.wl_egl.resize(width, height, 0, 0);
    }

    /// Render one rounded rectangle. Coordinates are in logical pixels, origin = top-left.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_rect(
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

    pub fn swap(&self) {
        self.egl
            .swap_buffers(self.egl_display, self.egl_surface)
            .expect("EGL swap_buffers failed");
    }

    /// Render an icon texture over a quad (same coord system as draw_rect).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_icon(
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
    /// `desaturate`: 0.0 = full colour, 1.0 = greyscale (used for deafened participants).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_avatar(
        &self,
        px: f32,
        py: f32,
        size: f32,
        surf_w: f32,
        surf_h: f32,
        tex: glow::NativeTexture,
        opacity: f32,
        desaturate: f32,
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
            let u_des = self
                .gl
                .get_uniform_location(self.avatar_prog, "u_desaturate");
            self.gl.uniform_1_f32(u_des.as_ref(), desaturate);
            self.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            self.gl.use_program(Some(self.program));
        }
    }
}

/// Upload an RGBA pixel buffer as a GL TEXTURE_2D with linear filtering (non-square).
pub unsafe fn upload_texture_wh(
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
        glow::PixelUnpackData::Slice(Some(pixels)),
    );
    tex
}

/// Upload a square RGBA pixel buffer.
unsafe fn upload_texture(gl: &glow::Context, pixels: &[u8], size: u32) -> glow::NativeTexture {
    upload_texture_wh(gl, pixels, size, size)
}

// ─── Text rendering helpers ───────────────────────────────────────────────────

pub fn render_text_texture(font: &fontdue::Font, text: &str, px_size: f32) -> (Vec<u8>, u32, u32) {
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

pub fn load_system_font() -> Option<fontdue::Font> {
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
                info!("Loaded font: {p}");
                return Some(font);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoothstep_values() {
        assert_eq!(smoothstep(0.0,1.0,0.0),0.0);
        assert_eq!(smoothstep(0.0,1.0,1.0),1.0);
        assert!((smoothstep(0.0,1.0,0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn sdf_rrect_center_inside() {
        let d = sdf_rrect(0.0,0.0,0.0,0.0,0.2,0.2,0.05);
        assert!(d < 0.0);
    }

    #[test]
    fn sdf_arc_behavior() {
        let r = sdf_arc(0.0, -0.1, 0.0, 0.0, 0.28, 0.03, true);
        assert!(r.is_finite());
        let r2 = sdf_arc(0.0, 0.1, 0.0, 0.0, 0.28, 0.03, true);
        assert_eq!(r2, 99.0);
    }

    #[test]
    fn rasterize_alpha_all() {
        let buf = rasterize(8, |_px,_py| -10.0);
        assert_eq!(buf.len(), (8*8*4) as usize);
        for i in 0..(8*8) {
            assert!(buf[i*4 + 3] >= 250);
        }
    }

    #[test]
    fn icons_nonempty() {
        let m = icon_mic(16);
        assert_eq!(m.len(), (16*16*4) as usize);
        assert!(m.iter().any(|b| *b != 0));
        let h = icon_headphone(16);
        assert_eq!(h.len(), (16*16*4) as usize);
        assert!(h.iter().any(|b| *b != 0));
        let s = icon_strikeout(16);
        assert_eq!(s.len(), (16*16*4) as usize);
        assert!(s.iter().any(|b| *b != 0));
    }

    #[test]
    fn render_text_texture_with_system_font_optional() {
        if let Some(font) = load_system_font() {
            let (pixels, w, h) = render_text_texture(&font, "Hello", 12.0);
            assert!(w > 0 && h > 0);
            assert_eq!(pixels.len(), (w * h * 4) as usize);
        }
    }
}


// Additional helper methods for testability and a mock EGL backend used in #[cfg(test)]
impl EglContext {
    pub fn upload_texture_wh(&self, pixels: &[u8], w: u32, h: u32) -> glow::NativeTexture {
        unsafe { upload_texture_wh(&self.gl, pixels, w, h) }
    }

    pub fn delete_texture(&self, tex: glow::NativeTexture) {
        unsafe { self.gl.delete_texture(tex); }
    }

    pub fn tex_mic(&self) -> glow::NativeTexture {
        self.tex_mic
    }
    pub fn tex_headphone(&self) -> glow::NativeTexture {
        self.tex_headphone
    }
    pub fn tex_strikeout(&self) -> glow::NativeTexture {
        self.tex_strikeout
    }
}

// Type alias for the Egl backend used by App (real in normal builds, mock in tests)
#[cfg(not(test))]
pub type Egl = EglContext;

#[cfg(test)]
pub struct MockEgl {}

#[cfg(test)]
impl MockEgl {
    pub fn new() -> Self { MockEgl {} }
    pub fn resize(&self, _w: i32, _h: i32) {}
    #[allow(clippy::too_many_arguments)]
    pub fn draw_rect(&self, _px: f32, _py: f32, _pw: f32, _ph: f32, _surf_w: f32, _surf_h: f32, _color: [f32;4], _radius: f32) {}
    #[allow(clippy::too_many_arguments)]
    pub fn draw_icon(&self, _px: f32, _py: f32, _pw: f32, _ph: f32, _surf_w:f32, _surf_h:f32, _tex: glow::NativeTexture, _opacity: f32) {}
    #[allow(clippy::too_many_arguments)]
    pub fn draw_avatar(&self, _px: f32, _py: f32, _size: f32, _surf_w: f32, _surf_h: f32, _tex: glow::NativeTexture, _opacity: f32, _desaturate: f32) {}
    pub fn swap(&self) {}
    pub fn delete_texture(&self, _tex: glow::NativeTexture) {}
    pub fn upload_texture_wh(&self, _pixels: &[u8], _w: u32, _h: u32) -> glow::NativeTexture { std::num::NonZeroU32::new(1).map(|nz| unsafe { std::mem::transmute::<std::num::NonZeroU32, glow::NativeTexture>(nz) }).unwrap() }
    pub fn tex_mic(&self) -> glow::NativeTexture { std::num::NonZeroU32::new(2).map(|nz| unsafe { std::mem::transmute::<std::num::NonZeroU32, glow::NativeTexture>(nz) }).unwrap() }
    pub fn tex_headphone(&self) -> glow::NativeTexture { std::num::NonZeroU32::new(3).map(|nz| unsafe { std::mem::transmute::<std::num::NonZeroU32, glow::NativeTexture>(nz) }).unwrap() }
    pub fn tex_strikeout(&self) -> glow::NativeTexture { std::num::NonZeroU32::new(4).map(|nz| unsafe { std::mem::transmute::<std::num::NonZeroU32, glow::NativeTexture>(nz) }).unwrap() }
}


#[cfg(test)]
pub type Egl = MockEgl;

impl EglContext {
    pub fn viewport(&self, x: i32, y: i32, w: i32, h: i32) {
        unsafe { self.gl.viewport(x, y, w, h); }
    }
    pub fn clear_color(&self, r: f32, g: f32, b: f32, a: f32) {
        unsafe { self.gl.clear_color(r, g, b, a); }
    }
    pub fn clear(&self, mask: u32) {
        unsafe { self.gl.clear(mask); }
    }
    pub fn use_main_program(&self) {
        unsafe { self.gl.use_program(Some(self.program)); }
    }
}

#[cfg(test)]
impl MockEgl {
    pub fn viewport(&self, _x: i32, _y: i32, _w: i32, _h: i32) {}
    pub fn clear_color(&self, _r: f32, _g: f32, _b: f32, _a: f32) {}
    pub fn clear(&self, _mask: u32) {}
    pub fn use_main_program(&self) {}
}


// Trait and impls to allow using Box<dyn EglBackend> for production and tests.
pub trait EglBackend {
    fn resize(&self, width: i32, height: i32);
    fn draw_rect(&self, px: f32, py: f32, pw: f32, ph: f32, surf_w: f32, surf_h: f32, color: [f32;4], radius: f32);
    fn draw_icon(&self, px: f32, py: f32, pw: f32, ph: f32, surf_w: f32, surf_h: f32, tex: glow::NativeTexture, opacity: f32);
    fn draw_avatar(&self, px: f32, py: f32, size: f32, surf_w: f32, surf_h: f32, tex: glow::NativeTexture, opacity: f32, desaturate: f32);
    fn swap(&self);
    fn delete_texture(&self, tex: glow::NativeTexture);
    fn upload_texture_wh(&self, pixels: &[u8], w: u32, h: u32) -> glow::NativeTexture;
    fn tex_mic(&self) -> glow::NativeTexture;
    fn tex_headphone(&self) -> glow::NativeTexture;
    fn tex_strikeout(&self) -> glow::NativeTexture;
    fn viewport(&self, x: i32, y: i32, w: i32, h: i32);
    fn clear_color(&self, r: f32, g: f32, b: f32, a: f32);
    fn clear(&self, mask: u32);
    fn use_main_program(&self);
}

impl EglBackend for EglContext {
    fn resize(&self, width: i32, height: i32) { EglContext::resize(self, width, height) }
    fn draw_rect(&self, px: f32, py: f32, pw: f32, ph: f32, surf_w: f32, surf_h: f32, color: [f32;4], radius: f32) { EglContext::draw_rect(self, px, py, pw, ph, surf_w, surf_h, color, radius) }
    fn draw_icon(&self, px: f32, py: f32, pw: f32, ph: f32, surf_w: f32, surf_h: f32, tex: glow::NativeTexture, opacity: f32) { EglContext::draw_icon(self, px, py, pw, ph, surf_w, surf_h, tex, opacity) }
    fn draw_avatar(&self, px: f32, py: f32, size: f32, surf_w: f32, surf_h: f32, tex: glow::NativeTexture, opacity: f32, desaturate: f32) { EglContext::draw_avatar(self, px, py, size, surf_w, surf_h, tex, opacity, desaturate) }
    fn swap(&self) { EglContext::swap(self) }
    fn delete_texture(&self, tex: glow::NativeTexture) { EglContext::delete_texture(self, tex) }
    fn upload_texture_wh(&self, pixels: &[u8], w: u32, h: u32) -> glow::NativeTexture { EglContext::upload_texture_wh(self, pixels, w, h) }
    fn tex_mic(&self) -> glow::NativeTexture { EglContext::tex_mic(self) }
    fn tex_headphone(&self) -> glow::NativeTexture { EglContext::tex_headphone(self) }
    fn tex_strikeout(&self) -> glow::NativeTexture { EglContext::tex_strikeout(self) }
    fn viewport(&self, x: i32, y: i32, w: i32, h: i32) { EglContext::viewport(self, x, y, w, h) }
    fn clear_color(&self, r: f32, g: f32, b: f32, a: f32) { EglContext::clear_color(self, r, g, b, a) }
    fn clear(&self, mask: u32) { EglContext::clear(self, mask) }
    fn use_main_program(&self) { EglContext::use_main_program(self) }
}

#[cfg(test)]
impl EglBackend for MockEgl {
    fn resize(&self, _width: i32, _height: i32) { self.resize(_width, _height) }
    fn draw_rect(&self, px: f32, py: f32, pw: f32, ph: f32, surf_w: f32, surf_h: f32, color: [f32;4], radius: f32) { self.draw_rect(px, py, pw, ph, surf_w, surf_h, color, radius) }
    fn draw_icon(&self, px: f32, py: f32, pw: f32, ph: f32, surf_w: f32, surf_h: f32, tex: glow::NativeTexture, opacity: f32) { self.draw_icon(px, py, pw, ph, surf_w, surf_h, tex, opacity) }
    fn draw_avatar(&self, px: f32, py: f32, size: f32, surf_w: f32, surf_h: f32, tex: glow::NativeTexture, opacity: f32, desaturate: f32) { self.draw_avatar(px, py, size, surf_w, surf_h, tex, opacity, desaturate) }
    fn swap(&self) { self.swap() }
    fn delete_texture(&self, tex: glow::NativeTexture) { self.delete_texture(tex) }
    fn upload_texture_wh(&self, pixels: &[u8], w: u32, h: u32) -> glow::NativeTexture { self.upload_texture_wh(pixels, w, h) }
    fn tex_mic(&self) -> glow::NativeTexture { self.tex_mic() }
    fn tex_headphone(&self) -> glow::NativeTexture { self.tex_headphone() }
    fn tex_strikeout(&self) -> glow::NativeTexture { self.tex_strikeout() }
    fn viewport(&self, x: i32, y: i32, w: i32, h: i32) { self.viewport(x,y,w,h) }
    fn clear_color(&self, r: f32, g: f32, b: f32, a: f32) { self.clear_color(r,g,b,a) }
    fn clear(&self, mask: u32) { self.clear(mask) }
    fn use_main_program(&self) { self.use_main_program() }
}

#[cfg(test)]
mod mock_tests {
    use super::*;

    #[test]
    fn mock_egl_methods_noop() {
        let egl = MockEgl::new();
        egl.resize(100, 50);
        egl.viewport(0, 0, 100, 50);
        egl.clear_color(0.1, 0.2, 0.3, 0.4);
        egl.clear(glow::COLOR_BUFFER_BIT);
        egl.use_main_program();
        let tex = egl.upload_texture_wh(&[255u8; 4], 1, 1);
        egl.draw_rect(0.0, 0.0, 10.0, 10.0, 100.0, 50.0, [1.0, 1.0, 1.0, 1.0], 2.0);
        egl.draw_icon(0.0, 0.0, 8.0, 8.0, 100.0, 50.0, tex, 0.5);
        egl.draw_avatar(0.0, 0.0, 8.0, 100.0, 50.0, tex, 0.5, 0.0);
        egl.delete_texture(tex);
        let _ = egl.tex_mic();
        let _ = egl.tex_headphone();
        let _ = egl.tex_strikeout();
        egl.swap();
    }
}
