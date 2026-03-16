#[cfg(not(test))]
use glow::HasContext;
#[cfg(not(test))]
use khronos_egl as egl;
pub mod shaders;

pub mod program_locations;

pub mod program_gl;

pub mod program;

#[cfg(not(test))]
pub use shaders::{AVATAR_FRAG_SRC, FRAG_SRC, ICON_FRAG_SRC, VERT_SRC};

// EGL_PLATFORM_WAYLAND_KHR (0x31D8) — part of EGL_KHR_platform_wayland
#[cfg(not(test))]
const EGL_PLATFORM_WAYLAND_KHR: egl::Enum = 0x31D8;

// ─── Procedural icon generation ──────────────────────────────────────────────

pub mod math;

pub mod draw;

pub use math::{rasterize, sdf_arc, sdf_rrect, smoothstep};

fn icon_mic(size: u32) -> Vec<u8> {
    rasterize(size, |px, py| {
        let body = sdf_rrect(px, py, 0.0, 0.0, 0.12, 0.22, 0.12);
        let stand = sdf_arc(px, py, 0.0, -0.08, 0.28, 0.03, true);
        let stem = sdf_rrect(px, py, 0.0, -0.36, 0.03, 0.055, 0.03);
        let base = sdf_rrect(px, py, 0.0, -0.42, 0.16, 0.03, 0.03);
        body.min(stand).min(stem).min(base)
    })
}

