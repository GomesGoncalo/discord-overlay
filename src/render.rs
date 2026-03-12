use std::ffi::c_void;

use smithay_client_toolkit as sctk;
use sctk::reexports::client::Connection;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::Proxy;
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

    pub fn resize(&self, width: i32, height: i32) {
        self.wl_egl.resize(width, height, 0, 0);
    }

    /// Render one rounded rectangle. Coordinates are in logical pixels, origin = top-left.
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
            self.gl.uniform_1_f32(Some(&self.avatar_loc_opacity), opacity);
            let u_des = self.gl.get_uniform_location(self.avatar_prog, "u_desaturate");
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
                eprintln!("Loaded font: {p}");
                return Some(font);
            }
        }
    }
    None
}
