// Pure math and SDF helpers for the render module.

/// Smoothstep easing used for SDF antialiasing.
pub fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Rounded-rect signed distance function.
pub fn sdf_rrect(px: f32, py: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let dx = (px - cx).abs() - hw + r;
    let dy = (py - cy).abs() - hh + r;
    dx.max(0.0).hypot(dy.max(0.0)) + dx.min(0.0).max(dy.min(0.0)) - r
}

/// Arc ring SDF helper used for headphone / band shapes.
pub fn sdf_arc(px: f32, py: f32, cx: f32, cy: f32, r: f32, w: f32, bottom: bool) -> f32 {
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

/// Rasterize an SDF into an RGBA byte buffer (white RGB, alpha SDF).
pub fn rasterize<F: Fn(f32, f32) -> f32>(size: u32, sdf_fn: F) -> Vec<u8> {
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
