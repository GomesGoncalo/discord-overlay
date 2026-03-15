// Draw helper utilities: generate quad vertices (NDC) and upload/bind them.
// Keep pure functions testable; GL-dependent helpers are cfg-gated.

/// Generate vertex array for a quad covering the pixel rectangle (px,py,pw,ph)
/// into NDC coordinates. Each vertex is (ndc_x, ndc_y, local_u, local_v).
/// Returned array layout matches the existing code: TL, TR, BL, BR.
pub fn verts_from_pixels(
    px: f32,
    py: f32,
    pw: f32,
    ph: f32,
    surf_w: f32,
    surf_h: f32,
) -> [f32; 16] {
    let x0 = px / surf_w * 2.0 - 1.0;
    let x1 = (px + pw) / surf_w * 2.0 - 1.0;
    let y0 = 1.0 - py / surf_h * 2.0;
    let y1 = 1.0 - (py + ph) / surf_h * 2.0;
    [
        x0, y0, 0.0, 0.0, x1, y0, 1.0, 0.0, x0, y1, 0.0, 1.0, x1, y1, 1.0, 1.0,
    ]
}

#[allow(dead_code)]
/// Convert verts (16 f32) into an owned Vec<u8> containing the raw bytes.
pub fn verts_to_bytes(verts: &[f32; 16]) -> Vec<u8> {
    // Safe copy of raw bytes
    unsafe { std::slice::from_raw_parts(verts.as_ptr() as *const u8, 16 * 4) }.to_vec()
}

#[cfg(not(test))]
use glow::HasContext;

#[cfg(not(test))]
/// Upload verts to the given VBO (ARRAY_BUFFER) and set DYNAMIC_DRAW.
pub unsafe fn upload_verts(gl: &glow::Context, vbo: glow::NativeBuffer, verts: &[f32; 16]) {
    let bytes = std::slice::from_raw_parts(verts.as_ptr() as *const u8, 16 * 4);
    gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
    gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::DYNAMIC_DRAW);
}

#[cfg(not(test))]
/// Enable and set the vertex attribute pointers for the quad vertex layout.
pub unsafe fn enable_quad_attribs(gl: &glow::Context, loc_pos: u32, loc_local: u32) {
    gl.enable_vertex_attrib_array(loc_pos);
    gl.enable_vertex_attrib_array(loc_local);
    // stride = 16 bytes (4 floats), offsets 0 and 8 bytes
    gl.vertex_attrib_pointer_f32(loc_pos, 2, glow::FLOAT, false, 16, 0);
    gl.vertex_attrib_pointer_f32(loc_local, 2, glow::FLOAT, false, 16, 8);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ndc_quad_basic() {
        let verts = verts_from_pixels(0.0, 0.0, 100.0, 200.0, 800.0, 600.0);
        let x0 = 0.0 / 800.0 * 2.0 - 1.0;
        let x1 = 100.0 / 800.0 * 2.0 - 1.0;
        let y0 = 1.0 - 0.0 / 600.0 * 2.0;
        let y1 = 1.0 - 200.0 / 600.0 * 2.0;
        assert!((verts[0] - x0).abs() < 1e-6);
        assert!((verts[4] - x1).abs() < 1e-6);
        assert!((verts[1] - y0).abs() < 1e-6);
        assert!((verts[9] - y1).abs() < 1e-6);
    }

    #[test]
    fn verts_bytes_len() {
        let verts = verts_from_pixels(0.0, 0.0, 8.0, 8.0, 16.0, 16.0);
        let b = verts_to_bytes(&verts);
        assert_eq!(b.len(), 64);
    }
}
