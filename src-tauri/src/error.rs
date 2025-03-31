use std::fmt;
use std::io;
use serde::Serialize;
use specta::Type;

#[derive(Debug, Type, Serialize)]
pub enum AppError {
    IoError(String),
    JsonError(String),
    ImageProcessingError(String),
    FileProcessingError(String),
    ConfigError(String),
    NotFoundError(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IoError(e) => write!(f, "IO error: {}", e),
            Self::JsonError(e) => write!(f, "JSON error: {}", e),
            Self::ImageProcessingError(msg) => write!(f, "Image processing error: {}", msg),
            Self::FileProcessingError(msg) => write!(f, "File processing error: {}", msg),
            Self::ConfigError(msg) => write!(f, "Configuration error: {}", msg),
            Self::NotFoundError(msg) => write!(f, "Not found: {}", msg),
        }
    }
}

impl From<io::Error> for AppError {
    fn from(error: io::Error) -> Self {
        Self::IoError(error.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(error: serde_json::Error) -> Self {
        Self::JsonError(error.to_string())
    }
}

impl From<tauri::Error> for AppError {
    fn from(error: tauri::Error) -> Self {
        Self::IoError(error.to_string())
    }
}

// For Tauri command compatibility
impl Into<String> for AppError {
    fn into(self) -> String {
        self.to_string()
    }
}