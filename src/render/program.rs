// Shader program wrapper utilities — encapsulate program creation and common uniform setters.

#[cfg(not(test))]
use glow::HasContext;

#[cfg(not(test))]
/// Wrapper for the main rounded-rect shader program (holds program id + queried locations).
pub struct MainProgram {
    pub program: glow::NativeProgram,
    pub locs: super::program_locations::MainProgramLocations,
}

#[cfg(not(test))]
impl MainProgram {
    /// Compile, link and query locations for the main program.
    pub unsafe fn new(gl: &glow::Context, vert_src: &str, frag_src: &str) -> Self {
        let prog = super::compile::compile_program(gl, vert_src, frag_src);
        let locs = super::program_locations::query_main_program(gl, prog);
        MainProgram {
            program: prog,
            locs,
        }
    }

    pub unsafe fn use_program(&self, gl: &glow::Context) {
        gl.use_program(Some(self.program));
    }

    pub unsafe fn set_color(&self, gl: &glow::Context, color: [f32; 4]) {
        gl.uniform_4_f32(
            Some(&self.locs.loc_color),
            color[0],
            color[1],
            color[2],
            color[3],
        );
    }

    pub unsafe fn set_size(&self, gl: &glow::Context, w: f32, h: f32) {
        gl.uniform_2_f32(Some(&self.locs.loc_size), w, h);
    }

    pub unsafe fn set_radius(&self, gl: &glow::Context, r: f32) {
        gl.uniform_1_f32(Some(&self.locs.loc_radius), r);
    }
}

#[cfg(not(test))]
/// Wrapper for simple programs that expose a single `u_opacity` uniform (icons, avatars).
pub struct OpacityProgram {
    pub program: glow::NativeProgram,
    pub loc_opacity: glow::UniformLocation,
}

#[cfg(not(test))]
impl OpacityProgram {
    pub unsafe fn new(gl: &glow::Context, vert_src: &str, frag_src: &str) -> Self {
        let prog = super::compile::compile_program(gl, vert_src, frag_src);
        let loc_opacity = super::program_locations::query_opacity(gl, prog);
        OpacityProgram {
            program: prog,
            loc_opacity,
        }
    }

    pub unsafe fn use_program(&self, gl: &glow::Context) {
        gl.use_program(Some(self.program));
    }

    pub unsafe fn set_opacity(&self, gl: &glow::Context, o: f32) {
        gl.uniform_1_f32(Some(&self.loc_opacity), o);
    }
}
