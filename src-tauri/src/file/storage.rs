use crate::error::AppError;
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

pub fn get_dir(
    app_handle: &AppHandle,
    dir_name: String,
    scratch_dir: Option<String>,
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

pub fn rename_image(model_name: &str, original_path: &Path, index: usize) -> String {
    let extension = original_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("");

    if index == 0 {
        format!("{}-main.{}", model_name, extension)
    } else {
        format!("{}-detail_{}.{}", model_name, index, extension)
    }
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

pub fn copy_images(
    image_paths: &[String],
    model_folder: &Path,
    clean_model_name: &str,
) -> Result<Vec<String>, AppError> {
    let mut copied_images = Vec::new();

    for (i, path) in image_paths.iter().enumerate() {
        let source_path = Path::new(path);
        let new_name = rename_image(clean_model_name, source_path, i);
        let destination_path = model_folder.join(&new_name);

        fs::copy(source_path, &destination_path)
            .map_err(|e| AppError::IoError(format!("failed to copy image; {}", e)))?;
        copied_images.push(destination_path.to_string_lossy().into_owned());
    }

    Ok(copied_images)
}

pub fn copy_files(file_paths: &[String], model_folder: &Path) -> Result<Vec<String>, AppError> {
    let mut copied_files = Vec::new();

    for path in file_paths {
        let source_path = Path::new(path);
        let file_name = source_path
            .file_name()
            .ok_or_else(|| AppError::IoError(format!("Invalid file path: {}", path)))?;

        let destination_folder = get_destination_folder(model_folder, source_path);
        let destination_path = destination_folder.join(file_name);

        fs::copy(source_path, &destination_path)
            .map_err(|e| AppError::IoError(format!("failed to copy file; {}", e)))?;
        copied_files.push(destination_path.to_string_lossy().into_owned());
    }

    Ok(copied_files)
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

pub fn convert_to_relative_paths(
    paths: &[String],
    base_dir: &Path,
) -> Result<Vec<String>, AppError> {
    paths
        .iter()
        .map(|path| {
            convert_to_relative_path(path, base_dir).map_err(|e| {
                AppError::IoError(format!("Failed to convert path to relative: {}", e))
            })
        })
        .collect()
}
