use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, Manager};
use crate::models::CompressionType;

fn get_7zip_path<R: tauri::Runtime>(app_handle: &AppHandle<R>) -> Result<PathBuf, String> {
    let resource_path = app_handle.path().resource_dir()
        .map_err(|e| format!("Failed to get resource dir: {}", e))?;

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
        let metadata = fs::metadata(&binary_path)
            .map_err(|e| format!("Failed to get metadata: {}", e))?;
        let mut perms = metadata.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&binary_path, perms)
            .map_err(|e| format!("Failed to set permissions: {}", e))?;
    }

    Ok(binary_path)
}

pub fn compress_dir(
    source_dir: &Path,
    target_path: &Path,
    app_handle: &AppHandle,
    compression_type: CompressionType,
    chunk_size: Option<u32>,
) -> Result<(), String> {
    let binary_path = get_7zip_path(app_handle)?;

    // Count total files for progress reporting
    let mut total_files = 0;
    count_files_in_dir(source_dir, &mut total_files)?;

    // Build the command
    let mut command = std::process::Command::new(&binary_path);

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);  // CREATE_NO_WINDOW
    }

    command
        .arg("a") // Add files to archive
        .arg("-mx=9") // Set compression level to maximum
        .arg("-bs=o"); // Set output stream to stdout

    // Set format based on compression type
    match compression_type {
        CompressionType::Zip => {
            command.arg("-tzip");
        },
        CompressionType::SevenZip => {
            command.arg("-t7z");
        },
        CompressionType::TarGz => {
            command.arg("-tgzip");
        },
        CompressionType::TarXz => {
            command.arg("-txz");
        },
    }

    // Add split size parameter if needed
    if let Some(size) = chunk_size {
        if size > 0 {
            command.arg(format!("-v{}m", size));
        }
    }

    command
        .arg(target_path)
        .arg(format!("{}/*", source_dir.to_string_lossy()));

    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start 7z: {}", e))?;

    let mut percent = 0;
    let stdout= if let Some(stdout) = child.stdout.take(){
        stdout
    } else {
        return Err("Failed to get stdout".to_string());
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
                ).map_err(|e| format!("Failed to emit progress: {}", e))?;
            }
        }
    }

    let status = child.wait().map_err(|e| format!("Failed to wait for 7z: {}", e))?;

    if !status.success() {
        if let Some(mut stderr) = child.stderr.take() {
            use std::io::Read;
            let mut error = String::new();
            let _ = Read::read_to_string(&mut stderr, &mut error);
            return Err(format!("7z failed: {}", error));
        }
        return Err("7z failed with unknown error".to_string());    }

    // Send completion notification
    app_handle.emit(
        "compression_progress",
        serde_json::json!({
            "processed": total_files,
            "total": total_files,
            "percent": 100,
        }),
    ).map_err(|e| format!("Failed to emit progress: {}", e))?;

    // Clean up source directory
    fs::remove_dir_all(source_dir)
        .map_err(|e| format!("Failed to remove source directory: {}", e))?;

    Ok(())
}

fn parse_percentage(line: &str) -> Option<u8> {
    let pos = line.find('%')?;
    if pos == 0 { return None; }
    line[0..pos].trim().parse::<u8>().ok()
}
fn count_files_in_dir(dir: &Path, count: &mut u32) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|err| format!("Failed to read dir: {}", err))? {
        let entry = entry.map_err(|err| format!("Failed to read entry: {}", err))?;
        let path = entry.path();

        if path.is_dir() {
            count_files_in_dir(&path, count)?;
        } else {
            *count += 1;
        }
    }
    Ok(())
}