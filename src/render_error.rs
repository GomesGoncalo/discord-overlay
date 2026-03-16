//! Error types for rendering operations.

use crate::discord::UserId;
use std::fmt;

/// Errors that can occur during rendering setup or operations.
#[derive(Debug)]
#[allow(dead_code)]
pub enum RenderError {
    /// EGL display not found or initialization failed.
    EglDisplayNotFound(String),
    /// EGL config selection failed.
    EglConfigSelectionFailed(String),
    /// EGL context creation failed.
    EglContextCreationFailed(String),
    /// EGL surface creation failed.
    EglSurfaceCreationFailed(String),
    /// OpenGL shader compilation failed.
    ShaderCompilationFailed { stage: &'static str, log: String },
    /// OpenGL program linking failed.
    ProgramLinkingFailed(String),
    /// Texture creation failed.
    TextureCreationFailed(String),
    /// Buffer object creation failed.
    BufferCreationFailed(String),
    /// Image decoding failed.
    ImageDecodeFailed { user_id: UserId, error: String },
    /// Font rendering failed.
    FontRenderingFailed(String),
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EglDisplayNotFound(e) => write!(f, "EGL display not found: {}", e),
            Self::EglConfigSelectionFailed(e) => write!(f, "EGL config selection failed: {}", e),
            Self::EglContextCreationFailed(e) => write!(f, "EGL context creation failed: {}", e),
            Self::EglSurfaceCreationFailed(e) => write!(f, "EGL surface creation failed: {}", e),
            Self::ShaderCompilationFailed { stage, log } => {
                write!(f, "{} shader compilation failed: {}", stage, log)
            }
            Self::ProgramLinkingFailed(e) => write!(f, "Program linking failed: {}", e),
            Self::TextureCreationFailed(e) => write!(f, "Texture creation failed: {}", e),
            Self::BufferCreationFailed(e) => write!(f, "Buffer creation failed: {}", e),
            Self::ImageDecodeFailed { user_id, error } => {
                write!(
                    f,
                    "Failed to decode avatar image for user {}: {}",
                    user_id, error
                )
            }
            Self::FontRenderingFailed(e) => write!(f, "Font rendering failed: {}", e),
        }
    }
}

impl std::error::Error for RenderError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::UserId;

    #[test]
    fn display_egl_display_not_found() {
        let e = RenderError::EglDisplayNotFound("no display".into());
        assert_eq!(format!("{e}"), "EGL display not found: no display");
    }

    #[test]
    fn display_egl_config_selection_failed() {
        let e = RenderError::EglConfigSelectionFailed("bad config".into());
        assert_eq!(format!("{e}"), "EGL config selection failed: bad config");
    }

    #[test]
    fn display_egl_context_creation_failed() {
        let e = RenderError::EglContextCreationFailed("ctx err".into());
        assert_eq!(format!("{e}"), "EGL context creation failed: ctx err");
    }

    #[test]
    fn display_egl_surface_creation_failed() {
        let e = RenderError::EglSurfaceCreationFailed("surf err".into());
        assert_eq!(format!("{e}"), "EGL surface creation failed: surf err");
    }

    #[test]
    fn display_shader_compilation_failed() {
        let e = RenderError::ShaderCompilationFailed {
            stage: "vertex",
            log: "undefined var".into(),
        };
        assert_eq!(
            format!("{e}"),
            "vertex shader compilation failed: undefined var"
        );
    }

    #[test]
    fn display_program_linking_failed() {
        let e = RenderError::ProgramLinkingFailed("link err".into());
        assert_eq!(format!("{e}"), "Program linking failed: link err");
    }

    #[test]
    fn display_texture_creation_failed() {
        let e = RenderError::TextureCreationFailed("tex err".into());
        assert_eq!(format!("{e}"), "Texture creation failed: tex err");
    }

    #[test]
    fn display_buffer_creation_failed() {
        let e = RenderError::BufferCreationFailed("buf err".into());
        assert_eq!(format!("{e}"), "Buffer creation failed: buf err");
    }

    #[test]
    fn display_image_decode_failed() {
        let e = RenderError::ImageDecodeFailed {
            user_id: UserId::from("u42"),
            error: "bad png".into(),
        };
        assert_eq!(
            format!("{e}"),
            "Failed to decode avatar image for user u42: bad png"
        );
    }

    #[test]
    fn display_font_rendering_failed() {
        let e = RenderError::FontRenderingFailed("missing glyph".into());
        assert_eq!(format!("{e}"), "Font rendering failed: missing glyph");
    }

    #[test]
    fn render_error_is_std_error() {
        let e = RenderError::ProgramLinkingFailed("x".into());
        let _: &dyn std::error::Error = &e;
    }
}
