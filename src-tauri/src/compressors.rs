use std::fs;
use std::path::Path;
use std::fs::File;
use std::io::{Read, Seek, Write};
use tauri::{AppHandle, Emitter};
use zip::{write::SimpleFileOptions, ZipWriter };

pub fn zip_directory(source_dir: &Path, zip_path: &Path, app_handle: &AppHandle) -> Result<(), String> {
    let mut total_files = 0;
    count_files_in_dir(source_dir, &mut total_files)?;

    let file  = File::create(zip_path).map_err(|err| format!("Failed to create zip file: {}", err))?;

    let mut zip = ZipWriter::new(file);
    let mut processed_files = 0;

    app_handle.emit("compression_progress",
        serde_json::json!({
            "processed": processed_files,
            "total": total_files,
            "percent": 0,
        })
    ).map_err(|err| format!("Failed to emit progress file: {}", err))?;

    add_dir_to_zip(&mut zip, source_dir, source_dir, app_handle, &mut processed_files, total_files)?;

    zip.finish().map_err(|err| format!("Failed to finish zip file: {}", err))?;

    app_handle.emit("compression_progress",
        serde_json::json!({
            "processed": total_files,
            "total": total_files,
            "percent": 100,
        })
    ).map_err(|err| format!("Failed to emit progress file: {}", err))?;

    fs::remove_dir_all(source_dir)
        .map_err(|err| format!("Failed to remove source directory: {}", err))?;

    Ok(())
}

fn count_files_in_dir(dir: &Path, count: &mut usize) -> Result<(), String> {
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

fn add_dir_to_zip<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    base_path: &Path,
    dir: &Path,
    app_handle: &AppHandle,
    processed_files: &mut usize,
    total_files: usize,
) -> Result<(), String> {
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true)
        .unix_permissions(0o755);

    for entry in fs::read_dir(dir).map_err(|err| format!("Failed to read dir: {}", err))? {
        let entry = entry.map_err(|err| format!("Failed to read entry: {}", err))?;
        let path = entry.path();

        let relative_path = path.strip_prefix(base_path)
            .map_err(|err| format!("Failed to strip prefix: {}", err))?;
        let name = relative_path.to_string_lossy();

        if path.is_dir() {
            zip.add_directory(name, options)
                .map_err(|err| format!("Failed to add directory to zip: {}", err))?;
            add_dir_to_zip(zip, base_path, &path, app_handle, processed_files, total_files)?;
        } else {
            zip.start_file(name, options)
                .map_err(|err| format!("Failed to start file in zip: {}", err))?;
            let mut file = File::open(&path)
                .map_err(|err| format!("Failed to open file: {}", err))?;
            let mut buffer = Vec::new();
            file.read_to_end(&mut buffer)
                .map_err(|err| format!("Failed to read file: {}", err))?;

            zip.write_all(&buffer)
                .map_err(|err| format!("Failed to write file to zip: {}", err))?;

            *processed_files += 1;
            let percent = (*processed_files as f32 / total_files as f32 * 100.0) as u8;

            app_handle.emit("compression_progress",
                serde_json::json!({
                    "processed": *processed_files,
                    "total": total_files,
                    "percent": percent,
                })
            ).map_err(|err| format!("Failed to emit progress file: {}", err))?;
        }
    }

    Ok(())
}
