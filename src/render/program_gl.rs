// Program-level GL abstraction used by shader program wrappers.
// Allows unit-testing wrappers with a mock GL and keeps production code tied to glow.

/// ProgramGl extends the compile::GlInterface so implementations also provide shader
/// creation/linking methods (Program type) while adding uniform and location helpers.
pub trait ProgramGl: super::compile::GlInterface {
    type UniformLocation;

    fn use_program(&self, program: &<Self as super::compile::GlInterface>::Program);
    fn uniform_4_f32(&self, loc: &Self::UniformLocation, x: f32, y: f32, z: f32, w: f32);
    fn uniform_2_f32(&self, loc: &Self::UniformLocation, x: f32, y: f32);
    fn uniform_1_f32(&self, loc: &Self::UniformLocation, x: f32);

    fn get_uniform_location(
        &self,
        program: &<Self as super::compile::GlInterface>::Program,
        name: &str,
    ) -> Option<Self::UniformLocation>;
    fn get_attrib_location(
        &self,
        program: &<Self as super::compile::GlInterface>::Program,
        name: &str,
    ) -> Option<u32>;
}

#[cfg(not(test))]
impl ProgramGl for glow::Context {
    type UniformLocation = glow::UniformLocation;

    fn use_program(&self, program: &<Self as super::compile::GlInterface>::Program) {
        unsafe { glow::HasContext::use_program(self, Some(*program)) }
    }

    fn uniform_4_f32(&self, loc: &Self::UniformLocation, x: f32, y: f32, z: f32, w: f32) {
        unsafe { glow::HasContext::uniform_4_f32(self, Some(loc), x, y, z, w) }
    }

    fn uniform_2_f32(&self, loc: &Self::UniformLocation, x: f32, y: f32) {
        unsafe { glow::HasContext::uniform_2_f32(self, Some(loc), x, y) }
    }

    fn uniform_1_f32(&self, loc: &Self::UniformLocation, x: f32) {
        unsafe { glow::HasContext::uniform_1_f32(self, Some(loc), x) }
    }

    fn get_uniform_location(
        &self,
        program: &<Self as super::compile::GlInterface>::Program,
        name: &str,
    ) -> Option<Self::UniformLocation> {
        unsafe { glow::HasContext::get_uniform_location(self, *program, name) }
    }

    fn get_attrib_location(
        &self,
        program: &<Self as super::compile::GlInterface>::Program,
        name: &str,
    ) -> Option<u32> {
        unsafe { glow::HasContext::get_attrib_location(self, *program, name) }
    }
}
