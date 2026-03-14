// Shader compile/link helper for render module.

#[cfg(not(test))]
use glow::HasContext;

#[cfg(not(test))]
pub unsafe fn compile_program(
    gl: &glow::Context,
    vert_src: &str,
    frag_src: &str,
) -> glow::NativeProgram {
    let vs = gl.create_shader(glow::VERTEX_SHADER).unwrap();
    gl.shader_source(vs, vert_src);
    gl.compile_shader(vs);
    assert!(
        gl.get_shader_compile_status(vs),
        "vertex shader: {}",
        gl.get_shader_info_log(vs)
    );

    let fs = gl.create_shader(glow::FRAGMENT_SHADER).unwrap();
    gl.shader_source(fs, frag_src);
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
}
