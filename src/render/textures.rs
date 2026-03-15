#[cfg(not(test))]
use glow::HasContext;

#[cfg(not(test))]
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

#[cfg(not(test))]
pub unsafe fn upload_texture(gl: &glow::Context, pixels: &[u8], size: u32) -> glow::NativeTexture {
    upload_texture_wh(gl, pixels, size, size)
}
