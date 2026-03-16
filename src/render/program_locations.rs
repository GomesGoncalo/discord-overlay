#![allow(dead_code)]
// Centralized shader attribute/uniform names and query helpers.

pub const ATTR_POSITION: &str = "a_position";
pub const ATTR_LOCAL: &str = "a_local";
pub const UNIFORM_COLOR: &str = "u_color";
pub const UNIFORM_SIZE: &str = "u_size";
pub const UNIFORM_RADIUS: &str = "u_radius";
pub const UNIFORM_OPACITY: &str = "u_opacity";

/// Locations for the main rounded-rect shader program.
pub struct MainProgramLocations<U> {
    pub loc_pos: u32,
    pub loc_local: u32,
    pub loc_color: U,
    pub loc_size: U,
    pub loc_radius: U,
}

/// Generic query implementation that uses the ProgramGl abstraction so tests can supply a mock GL.
pub unsafe fn query_main_program_generic<G: super::program_gl::ProgramGl>(
    gl: &G,
    program: &<G as super::compile::GlInterface>::Program,
) -> MainProgramLocations<G::UniformLocation> {
    let loc_color = gl.get_uniform_location(program, UNIFORM_COLOR).unwrap();
    let loc_size = gl.get_uniform_location(program, UNIFORM_SIZE).unwrap();
    let loc_radius = gl.get_uniform_location(program, UNIFORM_RADIUS).unwrap();
    let loc_pos = gl.get_attrib_location(program, ATTR_POSITION).unwrap();
    let loc_local = gl.get_attrib_location(program, ATTR_LOCAL).unwrap();
    MainProgramLocations {
        loc_pos,
        loc_local,
        loc_color,
        loc_size,
        loc_radius,
    }
}

/// Convenience wrapper used by production code (glow::Context).
#[cfg(not(test))]
pub unsafe fn query_main_program(
    gl: &glow::Context,
    program: glow::NativeProgram,
) -> MainProgramLocations<glow::UniformLocation> {
    query_main_program_generic(gl, &program)
}

/// Generic query for the standard opacity uniform used by icon/avatar shaders.
pub unsafe fn query_opacity_generic<G: super::program_gl::ProgramGl>(
    gl: &G,
    program: &<G as super::compile::GlInterface>::Program,
) -> G::UniformLocation {
    gl.get_uniform_location(program, UNIFORM_OPACITY).unwrap()
}

#[cfg(not(test))]
pub unsafe fn query_opacity(
    gl: &glow::Context,
    program: glow::NativeProgram,
) -> glow::UniformLocation {
    query_opacity_generic(gl, &program)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attr_and_uniform_names() {
        assert_eq!(ATTR_POSITION, "a_position");
        assert_eq!(ATTR_LOCAL, "a_local");
        assert_eq!(UNIFORM_COLOR, "u_color");
        assert_eq!(UNIFORM_SIZE, "u_size");
        assert_eq!(UNIFORM_RADIUS, "u_radius");
        assert_eq!(UNIFORM_OPACITY, "u_opacity");
    }
}
