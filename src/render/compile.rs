// Shader compile/link helper for render module.

// Abstraction over GLES API used by the compile helper. This allows unit-testing the
// compile/link logic without a real GL context by providing a mock implementation.
pub trait GlInterface {
    type Shader;
    type Program;

    fn create_shader(&self, shader_type: u32) -> Result<Self::Shader, String>;
    fn shader_source(&self, shader: &Self::Shader, src: &str);
    fn compile_shader(&self, shader: &Self::Shader);
    fn get_shader_compile_status(&self, shader: &Self::Shader) -> bool;
    fn get_shader_info_log(&self, shader: &Self::Shader) -> String;
    fn create_program(&self) -> Result<Self::Program, String>;
    fn attach_shader(&self, program: &Self::Program, shader: &Self::Shader);
    fn link_program(&self, program: &Self::Program);
    fn get_program_link_status(&self, program: &Self::Program) -> bool;
    fn get_program_info_log(&self, program: &Self::Program) -> String;
    fn delete_shader(&self, shader: &Self::Shader);
}

impl GlInterface for glow::Context {
    type Shader = glow::NativeShader;
    type Program = glow::NativeProgram;

    fn create_shader(&self, shader_type: u32) -> Result<Self::Shader, String> {
        unsafe { glow::HasContext::create_shader(self, shader_type) }
    }

    fn shader_source(&self, shader: &Self::Shader, src: &str) {
        unsafe { glow::HasContext::shader_source(self, *shader, src) }
    }

    fn compile_shader(&self, shader: &Self::Shader) {
        unsafe { glow::HasContext::compile_shader(self, *shader) }
    }

    fn get_shader_compile_status(&self, shader: &Self::Shader) -> bool {
        unsafe { glow::HasContext::get_shader_compile_status(self, *shader) }
    }

    fn get_shader_info_log(&self, shader: &Self::Shader) -> String {
        unsafe { glow::HasContext::get_shader_info_log(self, *shader) }
    }

    fn create_program(&self) -> Result<Self::Program, String> {
        unsafe { glow::HasContext::create_program(self) }
    }

    fn attach_shader(&self, program: &Self::Program, shader: &Self::Shader) {
        unsafe { glow::HasContext::attach_shader(self, *program, *shader) }
    }

    fn link_program(&self, program: &Self::Program) {
        unsafe { glow::HasContext::link_program(self, *program) }
    }

    fn get_program_link_status(&self, program: &Self::Program) -> bool {
        unsafe { glow::HasContext::get_program_link_status(self, *program) }
    }

    fn get_program_info_log(&self, program: &Self::Program) -> String {
        unsafe { glow::HasContext::get_program_info_log(self, *program) }
    }

    fn delete_shader(&self, shader: &Self::Shader) {
        unsafe { glow::HasContext::delete_shader(self, *shader) }
    }
}

/// Generic compile/link implementation parameterized over a GlInterface.
pub fn compile_program_generic<G: GlInterface>(
    gl: &G,
    vert_src: &str,
    frag_src: &str,
) -> G::Program {
    let vs = gl
        .create_shader(glow::VERTEX_SHADER)
        .expect("create_shader failed");
    gl.shader_source(&vs, vert_src);
    gl.compile_shader(&vs);
    assert!(
        gl.get_shader_compile_status(&vs),
        "vertex shader: {}",
        gl.get_shader_info_log(&vs)
    );

    let fs = gl
        .create_shader(glow::FRAGMENT_SHADER)
        .expect("create_shader failed");
    gl.shader_source(&fs, frag_src);
    gl.compile_shader(&fs);
    assert!(
        gl.get_shader_compile_status(&fs),
        "fragment shader: {}",
        gl.get_shader_info_log(&fs)
    );

    let prog = gl.create_program().expect("create_program failed");
    gl.attach_shader(&prog, &vs);
    gl.attach_shader(&prog, &fs);
    gl.link_program(&prog);
    assert!(
        gl.get_program_link_status(&prog),
        "link: {}",
        gl.get_program_info_log(&prog)
    );
    gl.delete_shader(&vs);
    gl.delete_shader(&fs);
    prog
}

// Production wrapper used by the EGL code. Kept behind cfg(not(test)) so tests
// continue to use the generic/mockable implementation.
#[cfg(not(test))]
pub unsafe fn compile_program(
    gl: &glow::Context,
    vert_src: &str,
    frag_src: &str,
) -> glow::NativeProgram {
    compile_program_generic(gl, vert_src, frag_src)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct MockGl {
        vs_status: RefCell<bool>,
        fs_status: RefCell<bool>,
        program_status: RefCell<bool>,
        vs_log: RefCell<String>,
        fs_log: RefCell<String>,
        prog_log: RefCell<String>,
        next_shader: RefCell<u32>,
        next_program: RefCell<u32>,
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
            }
        }
    }

    impl GlInterface for MockGl {
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

    #[test]
    fn compile_program_generic_success() {
        let mock = MockGl::new();
        let prog = compile_program_generic(&mock, "void main() {}", "void main() {}");
        assert!(prog >= 100);
    }

    #[test]
    #[should_panic(expected = "fragment shader:")]
    fn compile_program_fragment_fail() {
        let mock = MockGl::new();
        *mock.fs_status.borrow_mut() = false;
        *mock.fs_log.borrow_mut() = "frag error".to_string();
        let _ = compile_program_generic(&mock, "void main() {}", "bad frag");
    }

    #[test]
    #[should_panic(expected = "vertex shader:")]
    fn compile_program_vertex_fail() {
        let mock = MockGl::new();
        *mock.vs_status.borrow_mut() = false;
        *mock.vs_log.borrow_mut() = "vs error".to_string();
        let _ = compile_program_generic(&mock, "bad vert", "void main() {}");
    }

    #[test]
    #[should_panic(expected = "link:")]
    fn compile_program_link_fail() {
        let mock = MockGl::new();
        *mock.program_status.borrow_mut() = false;
        *mock.prog_log.borrow_mut() = "link error".to_string();
        let _ = compile_program_generic(&mock, "void main() {}", "void main() {}");
    }
}
