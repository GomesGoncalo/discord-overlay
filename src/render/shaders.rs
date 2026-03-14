// ─── GLSL shaders (GLES2 / #version 100) ─────────────────────────────────────

#[cfg(not(test))]
pub const VERT_SRC: &str = r"
attribute vec2 a_position;
attribute vec2 a_local;
varying vec2 v_local;
void main() {
    gl_Position = vec4(a_position, 0.0, 1.0);
    v_local = a_local;
}
";

/// Rounded-rectangle SDF shader used for button backgrounds and the drag handle.
#[cfg(not(test))]
pub const FRAG_SRC: &str = r"
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
#[cfg(not(test))]
pub const ICON_FRAG_SRC: &str = r"
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
#[cfg(not(test))]
pub const AVATAR_FRAG_SRC: &str = r"
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
