use std::fs;
use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager};
use crate::error::AppError;

pub fn move_files_with_processor<F>(
    files: &[String],
    target_dir: &Path,
    process_fn: F
) -> Result<Vec<String>, AppError>
where
    F: Fn(&str, &Path) -> Result<(PathBuf, String), AppError>
{
    let mut relative_paths = Vec::new();

    for file in files {
        let (_, relative_path) = process_fn(file, target_dir)?;
        relative_paths.push(relative_path);
    }

    Ok(relative_paths)
}

pub fn move_file_with_paths(
    source_path: &str,
    target_dir: &Path
) -> Result<(PathBuf, String), AppError> {
    let source = Path::new(source_path);
    let filename = source
        .file_name()
        .ok_or_else(|| AppError::ConfigError("Failed to get filename".to_string()))?;

    let target_subdir = get_target_subdir(source, target_dir)?;
    let target = target_subdir.join(filename);

    // Move the file
    match fs::rename(source, &target) {
        Ok(_) => (),
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            fs::copy(source, &target)?;
            fs::remove_file(source)?;
        }
        Err(e) => return Err(e.into()),
    }

    // Calculate relative path for JSON storage
    let relative_path = if target_subdir == *target_dir {
        filename.to_string_lossy().into_owned()
    } else {
        let subdir_name = target_subdir
            .file_name()
            .ok_or_else(|| AppError::ConfigError("Failed to get subdir name".to_string()))?;
        format!("{}/{}", subdir_name.to_string_lossy(), filename.to_string_lossy())
    };

    Ok((target, relative_path))
}

pub fn get_storage_dir(
    app_handle: &AppHandle,
    scratch_dir: Option<String>,
    dir_name: String,
) -> Result<PathBuf, AppError> {
    let temp_dir = if let Some(dir) = scratch_dir {
        PathBuf::from(dir).join(dir_name)
    } else {
        let app_dir = app_handle
            .path()
            .app_data_dir()?;
        app_dir.join(dir_name)
    };

    fs::create_dir_all(&temp_dir)?;

    Ok(temp_dir)
}

fn get_target_subdir(file_path: &Path, target_path: &Path) -> Result<PathBuf, AppError> {
    let extension = file_path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_lowercase())
        .unwrap_or_default();

    let target_subdir = match extension.as_str() {
        "lyt" | "lys" | "lychee" => target_path.join("lychee"),
        "chitu" | "chitubox" => target_path.join("chitubox"),
        _ => return Ok(target_path.to_path_buf()),
    };

    fs::create_dir_all(&target_subdir)?;

    Ok(target_subdir)
}

pub fn write_file(path: &Path, data: &[u8]) -> Result<String, AppError> {
    fs::write(path, data)?;
    Ok(path.to_string_lossy().to_string())
}