fn icon_headphone(size: u32) -> Vec<u8> {
    rasterize(size, |px, py| {
        let band = sdf_arc(px, py, 0.0, 0.0, 0.32, 0.04, false);
        let left = sdf_rrect(px, py, -0.32, 0.0, 0.09, 0.15, 0.07);
        let right = sdf_rrect(px, py, 0.32, 0.0, 0.09, 0.15, 0.07);
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

pub mod compile;
pub mod text;
pub mod textures;

pub use text::{load_system_font, render_text_texture};
#[cfg(not(test))]
pub use textures::{upload_texture, upload_texture_wh};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoothstep_values() {
        assert_eq!(smoothstep(0.0, 1.0, 0.0), 0.0);
        assert_eq!(smoothstep(0.0, 1.0, 1.0), 1.0);
        assert!((smoothstep(0.0, 1.0, 0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn smoothstep_edge_cases() {
        // Below range
        assert!(smoothstep(0.5, 1.0, 0.0) == 0.0);

        // Above range
        assert!(smoothstep(0.0, 0.5, 1.0) == 1.0);

        // Zero-width range (edge0 == edge1 causes division by zero, results in NaN or inf)
        let _result = smoothstep(0.5, 0.5, 0.5);
        // Don't test this case - it's undefined behavior with division by zero

        // Negative values
        let result = smoothstep(-1.0, 1.0, 0.0);
        assert!((result - 0.5).abs() < 1e-6);

        // Larger range
        let result = smoothstep(0.0, 100.0, 50.0);
        assert!((result - 0.5).abs() < 1e-6);
    }

    #[test]
    fn sdf_rrect_center_inside() {
        let d = sdf_rrect(0.0, 0.0, 0.0, 0.0, 0.2, 0.2, 0.05);
        assert!(d < 0.0);
    }

    #[test]
    fn sdf_rrect_outside() {
        // Point far outside should have positive distance
        let d = sdf_rrect(1.0, 1.0, 0.0, 0.0, 0.1, 0.1, 0.05);
        assert!(d > 0.0);
    }

    #[test]
    fn sdf_rrect_on_boundary() {
        // Points on/near boundary should transition from negative to positive
        let inside = sdf_rrect(0.0, 0.05, 0.0, 0.0, 0.1, 0.1, 0.01);
        let outside = sdf_rrect(0.0, 0.15, 0.0, 0.0, 0.1, 0.1, 0.01);
        assert!(inside < 0.0);
        assert!(outside > 0.0);
    }

    #[test]
    fn sdf_arc_behavior() {
        let r = sdf_arc(0.0, -0.1, 0.0, 0.0, 0.28, 0.03, true);
        assert!(r.is_finite());
        let r2 = sdf_arc(0.0, 0.1, 0.0, 0.0, 0.28, 0.03, true);
        assert_eq!(r2, 99.0);
    }

    #[test]
    fn sdf_arc_top_and_bottom() {
        // Test both top (bottom=false) and bottom (bottom=true) variants
        let top = sdf_arc(0.0, 0.0, 0.0, 0.0, 0.2, 0.02, false);
        let bottom = sdf_arc(0.0, 0.0, 0.0, 0.0, 0.2, 0.02, true);
        assert!(top.is_finite());
        assert!(bottom.is_finite());
    }

    #[test]
    fn sdf_arc_at_different_radii() {
        for r in [0.1, 0.2, 0.5, 1.0].iter() {
            let d = sdf_arc(0.0, 0.0, 0.0, 0.0, *r, 0.05, true);
            assert!(d.is_finite(), "sdf_arc should be finite for radius {}", r);
        }
    }

    #[test]
    fn rasterize_alpha_all() {
        let buf = rasterize(8, |_px, _py| -10.0);
        assert_eq!(buf.len(), (8 * 8 * 4) as usize);
        for i in 0..(8 * 8) {
            assert!(buf[i * 4 + 3] >= 250);
        }
    }

    #[test]
    fn rasterize_varying_distances() {
        // Test rasterization with varying SDF distances
        let buf = rasterize(4, |px, py| {
            let dx = px - 0.5;
            let dy = py - 0.5;
            (dx * dx + dy * dy).sqrt() - 0.2
        });
        assert_eq!(buf.len(), (4 * 4 * 4) as usize);

        // Verify we have some variation in alpha values
        let alphas: Vec<u8> = (0..4 * 4).map(|i| buf[i * 4 + 3]).collect();

        // Should have at least some non-maximum values
        assert!(alphas.iter().any(|&a| a < 255));
    }

    #[test]
    fn rasterize_different_sizes() {
        for size in [4, 8, 16, 32].iter() {
            let buf = rasterize(*size, |_px, _py| -1.0);
            assert_eq!(buf.len(), (*size * *size * 4) as usize);
            // All pixels should have full alpha for negative distance
            for i in 0..*size as usize * *size as usize {
                assert!(buf[i * 4 + 3] > 200);
            }
        }
    }

    #[test]
    fn icons_nonempty() {
        let m = icon_mic(16);
        assert_eq!(m.len(), (16 * 16 * 4) as usize);
        assert!(m.iter().any(|b| *b != 0));
        let h = icon_headphone(16);
        assert_eq!(h.len(), (16 * 16 * 4) as usize);
        assert!(h.iter().any(|b| *b != 0));
        let s = icon_strikeout(16);
        assert_eq!(s.len(), (16 * 16 * 4) as usize);
        assert!(s.iter().any(|b| *b != 0));
    }

    #[test]
    fn icons_have_alpha() {
        for (icon_fn, name) in [
            (icon_mic as fn(u32) -> Vec<u8>, "mic"),
            (icon_headphone, "headphone"),
            (icon_strikeout, "strikeout"),
        ] {
            let buf = icon_fn(16);
            // Icons should have some pixel data (not all zeros)
            assert!(
                buf.iter().any(|&b| b != 0),
                "{} icon should have non-zero pixels",
                name
            );
        }
    }

    #[test]
    fn icons_consistent_size() {
        for size in [8, 16, 32].iter() {
            let m = icon_mic(*size);
            let h = icon_headphone(*size);
            let s = icon_strikeout(*size);
            let expected_len = (*size * *size * 4) as usize;
            assert_eq!(m.len(), expected_len);
            assert_eq!(h.len(), expected_len);
            assert_eq!(s.len(), expected_len);
        }
    }

    #[test]
    fn render_text_texture_with_system_font_optional() {
        if let Some(font) = load_system_font() {
            let (pixels, w, h) = render_text_texture(&font, "Hello", 12.0);
            assert!(w > 0 && h > 0);
            assert_eq!(pixels.len(), (w * h * 4) as usize);
        }
    }

    #[test]
    fn render_text_texture_different_sizes() {
        if let Some(font) = load_system_font() {
            for size in [8.0, 12.0, 16.0, 24.0].iter() {
                let (pixels, w, h) = render_text_texture(&font, "Test", *size);
                assert!(w > 0 && h > 0);
                assert_eq!(pixels.len(), (w * h * 4) as usize);
                // Larger font size should generally produce larger texture
                assert!(w > 4);
                assert!(h > 4);
            }
        }
    }

    #[test]
    fn render_text_texture_empty_string() {
        if let Some(font) = load_system_font() {
            let (pixels, w, h) = render_text_texture(&font, "", 12.0);
            // Empty string may produce minimal texture or 1x1
            assert!(w >= 1 && h >= 1);
            assert_eq!(pixels.len(), (w * h * 4) as usize);
        }
    }
}

#[cfg(not(test))]
// Additional helper methods for testability and a mock EGL backend used in #[cfg(test)]
impl EglContext {
    pub fn upload_texture_wh(&self, pixels: &[u8], w: u32, h: u32) -> glow::NativeTexture {
        unsafe { upload_texture_wh(&self.gl, pixels, w, h) }
    }

    pub fn delete_texture(&self, tex: glow::NativeTexture) {
        unsafe {
            self.gl.delete_texture(tex);
        }
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

#[cfg(test)]
pub struct MockEgl {}

#[cfg(test)]
impl MockEgl {
    pub fn new() -> Self {
        MockEgl {}
    }
    pub fn resize(&self, _w: i32, _h: i32) {}
    #[allow(clippy::too_many_arguments)]
    pub fn draw_rect(
        &self,
        _px: f32,
        _py: f32,
        _pw: f32,
        _ph: f32,
        _surf_w: f32,
        _surf_h: f32,
        _color: [f32; 4],
        _radius: f32,
    ) {
    }
    #[allow(clippy::too_many_arguments)]
    pub fn draw_icon(
        &self,
        _px: f32,
        _py: f32,
        _pw: f32,
        _ph: f32,
        _surf_w: f32,
        _surf_h: f32,
        _tex: glow::NativeTexture,
        _opacity: f32,
    ) {
    }
    #[allow(clippy::too_many_arguments)]
    pub fn draw_avatar(
        &self,
        _px: f32,
        _py: f32,
        _size: f32,
        _surf_w: f32,
        _surf_h: f32,
        _tex: glow::NativeTexture,
        _opacity: f32,
        _desaturate: f32,
    ) {
    }
    pub fn swap(&self) {}
    pub fn delete_texture(&self, _tex: glow::NativeTexture) {}
    pub fn upload_texture_wh(&self, _pixels: &[u8], _w: u32, _h: u32) -> glow::NativeTexture {
        std::num::NonZeroU32::new(1)
            .map(|nz| unsafe {
                std::mem::transmute::<std::num::NonZeroU32, glow::NativeTexture>(nz)
            })
            .unwrap()
    }
    pub fn tex_mic(&self) -> glow::NativeTexture {
        std::num::NonZeroU32::new(2)
            .map(|nz| unsafe {
                std::mem::transmute::<std::num::NonZeroU32, glow::NativeTexture>(nz)
            })
            .unwrap()
    }
    pub fn tex_headphone(&self) -> glow::NativeTexture {
        std::num::NonZeroU32::new(3)
            .map(|nz| unsafe {
                std::mem::transmute::<std::num::NonZeroU32, glow::NativeTexture>(nz)
            })
            .unwrap()
    }
    pub fn tex_strikeout(&self) -> glow::NativeTexture {
        std::num::NonZeroU32::new(4)
            .map(|nz| unsafe {
                std::mem::transmute::<std::num::NonZeroU32, glow::NativeTexture>(nz)
            })
            .unwrap()
    }
}

#[cfg(not(test))]
impl EglContext {
    pub fn viewport(&self, x: i32, y: i32, w: i32, h: i32) {
        unsafe {
            self.gl.viewport(x, y, w, h);
        }
    }
    pub fn clear_color(&self, r: f32, g: f32, b: f32, a: f32) {
        unsafe {
            self.gl.clear_color(r, g, b, a);
        }
    }
    pub fn clear(&self, mask: u32) {
        unsafe {
            self.gl.clear(mask);
        }
    }
    pub fn use_main_program(&self) {
        unsafe {
            self.main_prog.use_program(&self.gl);
        }
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
#[allow(clippy::too_many_arguments)]
pub trait EglBackend {
    fn resize(&self, width: i32, height: i32);
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
    );
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
    );
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
    );
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

#[cfg(not(test))]
pub mod egl_context;
#[cfg(not(test))]
pub use self::egl_context::EglContext;

#[cfg(test)]
impl EglBackend for MockEgl {
    fn resize(&self, _width: i32, _height: i32) {
        self.resize(_width, _height)
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
        self.draw_rect(px, py, pw, ph, surf_w, surf_h, color, radius)
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
        self.draw_icon(px, py, pw, ph, surf_w, surf_h, tex, opacity)
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
        self.draw_avatar(px, py, size, surf_w, surf_h, tex, opacity, desaturate)
    }
    fn swap(&self) {
        self.swap()
    }
    fn delete_texture(&self, tex: glow::NativeTexture) {
        self.delete_texture(tex)
    }
    fn upload_texture_wh(&self, pixels: &[u8], w: u32, h: u32) -> glow::NativeTexture {
        self.upload_texture_wh(pixels, w, h)
    }
    fn tex_mic(&self) -> glow::NativeTexture {
        self.tex_mic()
    }
    fn tex_headphone(&self) -> glow::NativeTexture {
        self.tex_headphone()
    }
    fn tex_strikeout(&self) -> glow::NativeTexture {
        self.tex_strikeout()
    }
    fn viewport(&self, x: i32, y: i32, w: i32, h: i32) {
        self.viewport(x, y, w, h)
    }
    fn clear_color(&self, r: f32, g: f32, b: f32, a: f32) {
        self.clear_color(r, g, b, a)
    }
    fn clear(&self, mask: u32) {
        self.clear(mask)
    }
    fn use_main_program(&self) {
        self.use_main_program()
    }
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
