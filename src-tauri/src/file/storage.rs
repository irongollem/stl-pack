use std::fs;
use std::path::{Path, PathBuf};

use crate::error::AppError;
use tauri::{AppHandle, Manager};

pub fn get_storage_dir(
    app_handle: &AppHandle,
    scratch_dir: Option<String>,
    dir_name: String,
) -> Result<PathBuf, AppError> {
    let temp_dir = if let Some(dir) = scratch_dir {
        PathBuf::from(dir).join(dir_name)
    } else {
        let app_dir = app_handle.path().app_data_dir()?;
        app_dir.join(dir_name)
    };

    fs::create_dir_all(&temp_dir)?;

    Ok(temp_dir)
}

pub fn get_destination_folder(model_folder: &Path, file_path: &Path) -> PathBuf {
    let extension = file_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_lowercase();

    match extension.as_str() {
        "chitubox" => {
            let subfolder = model_folder.join("chitubox");
            fs::create_dir_all(&subfolder).unwrap_or_default();
            subfolder
        }
        "lys" => {
            let subfolder = model_folder.join("lychee");
            fs::create_dir_all(&subfolder).unwrap_or_default();
            subfolder
        }
        _ => model_folder.to_path_buf(),
    }
}

pub fn convert_to_relative_path(absolute_path: &str, base_dir: &Path) -> Result<String, AppError> {
    let base_str = base_dir.to_string_lossy().into_owned();
    let absolute_str = absolute_path.to_string();

    if absolute_str.starts_with(&base_str) {
        let rel_path = absolute_str[base_str.len()..]
            .trim_start_matches(std::path::MAIN_SEPARATOR)
            .to_string();
        Ok(rel_path)
    } else {
        Err(AppError::InvalidInput(format!(
            "Path '{}' is not within base directory '{}'",
            absolute_path,
            base_dir.display()
        )))
    }
}
