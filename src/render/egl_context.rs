//! Extracted EglContext implementation (moved from mod.rs)

#[cfg(not(test))]
use std::ffi::c_void;

#[cfg(not(test))]
use glow::HasContext;
#[cfg(not(test))]
use khronos_egl as egl;
#[cfg(not(test))]
use sctk::reexports::client::protocol::wl_surface::WlSurface;
#[cfg(not(test))]
use sctk::reexports::client::Connection;
#[cfg(not(test))]
use sctk::reexports::client::Proxy;
#[cfg(not(test))]
use smithay_client_toolkit as sctk;
#[cfg(not(test))]
use wayland_egl::WlEglSurface;

#[cfg(not(test))]
use super::{
    icon_headphone, icon_mic, icon_strikeout, upload_texture, AVATAR_FRAG_SRC,
    EGL_PLATFORM_WAYLAND_KHR, FRAG_SRC, ICON_FRAG_SRC, VERT_SRC,
};

#[cfg(not(test))]
use super::EglBackend;

#[cfg(not(test))]
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
#[cfg(not(test))]
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
#[cfg(not(test))]
impl EglBackend for EglContext {
    fn resize(&self, width: i32, height: i32) {
        EglContext::resize(self, width, height)
    }
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
        EglContext::draw_rect(self, px, py, pw, ph, surf_w, surf_h, color, radius)
    }
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
        EglContext::draw_icon(self, px, py, pw, ph, surf_w, surf_h, tex, opacity)
    }
    fn draw_avatar(
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
        EglContext::draw_avatar(self, px, py, size, surf_w, surf_h, tex, opacity, desaturate)
    }
    fn swap(&self) {
        EglContext::swap(self)
    }
    fn delete_texture(&self, tex: glow::NativeTexture) {
        EglContext::delete_texture(self, tex)
    }
    fn upload_texture_wh(&self, pixels: &[u8], w: u32, h: u32) -> glow::NativeTexture {
        EglContext::upload_texture_wh(self, pixels, w, h)
    }
    fn tex_mic(&self) -> glow::NativeTexture {
        EglContext::tex_mic(self)
    }
    fn tex_headphone(&self) -> glow::NativeTexture {
        EglContext::tex_headphone(self)
    }
    fn tex_strikeout(&self) -> glow::NativeTexture {
        EglContext::tex_strikeout(self)
    }
    fn viewport(&self, x: i32, y: i32, w: i32, h: i32) {
        EglContext::viewport(self, x, y, w, h)
    }
    fn clear_color(&self, r: f32, g: f32, b: f32, a: f32) {
        EglContext::clear_color(self, r, g, b, a)
    }
    fn clear(&self, mask: u32) {
        EglContext::clear(self, mask)
    }
    fn use_main_program(&self) {
        EglContext::use_main_program(self)
    }
}
