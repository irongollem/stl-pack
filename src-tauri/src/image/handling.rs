use std::path::{Path, PathBuf};

use tauri::AppHandle;

use crate::error::AppError;
use crate::file::storage::{get_storage_dir, move_file_with_paths, write_file};
use crate::image::crop::generate_smart_thumbnail;
use crate::settings::SETTINGS_CACHE;
use crate::file::utils::{clean_name};

pub fn store_image_and_create_thumbnail(
    source_path: &str,
    target_path: &Path,
) -> Result<(PathBuf, String), AppError> {
    let (file_path, relative_path) = move_file_with_paths(source_path, target_path)?;

    generate_smart_thumbnail(file_path.as_path(), 512)
        .map_err(|e| AppError::ImageProcessingError(format!("Failed to generate thumbnail: {}", e)))?;

    Ok((file_path, relative_path))
}

#[tauri::command]
#[specta::specta]
pub async fn store_image(
    app_handle: AppHandle,
    image_data: Vec<u8>,
    image_name: String,
    model_name: String,
    image_index: u32,
) -> Result<String, AppError> {
    let settings = SETTINGS_CACHE.lock()
        .map_err(|e| AppError::ConfigError(format!("{}", e)))?;
    let temp_dir = get_storage_dir(&app_handle, settings.scratch_dir.clone(), "temp".to_string())?;
    let clean_model_name = clean_name(&model_name);

    let extension = Path::new(&image_name)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("");

    let new_filename = if image_index == 0 {
        format!("{}-main.{}", clean_model_name, extension)
    } else {
        format!("{}-detail_{}.{}", clean_model_name, image_index, extension)
    };

    let file_path = temp_dir.join(new_filename);
    write_file(&file_path, &image_data)
}
