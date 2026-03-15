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
                tracing::info!("Loaded font: {p}");
                return Some(font);
            }
        }
    }
    None
}
