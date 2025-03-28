use crate::compressors::zip_directory;
use crate::image::crop::generate_smart_thumbnail;
use crate::models::StlModel;
use specta;
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

pub fn get_storage_dir(
    app_handle: &AppHandle,
    scratch_dir: Option<String>,
    dir_name: String,
) -> Result<PathBuf, String> {
    let temp_dir = if let Some(dir) = scratch_dir {
        PathBuf::from(dir).join(dir_name)
    } else {
        let app_dir = app_handle
            .path()
            .app_data_dir()
            .map_err(|e| format!("Failed to get app data dir: {}", e))?;
        app_dir.join(dir_name)
    };

    fs::create_dir_all(&temp_dir).map_err(|e| format!("Failed to create temp directory: {}", e))?;

    Ok(temp_dir)
}

pub fn clean_name(name: &str) -> String {
    name.trim().to_lowercase().replace(" ", "_")
}

pub fn write_file(path: &Path, data: &[u8]) -> Result<String, String> {
    fs::write(path, data).map_err(|e| format!("Failed to write file: {}", e))?;

    Ok(path.to_string_lossy().to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn store_image(
    app_handle: AppHandle,
    image_data: Vec<u8>,
    image_name: String,
    model_name: String,
    image_index: u32,
    scratch_dir: Option<String>,
) -> Result<String, String> {
    let temp_dir = get_storage_dir(&app_handle, scratch_dir, "temp".to_string())?;
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

#[tauri::command]
#[specta::specta]
pub async fn store_model_file(
    app_handle: AppHandle,
    file_data: Vec<u8>,
    file_name: String,
    model_name: String,
    scratch_dir: Option<String>,
) -> Result<String, String> {
    let temp_dir = get_storage_dir(&app_handle, scratch_dir, "temp".to_string())?;
    let clean_model_name = clean_name(&model_name);

    let new_filename = format!("{}-{}", clean_model_name, file_name);

    let file_path = temp_dir.join(new_filename);
    write_file(&file_path, &file_data)
}

#[tauri::command]
#[specta::specta]
pub async fn save_model(
    app_handle: AppHandle,
    model: StlModel,
    target_dir: Option<String>,
) -> Result<(), String> {
    let models_dir = get_storage_dir(&app_handle, target_dir, "models".to_string())?;
    let model_dir_name = clean_name(&model.model_name);
    let model_dir = models_dir.join(&model_dir_name);

    if model_dir.exists() {
        fs::remove_dir_all(&model_dir)
            .map_err(|e| format!("Failed to remove existing model directory: {}", e))?;
    }
    fs::create_dir_all(&model_dir)
        .map_err(|e| format!("Failed to create model directory: {}", e))?;

    let model_json = serde_json::to_string_pretty(&model)
        .map_err(|e| format!("Failed to serialize model: {}", e))?;
    fs::write(&model_dir.join("model.json"), model_json)
        .map_err(|e| format!("Failed to write model.json: {}", e))?;

    move_model_files_and_images(&model.images, &model.model_files, &model_dir, &app_handle)?;

    Ok(())
}

fn move_model_files_and_images(
    images: &[String],
    model_files: &[String],
    target_dir: &Path,
    app_handle: &AppHandle,
) -> Result<(), String> {
    let move_file = |source_path: &str, target_path: &Path| -> Result<PathBuf, String> {
        let source = Path::new(source_path);
        let extension = source
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_lowercase())
            .unwrap_or_default();
        let filename = source.file_name().ok_or_else(|| "Failed to get filename")?;

        let target_subdir = match extension.as_str() {
            "lyt" | "lys" | "lychee" => {
                let subdir = target_path.join("lychee");
                fs::create_dir_all(&subdir)
                    .map_err(|e| format!("Failed to create lychee directory: {}", e))?;
                subdir
            }

            "chitu" | "chitubox" => {
                let subdir = target_path.join("chitubox");
                fs::create_dir_all(&subdir)
                    .map_err(|e| format!("Failed to create chitubox directory: {}", e))?;
                subdir
            }

            _ => target_path.to_path_buf(),
        };

        let target = target_subdir.join(filename);

        match fs::rename(source, &target) {
            Ok(_) => Ok(target),
            Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
                fs::copy(source, &target).map_err(|e| format!("Failed to copy file: {}", e))?;
                fs::remove_file(source)
                    .map_err(|e| format!("Failed to remove source file: {}", e))?;
                Ok(target)
            }
            Err(e) => Err(format!("Failed to move file: {}", e)),
        }
    };

    for img in images {
        let file_path = move_file(img, target_dir)?;
        let _thumb = generate_smart_thumbnail(file_path.as_path(), 500)
            .map_err(|e| format!("Failed to generate smart thumbnail: {}", e))?;
        // TODO: save thumbnail with name to the release directory
    }

    for file in model_files {
        move_file(file, target_dir)?;
    }

    zip_directory(target_dir, &target_dir.with_extension("zip"), app_handle)
        .map_err(|e| format!("Failed to zip model directory: {}", e))?;

    Ok(())
}
