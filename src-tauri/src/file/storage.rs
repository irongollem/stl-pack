use crate::error::AppError;
use crate::settings::SETTINGS_CACHE;
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

pub fn get_scratch_path(app_handle: &AppHandle) -> Result<PathBuf, AppError> {
    let scratch_dir = {
        SETTINGS_CACHE
            .lock()
            .map_err(|e| AppError::ConfigError(format!("{}", e)))?
            .scratch_dir
            .clone()
    };

    if let Some(dir) = scratch_dir {
        Ok(PathBuf::from(dir))
    } else {
        // The scratchdir is temporary in nature so the default is the app_data dir
        Ok(app_handle.path().app_data_dir()?)
    }
}

pub fn create_dir_on_scratch(
    app_handle: &AppHandle,
    dir_name: String,
) -> Result<PathBuf, AppError> {
    let scratch_root = get_scratch_path(app_handle)?;
    let temp_dir = scratch_root.join(dir_name);

    fs::create_dir_all(&temp_dir)?;

    Ok(temp_dir)
}

pub fn get_target_path(app_handle: &AppHandle) -> Result<PathBuf, AppError> {
    let target_dir = {
        SETTINGS_CACHE
            .lock()
            .map_err(|e| AppError::ConfigError(format!("{}", e)))?
            .target_dir
            .clone()
    };

    if let Some(dir) = target_dir {
        Ok(PathBuf::from(dir))
    } else {
        Ok(app_handle
            .path()
            .document_dir()?
            .join("Plinth")
            .join("exports"))
    }
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

        let destination_path = model_folder.join(file_name);

        // Same basename from two different source folders would silently
        // overwrite here — fail loudly instead of losing a part
        if destination_path.exists() {
            return Err(AppError::InvalidInput(format!(
                "Duplicate file name '{}': a file with this name was already added to this model. Rename one of the files and try again.",
                file_name.to_string_lossy()
            )));
        }

        fs::copy(source_path, &destination_path)
            .map_err(|e| AppError::IoError(format!("failed to copy file; {}", e)))?;
        copied_files.push(destination_path.to_string_lossy().into_owned());
    }

    Ok(copied_files)
}

/// Copy the creator's licence file into the release root under the CANONICAL
/// name `licence.<ext>` — collect_files_for_compression recognizes that stem
/// and routes the file into release.3pk, whatever the source was called.
pub fn copy_licence(source: &str, release_dir: &Path) -> Result<String, AppError> {
    let source_path = Path::new(source);
    if !source_path.is_file() {
        return Err(AppError::NotFoundError(format!(
            "Licence file '{}' does not exist — update it in Settings",
            source
        )));
    }
    let extension = source_path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let destination = release_dir.join(format!("licence{}", extension));
    fs::copy(source_path, &destination)
        .map_err(|e| AppError::IoError(format!("Failed to copy licence file: {}", e)))?;
    Ok(destination.to_string_lossy().into_owned())
}

/// Root-level loose files that belong INSIDE release.3pk: the release-level
/// metadata (jsons), preview images in any format the builder accepts, and
/// the licence file copy_licence wrote. Everything else loose stays
/// zip-only — the .3pk is deliberately small.
fn belongs_in_3pk(file_name: &str) -> bool {
    let lower = file_name.to_lowercase();
    if lower.ends_with(".json") {
        return true;
    }
    if crate::catalog::IMAGE_EXTENSIONS
        .iter()
        .any(|ext| lower.ends_with(&format!(".{}", ext)))
    {
        return true;
    }
    let stem = lower.rsplit_once('.').map_or(lower.as_str(), |(s, _)| s);
    stem == "licence" || stem == "license"
}

pub fn convert_to_relative_path(absolute_path: &str, base_dir: &Path) -> Result<String, AppError> {
    // strip_prefix is component-aware: unlike string starts_with it can't
    // mistake a sibling like /x/ScratchOld for being inside /x/Scratch
    Path::new(absolute_path)
        .strip_prefix(base_dir)
        .map(|rel| rel.to_string_lossy().into_owned())
        .map_err(|_| {
            AppError::InvalidInput(format!(
                "Path '{}' is not within base directory '{}'",
                absolute_path,
                base_dir.display()
            ))
        })
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

/// (group/model directories, files destined for release.3pk, files destined for release.zip)
pub type CompressionFileSets = (Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>);

pub fn collect_files_for_compression(
    release_dir_path: &PathBuf,
) -> Result<CompressionFileSets, AppError> {
    let mut group_and_model_dirs = Vec::new();
    let mut files_for_3pk = Vec::new();
    let mut files_for_zip = Vec::new();

    for entry in fs::read_dir(release_dir_path)
        .map_err(|e| AppError::IoError(format!("Failed to read release directory: {}", e)))?
    {
        let entry = entry.map_err(|e| AppError::IoError(format!("Failed to read entry: {}", e)))?;
        let path = entry.path();

        if path.is_dir() {
            group_and_model_dirs.push(path.clone());
        } else if path.is_file() {
            let file_name = path
                .file_name()
                .ok_or_else(|| AppError::ConfigError("Invalid file name".to_string()))?
                .to_string_lossy()
                .into_owned();

            if belongs_in_3pk(&file_name) {
                files_for_3pk.push(path.clone());
            }

            files_for_zip.push(path.clone());
        }
    }

    Ok((group_and_model_dirs, files_for_3pk, files_for_zip))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The .3pk carries release-level metadata, previews in EVERY builder
    /// image format, and the licence — while bulky extras stay zip-only.
    #[test]
    fn routes_metadata_images_and_licence_into_the_3pk() {
        for name in [
            "release.json",
            "manifest.json",
            "cover.png",
            "group-shot.jpg",
            "teaser.webp",
            "licence.pdf",
            "LICENSE.md",
            "licence",
        ] {
            assert!(belongs_in_3pk(name), "{} belongs in release.3pk", name);
        }
        for name in ["paint-guide.pdf", "extras.zip", "notes.txt", "licences-overview.pdf"] {
            assert!(!belongs_in_3pk(name), "{} stays zip-only", name);
        }
    }

    #[test]
    fn licence_copy_lands_under_the_canonical_name() {
        let dir = std::env::temp_dir().join(format!("stlpack_licence_{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("release")).unwrap();
        let source = dir.join("My Custom EULA v3.PDF");
        std::fs::write(&source, b"terms").unwrap();

        let copied = copy_licence(&source.to_string_lossy(), &dir.join("release")).unwrap();
        let copied = Path::new(&copied);
        assert_eq!(copied.file_name().unwrap(), "licence.PDF");
        assert_eq!(std::fs::read(copied).unwrap(), b"terms");

        // A stale settings path fails loudly, not with a half-made release
        let missing = copy_licence(&dir.join("gone.pdf").to_string_lossy(), &dir.join("release"));
        assert!(missing.is_err());

        std::fs::remove_dir_all(&dir).ok();
    }
}
