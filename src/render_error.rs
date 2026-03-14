//! Error types for rendering operations.

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
    ImageDecodeFailed { user_id: String, error: String },
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
