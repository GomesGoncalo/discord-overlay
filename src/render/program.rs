// Shader program wrapper utilities — encapsulate program creation and common uniform setters.

use std::marker::PhantomData;

/// Generic wrapper for the main rounded-rect shader program.
/// Parameterized over a `G` which implements both `compile::GlInterface` and `program_gl::ProgramGl`.
pub struct MainProgram<G>
where
    G: crate::render::program_gl::ProgramGl,
{
    program: <G as crate::render::compile::GlInterface>::Program,
    locs: crate::render::program_locations::MainProgramLocations<
        <G as crate::render::program_gl::ProgramGl>::UniformLocation,
    >,
    _marker: PhantomData<G>,
}

impl<G> MainProgram<G>
where
    G: crate::render::program_gl::ProgramGl,
    <G as crate::render::compile::GlInterface>::Program: Copy,
{
    /// Compile, link and query locations for the main program.
    pub unsafe fn new(gl: &G, vert_src: &str, frag_src: &str) -> Self {
        let prog = crate::render::compile::compile_program_generic(gl, vert_src, frag_src);
        let locs = crate::render::program_locations::query_main_program_generic(gl, &prog);
        MainProgram {
            program: prog,
            locs,
            _marker: PhantomData,
        }
    }

    pub unsafe fn use_program(&self, gl: &G) {
        gl.use_program(&self.program);
    }

    pub unsafe fn set_color(&self, gl: &G, color: [f32; 4]) {
        gl.uniform_4_f32(&self.locs.loc_color, color[0], color[1], color[2], color[3]);
    }

    pub unsafe fn set_size(&self, gl: &G, w: f32, h: f32) {
        gl.uniform_2_f32(&self.locs.loc_size, w, h);
    }

    pub unsafe fn set_radius(&self, gl: &G, r: f32) {
        gl.uniform_1_f32(&self.locs.loc_radius, r);
    }

    /// Accessor for underlying program id. Requires the Program type to be Copy.
    pub fn id(&self) -> <G as super::compile::GlInterface>::Program {
        self.program
    }

    /// Accessor for queried locations.
    #[allow(dead_code)]
    pub fn locs(
        &self,
    ) -> &crate::render::program_locations::MainProgramLocations<
        <G as crate::render::program_gl::ProgramGl>::UniformLocation,
    > {
        &self.locs
    }
}

/// Generic wrapper for simple programs that expose a single `u_opacity` uniform (icons, avatars).
pub struct OpacityProgram<G>
where
    G: crate::render::program_gl::ProgramGl,
{
    program: <G as crate::render::compile::GlInterface>::Program,
    loc_opacity: <G as crate::render::program_gl::ProgramGl>::UniformLocation,
    _marker: PhantomData<G>,
}

