use std::fs;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

use crate::compressors::compress_dir;
use crate::error::AppError;
use crate::file::storage::{get_storage_dir, move_file_with_paths, move_files_with_processor, write_file};
use crate::file::utils::clean_name;
use crate::image::handling::{store_image_and_create_thumbnail};
use crate::models::{CompressionType, Release, StlModel};
use crate::settings::SETTINGS_CACHE;

#[tauri::command]
#[specta::specta]
pub async fn store_model_file(
    app_handle: AppHandle,
    file_data: Vec<u8>,
    file_name: String,
    model_name: String,
) -> Result<String, AppError> {
    let settings = SETTINGS_CACHE.lock()
        .map_err(|e| AppError::ConfigError(format!("{}", e)))?;
    let temp_dir = get_storage_dir(&app_handle, settings.scratch_dir.clone(), "temp".to_string())?;
    let clean_model_name = clean_name(&model_name);
    let new_filename = format!("{}-{}", clean_model_name, file_name);
    let file_path = temp_dir.join(new_filename);
    write_file(&file_path, &file_data)?;

    // Return the file path as a string
    Ok(file_path.to_string_lossy().to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn save_model(
    app_handle: AppHandle,
    model: StlModel,
) -> Result<(), AppError> {
    let settings = SETTINGS_CACHE.lock()
        .map_err(|e| AppError::ConfigError(format!("{}", e)))?;

    // Get the current release directory
    let releases_dir = get_storage_dir(&app_handle, settings.scratch_dir.clone(), "releases".to_string())?;
    let release_entries = fs::read_dir(&releases_dir)?;
    let release_dir = release_entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .ok_or_else(|| AppError::NotFoundError("No active release found. Create a release first.".to_string()))?
        .path();

    // Create models directory inside the release
    let models_dir = release_dir.join("models");
    fs::create_dir_all(&models_dir)?;

    // Handle group folder structure
    let model_dir = if let Some(group_name) = &model.group {
        let clean_group_name = clean_name(group_name);
        let group_dir = models_dir.join(&clean_group_name);
        fs::create_dir_all(&group_dir)?;
        group_dir.join(clean_name(&model.model_name))
    } else {
        models_dir.join(clean_name(&model.model_name))
    };

    if model_dir.exists() {
        fs::remove_dir_all(&model_dir)?;
    }

    fs::create_dir_all(&model_dir)?;

    // Move files and get relative paths
    let relative_model_file_paths = move_files_with_processor(&model.model_files, &model_dir, move_file_with_paths)?;
    let relative_image_paths = move_files_with_processor(&model.images, &model_dir, store_image_and_create_thumbnail)?;

    // Create a model with relative paths instead of absolute
    let model_with_relative_paths = StlModel {
        model_name: model.model_name,
        description: model.description,
        tags: model.tags,
        group: model.group,
        images: relative_image_paths,
        model_files: relative_model_file_paths,
    };

    // Save the JSON with relative paths
    let model_json = serde_json::to_string_pretty(&model_with_relative_paths)?;
    fs::write(&model_dir.join("model.json"), model_json)?;

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn create_release(
    app_handle: AppHandle,
    release: Release,
) -> Result<(), AppError> {
    let settings = SETTINGS_CACHE.lock()
        .map_err(|e| AppError::ConfigError(format!("{}", e)))?;
    let release_dir = get_storage_dir(&app_handle, settings.scratch_dir.clone(), "releases".to_string())?;
    let release_name = clean_name(&release.name);
    let designer_name = clean_name(&release.designer);

    let release_date = {
        let date_parts = release.date.split('/').collect::<Vec<_>>();
        match date_parts.as_slice() {
            [month, year] => format!("{:02}-{}", month.parse::<u8>().unwrap_or(1), year),
            _ => "01-2023".to_string(), // Default if parsing fails
        }
    };

    let release_dir_name = format!("{}-{}-{}", designer_name, release_date, release_name);
    let release_dir = release_dir.join(&release_dir_name);

    if release_dir.exists() {
        fs::remove_dir_all(&release_dir)?;
    }

    fs::create_dir_all(&release_dir)?;

    let release_json = serde_json::to_string_pretty(&release)?;

    fs::write(&release_dir.join("release.json"), release_json)?;

    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn finalize_release(
    app_handle: AppHandle,
    release_name: String,
) -> Result<(), AppError> {
    let settings = SETTINGS_CACHE.lock()
        .map_err(|e| AppError::ConfigError(format!("{}", e)))?;

    let releases_dir = get_storage_dir(&app_handle, settings.scratch_dir.clone(), "releases".to_string())?;

    // Find the release directory by name
    let entries = fs::read_dir(&releases_dir)?;

    // Find the directory that contains the release name
    let release_dir = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        .find(|e| e.file_name().to_string_lossy().contains(&release_name))
        .ok_or_else(|| AppError::NotFoundError(format!("Release '{}' not found", release_name)))?
        .path();

    // Determine the target directory from settings or use default
    let target_dir = if let Some(dir) = &settings.target_dir {
        PathBuf::from(dir)
    } else {
        app_handle.path().app_data_dir()?
            .join("exports")
    };

    fs::create_dir_all(&target_dir)?;

    // Get release directory name for the archive
    let release_dir_name = release_dir
        .file_name()
        .ok_or_else(|| AppError::ConfigError("Invalid release directory name".to_string()))?
        .to_string_lossy()
        .to_string();

    let archive_path = target_dir.join(format!("{}.zip", release_dir_name));

    // Compress the entire release directory
    compress_dir(
        &release_dir,
        &archive_path,
        &app_handle,
        settings.compression_type.clone().unwrap_or(CompressionType::Zip),
        settings.chunk_size,
    )?;

    Ok(())
}

