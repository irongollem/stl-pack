use super::storage;
use crate::compressors::compress_dir;
use crate::error::AppError;
use crate::file::utils::clean_name;
use crate::models::{CompressionType, Release, StlModel};
use crate::settings::SETTINGS_CACHE;
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

#[tauri::command]
#[specta::specta]
pub async fn save_model(
    app_handle: AppHandle,
    model: StlModel,
    release_dir: String,
    file_paths: Vec<String>,
    image_paths: Vec<String>,
) -> Result<StlModel, AppError> {
    let var_name = {
        let settings = SETTINGS_CACHE
            .lock()
            .map_err(|e| AppError::ConfigError(format!("{}", e)))?;
        settings.scratch_dir.clone()
    };
    let scratch_dir = var_name;

    let release_path = storage::get_dir(&app_handle, release_dir, scratch_dir)?;

    let clean_model_name = clean_name(&model.model_name);
    let model_folder = match model.group {
        Some(ref group_name) => {
            let clean_group_name = clean_name(group_name);
            let group_dir = release_path.join(&clean_group_name);
            fs::create_dir_all(&group_dir)?;
            group_dir.join(&clean_model_name)
        }
        None => release_path.join(&clean_model_name),
    };

    fs::create_dir_all(&model_folder)
        .map_err(|e| AppError::IoError(format!("failed to create model folder; {}", e)))?;

    fn copy_images(
        image_paths: &[String],
        model_folder: &Path,
        clean_model_name: &str,
    ) -> Result<Vec<String>, AppError> {
        let mut copied_images = Vec::new();

        for (i, path) in image_paths.iter().enumerate() {
            let source_path = Path::new(path);
            let new_name = storage::rename_image(clean_model_name, source_path, i);
            let destination_path = model_folder.join(&new_name);

            fs::copy(source_path, &destination_path)
                .map_err(|e| AppError::IoError(format!("failed to copy image; {}", e)))?;
            copied_images.push(destination_path.to_string_lossy().into_owned());
        }

        Ok(copied_images)
    }

    fn copy_files(file_paths: &[String], model_folder: &Path) -> Result<Vec<String>, AppError> {
        let mut copied_files = Vec::new();

        for path in file_paths {
            let source_path = Path::new(path);
            let file_name = source_path
                .file_name()
                .ok_or_else(|| AppError::IoError(format!("Invalid file path: {}", path)))?;

            let destination_folder = storage::get_destination_folder(model_folder, source_path);
            let destination_path = destination_folder.join(file_name);

            fs::copy(source_path, &destination_path)
                .map_err(|e| AppError::IoError(format!("failed to copy file; {}", e)))?;
            copied_files.push(destination_path.to_string_lossy().into_owned());
        }

        Ok(copied_files)
    }

    let copied_images = copy_images(&image_paths, &model_folder, &clean_model_name)?;
    let copied_files = copy_files(&file_paths, &model_folder)?;

    let mut relative_image_paths = Vec::new();
    for path in &copied_images {
        let rel_path = storage::convert_to_relative_path(path, &model_folder).map_err(|e| {
            AppError::IoError(format!("Failed to convert image path to relative: {}", e))
        })?;
        relative_image_paths.push(rel_path);
    }

    let mut relative_file_paths = Vec::new();
    for path in &copied_files {
        let rel_path = storage::convert_to_relative_path(path, &model_folder).map_err(|e| {
            AppError::IoError(format!("Failed to convert file path to relative: {}", e))
        })?;
        relative_file_paths.push(rel_path);
    }

    let model_with_relative_paths = StlModel {
        model_name: model.model_name,
        description: model.description,
        tags: model.tags,
        group: model.group,
        images: relative_image_paths,
        model_files: relative_file_paths,
    };

    let model_json = serde_json::to_string_pretty(&model_with_relative_paths)?;
    fs::write(&model_folder.join("model.json"), model_json)?;

    Ok(model_with_relative_paths)
}

#[tauri::command]
#[specta::specta]
pub async fn create_release(app_handle: AppHandle, release: Release) -> Result<(), AppError> {
    let settings = SETTINGS_CACHE
        .lock()
        .map_err(|e| AppError::ConfigError(format!("{}", e)))?;
    let scratch_dir = storage::get_dir(&app_handle, "".to_string(), settings.scratch_dir.clone())?;

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
    let release_dir = scratch_dir.join(&release_dir_name);

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
pub async fn finalize_release(app_handle: AppHandle, release_name: String) -> Result<(), AppError> {
    let settings = SETTINGS_CACHE
        .lock()
        .map_err(|e| AppError::ConfigError(format!("{}", e)))?;

    let scratch_dir = storage::get_dir(
        &app_handle,
        "".to_string(), // Empty string to avoid the "releases" subdirectory
        settings.scratch_dir.clone(),
    )?;

    let entries = fs::read_dir(&scratch_dir)?;
    let release_dir = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        .find(|e| e.file_name().to_string_lossy().contains(&release_name))
        .ok_or_else(|| AppError::NotFoundError(format!("Release '{}' not found", release_name)))?
        .path();

    let target_dir = if let Some(dir) = &settings.target_dir {
        PathBuf::from(dir)
    } else {
        app_handle.path().app_data_dir()?.join("exports")
    };

    fs::create_dir_all(&target_dir)?;

    let release_dir_name = release_dir
        .file_name()
        .ok_or_else(|| AppError::ConfigError("Invalid release directory name".to_string()))?
        .to_string_lossy()
        .to_string();

    let archive_path = target_dir.join(format!("{}.zip", release_dir_name));

    compress_dir(
        &release_dir,
        &archive_path,
        &app_handle,
        settings
            .compression_type
            .clone()
            .unwrap_or(CompressionType::Zip),
        settings.chunk_size,
    )?;

    fs::remove_dir_all(&release_dir)
        .map_err(|e| AppError::IoError(format!("Failed to clean up release directory: {}", e)))?;

    Ok(())
}