impl<G> OpacityProgram<G>
where
    G: crate::render::program_gl::ProgramGl,
    <G as crate::render::compile::GlInterface>::Program: Copy,
{
    pub unsafe fn new(gl: &G, vert_src: &str, frag_src: &str) -> Self {
        let prog = crate::render::compile::compile_program_generic(gl, vert_src, frag_src);
        let loc_opacity = crate::render::program_locations::query_opacity_generic(gl, &prog);
        OpacityProgram {
            program: prog,
            loc_opacity,
            _marker: PhantomData,
        }
    }

    #[allow(dead_code)]
    pub unsafe fn use_program(&self, gl: &G) {
        gl.use_program(&self.program);
    }

    pub unsafe fn set_opacity(&self, gl: &G, o: f32) {
        gl.uniform_1_f32(&self.loc_opacity, o);
    }

    /// Accessor for underlying program id.
    #[allow(dead_code)]
    pub fn id(&self) -> <G as crate::render::compile::GlInterface>::Program {
        self.program
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    // Minimal Mock used to test wrappers. Implements both compile::GlInterface and
    // program_gl::ProgramGl so it can be used with the generic wrappers.
    struct MockGl {
        vs_status: RefCell<bool>,
        fs_status: RefCell<bool>,
        program_status: RefCell<bool>,
        vs_log: RefCell<String>,
        fs_log: RefCell<String>,
        prog_log: RefCell<String>,
        next_shader: RefCell<u32>,
        next_program: RefCell<u32>,
        used_programs: RefCell<Vec<u32>>,
        uniform_calls: RefCell<Vec<(u32, Vec<f32>)>>,
    }

    impl MockGl {
        fn new() -> Self {
            MockGl {
                vs_status: RefCell::new(true),
                fs_status: RefCell::new(true),
                program_status: RefCell::new(true),
                vs_log: RefCell::new(String::new()),
                fs_log: RefCell::new(String::new()),
                prog_log: RefCell::new(String::new()),
                next_shader: RefCell::new(1),
                next_program: RefCell::new(100),
                used_programs: RefCell::new(vec![]),
                uniform_calls: RefCell::new(vec![]),
            }
        }
    }

    impl crate::render::compile::GlInterface for MockGl {
        type Shader = u32;
        type Program = u32;

        fn create_shader(&self, _shader_type: u32) -> Result<Self::Shader, String> {
            let mut s = self.next_shader.borrow_mut();
            let val = *s;
            *s += 1;
            Ok(val)
        }

        fn shader_source(&self, _shader: &Self::Shader, _src: &str) {}

        fn compile_shader(&self, _shader: &Self::Shader) {}

        fn get_shader_compile_status(&self, shader: &Self::Shader) -> bool {
            if *shader % 2 == 0 {
                *self.fs_status.borrow()
            } else {
                *self.vs_status.borrow()
            }
        }

        fn get_shader_info_log(&self, shader: &Self::Shader) -> String {
            if *shader % 2 == 0 {
                self.fs_log.borrow().clone()
            } else {
                self.vs_log.borrow().clone()
            }
        }

        fn create_program(&self) -> Result<Self::Program, String> {
            let mut p = self.next_program.borrow_mut();
            let val = *p;
            *p += 1;
            Ok(val)
        }

        fn attach_shader(&self, _program: &Self::Program, _shader: &Self::Shader) {}

        fn link_program(&self, _program: &Self::Program) {}

        fn get_program_link_status(&self, _program: &Self::Program) -> bool {
            *self.program_status.borrow()
        }

        fn get_program_info_log(&self, _program: &Self::Program) -> String {
            self.prog_log.borrow().clone()
        }

        fn delete_shader(&self, _shader: &Self::Shader) {}
    }

    impl crate::render::program_gl::ProgramGl for MockGl {
        type UniformLocation = u32;

        fn use_program(&self, program: &<Self as crate::render::compile::GlInterface>::Program) {
            self.used_programs.borrow_mut().push(*program);
        }

        fn uniform_4_f32(&self, loc: &Self::UniformLocation, x: f32, y: f32, z: f32, w: f32) {
            self.uniform_calls
                .borrow_mut()
                .push((*loc, vec![x, y, z, w]));
        }

        fn uniform_2_f32(&self, loc: &Self::UniformLocation, x: f32, y: f32) {
            self.uniform_calls.borrow_mut().push((*loc, vec![x, y]));
        }

        fn uniform_1_f32(&self, loc: &Self::UniformLocation, x: f32) {
            self.uniform_calls.borrow_mut().push((*loc, vec![x]));
        }

        fn get_uniform_location(
            &self,
            _program: &<Self as crate::render::compile::GlInterface>::Program,
            name: &str,
        ) -> Option<Self::UniformLocation> {
            match name {
                "u_color" => Some(10),
                "u_size" => Some(11),
                "u_radius" => Some(12),
                "u_opacity" => Some(20),
                _ => None,
            }
        }

        fn get_attrib_location(
            &self,
            _program: &<Self as crate::render::compile::GlInterface>::Program,
            name: &str,
        ) -> Option<u32> {
            match name {
                "a_position" => Some(1),
                "a_local" => Some(2),
                _ => None,
            }
        }
    }

    #[test]
    fn main_and_opacity_program_setters() {
        let mock = MockGl::new();
        let main_prog =
            unsafe { MainProgram::<MockGl>::new(&mock, "void main() {}", "void main() {}") };
        let opacity_prog =
            unsafe { OpacityProgram::<MockGl>::new(&mock, "void main() {}", "void main() {}") };

        unsafe { main_prog.use_program(&mock) };
        assert!(mock.used_programs.borrow().contains(&main_prog.id()));

        unsafe { main_prog.set_color(&mock, [1.0, 0.0, 0.0, 1.0]) };
        let calls = mock.uniform_calls.borrow();
        assert_eq!(calls.last().unwrap().0, 10);
        assert_eq!(calls.last().unwrap().1, vec![1.0, 0.0, 0.0, 1.0]);
        drop(calls);

        unsafe { main_prog.set_size(&mock, 32.0, 16.0) };
        let calls = mock.uniform_calls.borrow();
        assert_eq!(calls.last().unwrap().0, 11);
        drop(calls);

        unsafe { main_prog.set_radius(&mock, 5.0) };
        let calls = mock.uniform_calls.borrow();
        assert_eq!(calls.last().unwrap().0, 12);
        drop(calls);

        unsafe { opacity_prog.set_opacity(&mock, 0.5) };
        let calls = mock.uniform_calls.borrow();
        assert_eq!(calls.last().unwrap().0, 20);
        drop(calls);
    }
}
