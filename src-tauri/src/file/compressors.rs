use crate::error::AppError;
use crate::models::CompressionType;
use crate::settings::SETTINGS_CACHE;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, Manager};

pub fn get_compression_type() -> Result<CompressionType, AppError> {
    let compression_type = {
        SETTINGS_CACHE
            .lock()
            .map_err(|e| AppError::ConfigError(format!("{}", e)))?
            .compression_type
            .clone()
    };
    match compression_type {
        Some(comp) => Ok(comp),
        None => Err(AppError::ConfigError(
            "Compression type not set".to_string(),
        )),
    }
}

pub fn get_chunk_size() -> Result<Option<u32>, AppError> {
    let chunk_size = {
        SETTINGS_CACHE
            .lock()
            .map_err(|e| AppError::ConfigError(format!("{}", e)))?
            .chunk_size
            .clone()
    };
    Ok(chunk_size)
}

fn get_7zip_path<R: tauri::Runtime>(app_handle: &AppHandle<R>) -> Result<PathBuf, AppError> {
    let resource_path = app_handle.path().resource_dir()?;

    #[cfg(target_os = "windows")]
    let binary_path = resource_path.join("win").join("7za.exe");

    #[cfg(target_os = "macos")]
    let binary_path = resource_path.join("macos").join("7zz");

    #[cfg(target_os = "linux")]
    let binary_path = resource_path.join("linux").join("7zz");

    // Set executable permissions on Unix systems
    #[cfg(not(target_os = "windows"))]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::metadata(&binary_path)?;
        let mut perms = metadata.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&binary_path, perms)?;
    }

    Ok(binary_path)
}

pub fn compress_dir(
    source_dir: &Path,
    target_path: &Path,
    app_handle: &AppHandle,
) -> Result<(), AppError> {
    let binary_path = get_7zip_path(app_handle)?;

    if !binary_path.exists() {
        return Err(AppError::FileProcessingError(format!(
            "7-Zip binary not found at {:?}. Please ensure the application is properly installed.",
            binary_path
        )));
    }

    // Count total files for progress reporting
    let mut total_files = 0;
    count_files_in_dir(source_dir, &mut total_files)?;

    // Build the command
    let mut command = std::process::Command::new(&binary_path);

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }

    command
        .arg("a") // Add files to archive
        .arg("-mx=9") // Set compression level to maximum
        .arg("-bs=o"); // Set output stream to stdout

    let compression_type = get_compression_type()?;
    // Set format based on compression type
    match compression_type {
        CompressionType::Zip => {
            command.arg("-tzip");
        }
        CompressionType::SevenZip => {
            command.arg("-t7z");
        }
        CompressionType::TarGz => {
            command.arg("-tgzip");
        }
        CompressionType::TarXz => {
            command.arg("-txz");
        }
    }

    let chunk_size = get_chunk_size()?;
    // Add split size parameter if needed
    if let Some(size) = chunk_size {
        if size > 0 {
            command.arg(format!("-v{}m", size));
        }
    }

    command
        .arg(target_path)
        .arg(format!("{}/*", source_dir.to_string_lossy()));

    let command_str = format!("{:?}", command);
    let mut child = match command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            return Err(AppError::FileProcessingError(format!(
                "Failed to start 7z process: {}. Command: {}",
                e, command_str
            )))
        }
    };

    let mut percent = 0;
    let stdout = if let Some(stdout) = child.stdout.take() {
        stdout
    } else {
        return Err(AppError::FileProcessingError(
            "Failed to get stdout".to_string(),
        ));
    };

    let reader = BufReader::new(stdout);
    let app = app_handle.clone();

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(line) => line,
            Err(_) => continue,
        };

        if let Some(new_percent) = parse_percentage(&line) {
            if new_percent != percent {
                percent = new_percent;
                let processed = (total_files * percent as u32) / 100;
                app.emit(
                    "compression_progress",
                    serde_json::json!({
                        "processed": processed,
                        "total": total_files,
                        "percent": percent,
                    }),
                )?;
            }
        }
    }

    let status = child.wait()?;

    if !status.success() {
        if let Some(mut stderr) = child.stderr.take() {
            use std::io::Read;
            let mut error = String::new();
            let _ = Read::read_to_string(&mut stderr, &mut error);
            return Err(AppError::FileProcessingError(format!(
                "7z failed: {}",
                error
            )));
        }
        return Err(AppError::FileProcessingError(
            "7z failed with unknown error".to_string(),
        ));
    }

    // Send completion notification
    app_handle.emit(
        "compression_progress",
        serde_json::json!({
            "processed": total_files,
            "total": total_files,
            "percent": 100,
        }),
    )?;

    // Clean up source directory
    fs::remove_dir_all(source_dir)?;

    Ok(())
}

fn parse_percentage(line: &str) -> Option<u8> {
    let pos = line.find('%')?;
    if pos == 0 {
        return None;
    }
    line[0..pos].trim().parse::<u8>().ok()
}

fn count_files_in_dir(dir: &Path, count: &mut u32) -> Result<(), AppError> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            count_files_in_dir(&path, count)?;
        } else {
            *count += 1;
        }
    }
    Ok(())
}
