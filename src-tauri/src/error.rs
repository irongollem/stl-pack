use serde::Serialize;
use specta::Type;
use std::fmt;
use std::io;

#[derive(Debug, Type, Serialize)]
#[non_exhaustive]
pub enum AppError {
    InvalidInput(String),
    IoError(String),
    JsonError(String),
    FileProcessingError(String),
    ConfigError(String),
    NotFoundError(String),
    ImageProcessingError(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(e) => write!(f, "Invalid input: {}", e),
            Self::IoError(e) => write!(f, "IO error: {}", e),
            Self::JsonError(e) => write!(f, "JSON error: {}", e),
            Self::FileProcessingError(msg) => write!(f, "File processing error: {}", msg),
            Self::ConfigError(msg) => write!(f, "Configuration error: {}", msg),
            Self::NotFoundError(msg) => write!(f, "Not found: {}", msg),
            Self::ImageProcessingError(msg) => write!(f, "Failed to process image: {}", msg),
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

impl From<AppError> for String {
    fn from(err: AppError) -> String {
        err.to_string()
    }
}

impl Drop for AppError {
    fn drop(&mut self) {}
}
