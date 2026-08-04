use super::engine;
use crate::error::AppError;
use crate::models::events::{
    RenderCancelledStatus, RenderCompletedStatus, RenderFailedStatus, RenderProgressStatus,
    RenderStartedStatus, RenderStatus,
};
use crate::models::{BlenderInfo, RenderOptions};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::AppHandle;
use tauri_specta::Event;
use tokio::sync::Notify;
use uuid::Uuid;

// pub(crate): batch renders register here too, so one registry serves both.
pub(crate) static ACTIVE_RENDERS: Lazy<Mutex<HashMap<String, Arc<Notify>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[tauri::command]
#[specta::specta]
pub async fn detect_blender() -> Result<BlenderInfo, AppError> {
    engine::detect_blender().await
}

#[tauri::command]
#[specta::specta]
pub async fn start_render(
    app_handle: AppHandle,
    parts: Vec<String>,
    options: RenderOptions,
) -> Result<String, AppError> {
    // The Blender script imports STL only — slicer scenes (.lys, .chitubox)
    // and other sidecar files that ride along in model folders would make
    // the import step die mid-render with an opaque Blender error. Filter
    // here so every caller gets the same guarantee.
    let parts: Vec<String> = parts
        .into_iter()
        .filter(|p| {
            Path::new(p)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("stl"))
        })
        .collect();
    if parts.is_empty() {
        return Err(AppError::InvalidInput(
            "No renderable files: the render engine imports .stl parts only".to_string(),
        ));
    }
    for part in &parts {
        if !Path::new(part).is_file() {
            return Err(AppError::NotFoundError(format!("STL not found: {}", part)));
        }
    }

    // Broken look overrides fail HERE, not seconds later inside a Blender
    // cold start. The script re-validates anyway (defense in depth), but a
    // render that can only die deserves an instant, clear error.
    if let Some(config) = options
        .look_config
        .as_deref()
        .filter(|c| !c.trim().is_empty())
    {
        match serde_json::from_str::<serde_json::Value>(config) {
            Ok(serde_json::Value::Object(_)) => {}
            Ok(_) => {
                return Err(AppError::InvalidInput(
                    "Look overrides must be a JSON object".to_string(),
                ))
            }
            Err(e) => {
                return Err(AppError::InvalidInput(format!(
                    "Look overrides are not valid JSON: {}",
                    e
                )))
            }
        }
    }

    let blender = engine::detect_blender_cached().await?;
    let script = engine::materialize_render_script(&app_handle)?;

    let output_path = match &options.output_path {
        Some(out) => PathBuf::from(out),
        None => PathBuf::from(&parts[0]).with_extension("png"),
    };
    // Never clobber silently: without explicit overwrite consent an
    // existing file gets a -1/-2/... suffix (the Started/Completed events
    // carry the real path, so the UI always shows where it went)
    let output_path = if options.overwrite {
        output_path
    } else {
        crate::file::utils::unique_path(output_path)
    };

    let job_id = Uuid::new_v4().to_string();
    let cancel_token = Arc::new(Notify::new());
    {
        let mut renders = ACTIVE_RENDERS.lock().map_err(|e| {
            AppError::ConfigError(format!("Failed to access render registry: {}", e))
        })?;
        renders.insert(job_id.clone(), Arc::clone(&cancel_token));
    }

    let job_id_clone = job_id.clone();
    tokio::spawn(async move {
        run_render_job(
            app_handle,
            job_id_clone,
            blender,
            script,
            parts,
            options,
            output_path,
            cancel_token,
        )
        .await;
    });

    Ok(job_id)
}

/// Read an image as a data URL for the branding bake. The webview draws
/// the finished render (plus logo) onto a canvas — but images loaded via
/// the asset: protocol are cross-origin and TAINT the canvas, making
/// toBlob() throw. Data URLs don't, so the bytes take this detour.
#[tauri::command]
#[specta::specta]
pub async fn read_image_base64(path: String) -> Result<String, AppError> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = std::fs::read(&path)
            .map_err(|e| AppError::IoError(format!("Failed to read image {}: {}", path, e)))?;
        let mime = match Path::new(&path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref()
        {
            Some("jpg") | Some("jpeg") => "image/jpeg",
            Some("webp") => "image/webp",
            Some("gif") => "image/gif",
            Some("svg") => "image/svg+xml",
            _ => "image/png",
        };
        Ok(format!("data:{};base64,{}", mime, STANDARD.encode(&bytes)))
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Image read task failed: {}", e)))?
}

/// Overwrite an existing render with the branding-composited PNG the
/// webview produced. Three guards keep a bad bake from eating a good
/// render: the target must already exist (this only ever re-writes a
/// finished render), the bytes must carry the PNG magic (a blank/failed
/// canvas export dies here, not on disk), and the write goes through
/// temp + rename so a crash can't leave a half-written file.
#[tauri::command]
#[specta::specta]
pub async fn write_png_base64(path: String, data: String) -> Result<(), AppError> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    tauri::async_runtime::spawn_blocking(move || {
        let target = PathBuf::from(&path);
        if !target.is_file() {
            return Err(AppError::NotFoundError(format!(
                "Refusing to write composited image: {} does not exist",
                path
            )));
        }
        // Accept a full data URL or bare base64 — everything after the
        // last comma is the payload either way
        let payload = data.rsplit(',').next().unwrap_or(&data);
        let bytes = STANDARD
            .decode(payload)
            .map_err(|e| AppError::InvalidInput(format!("Invalid image data: {}", e)))?;
        if !bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
            return Err(AppError::InvalidInput(
                "Composited data is not a PNG — keeping the original render".to_string(),
            ));
        }
        let tmp = target.with_extension("png.tmp");
        std::fs::write(&tmp, &bytes)
            .map_err(|e| AppError::IoError(format!("Failed to write composited image: {}", e)))?;
        std::fs::rename(&tmp, &target).map_err(|e| {
            std::fs::remove_file(&tmp).ok();
            AppError::IoError(format!("Failed to replace render with composite: {}", e))
        })
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Image write task failed: {}", e)))?
}

/// Read a shareable look file for import. The webview's fs capability is
/// read-only and dialog-driven anyway; this narrow command keeps the same
/// posture: .json only, 1 MB cap (a look file is ~1 KB — anything bigger is
/// a mis-pick), parsing/validation stays client-side where the schema lives.
#[tauri::command]
#[specta::specta]
pub async fn read_look_json(path: String) -> Result<String, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let target = Path::new(&path);
        if !target
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        {
            return Err(AppError::InvalidInput(
                "Look files are .json documents".to_string(),
            ));
        }
        let meta = std::fs::metadata(target)
            .map_err(|e| AppError::IoError(format!("Failed to read look file {}: {}", path, e)))?;
        if meta.len() > 1_000_000 {
            return Err(AppError::InvalidInput(
                "That file is too large to be a look file".to_string(),
            ));
        }
        std::fs::read_to_string(target)
            .map_err(|e| AppError::IoError(format!("Failed to read look file {}: {}", path, e)))
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Look read task failed: {}", e)))?
}

/// Write a shareable look file where the save dialog pointed. Mirrors
/// write_png_base64's guarded style: the contents must parse as JSON (the
/// caller builds them, but a corrupt write would burn the person it was
/// shared WITH), and temp + rename means no half-written file survives.
#[tauri::command]
#[specta::specta]
pub async fn write_look_json(path: String, contents: String) -> Result<(), AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let target = PathBuf::from(&path);
        if !target
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        {
            return Err(AppError::InvalidInput(
                "Look files are .json documents".to_string(),
            ));
        }
        if serde_json::from_str::<serde_json::Value>(&contents).is_err() {
            return Err(AppError::InvalidInput(
                "Refusing to write a corrupt look file".to_string(),
            ));
        }
        let tmp = target.with_extension("json.tmp");
        std::fs::write(&tmp, &contents)
            .map_err(|e| AppError::IoError(format!("Failed to write look file: {}", e)))?;
        std::fs::rename(&tmp, &target).map_err(|e| {
            std::fs::remove_file(&tmp).ok();
            AppError::IoError(format!("Failed to finalize look file: {}", e))
        })
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Look write task failed: {}", e)))?
}

#[tauri::command]
#[specta::specta]
pub async fn cancel_render(job_id: String) -> Result<(), AppError> {
    let renders = ACTIVE_RENDERS
        .lock()
        .map_err(|e| AppError::ConfigError(format!("Failed to access render registry: {}", e)))?;

    if let Some(token) = renders.get(&job_id) {
        token.notify_waiters();
        Ok(())
    } else {
        Err(AppError::NotFoundError(format!(
            "No active render job with ID: {}",
            job_id
        )))
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_render_job(
    app_handle: AppHandle,
    job_id: String,
    blender: BlenderInfo,
    script: PathBuf,
    parts: Vec<String>,
    options: RenderOptions,
    output_path: PathBuf,
    cancel_token: Arc<Notify>,
) {
    let start_time = std::time::Instant::now();

    RenderStatus::Started(RenderStartedStatus {
        job_id: job_id.clone(),
        output_path: output_path.to_string_lossy().into_owned(),
    })
    .emit(&app_handle)
    .ok();

    let result = run_blender(
        &app_handle,
        &job_id,
        &blender,
        &script,
        &parts,
        &options,
        &output_path,
        &cancel_token,
    )
    .await;

    if let Ok(mut renders) = ACTIVE_RENDERS.lock() {
        renders.remove(&job_id);
    }

    match result {
        Ok(()) => {
            RenderStatus::Completed(RenderCompletedStatus {
                job_id,
                output_path: output_path.to_string_lossy().into_owned(),
                elapsed_seconds: start_time.elapsed().as_secs_f64(),
            })
            .emit(&app_handle)
            .ok();
        }
        Err(AppError::UserCancelled(_)) => {
            RenderStatus::Cancelled(RenderCancelledStatus { job_id })
                .emit(&app_handle)
                .ok();
        }
        Err(e) => {
            RenderStatus::Failed(RenderFailedStatus {
                job_id,
                error: e.to_string(),
            })
            .emit(&app_handle)
            .ok();
        }
    }
}

/// `AbortedByCaller` never happens here (the `on_line` closure below never
/// returns `Break`) — kept in the match only because `BlenderRunError`
/// must be matched exhaustively.
fn map_run_error(e: engine::BlenderRunError) -> AppError {
    use engine::BlenderRunError::*;
    match e {
        SpawnFailed(source) => AppError::IoError(format!("Failed to launch Blender: {}", source)),
        StdoutCaptureFailed => AppError::IoError("Failed to capture Blender stdout".to_string()),
        ReadFailed { source, .. } => {
            AppError::IoError(format!("Failed reading Blender output: {}", source))
        }
        WaitFailed { source, .. } => {
            AppError::IoError(format!("Failed waiting for Blender: {}", source))
        }
        Cancelled { .. } | AbortedByCaller { .. } => {
            AppError::UserCancelled("Render cancelled".to_string())
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_blender(
    app_handle: &AppHandle,
    job_id: &str,
    blender: &BlenderInfo,
    script: &Path,
    parts: &[String],
    options: &RenderOptions,
    output_path: &Path,
    cancel_token: &Notify,
) -> Result<(), AppError> {
    let cmd = engine::build_render_command(blender, script, parts, options, output_path);
    let mut last_percent: u32 = 0;

    let run = engine::run_blender_lines(cmd, Some(cancel_token), |line| {
        if let Some((current, total)) = engine::parse_sample_progress(line) {
            let percent = (current * 100) / total;
            if percent != last_percent {
                last_percent = percent;
                RenderStatus::Progress(RenderProgressStatus {
                    job_id: job_id.to_string(),
                    current_sample: current,
                    total_samples: total,
                    percent,
                })
                .emit(app_handle)
                .ok();
            }
        }
        ControlFlow::Continue(())
    })
    .await
    .map_err(map_run_error)?;

    if !run.status.success() {
        return Err(AppError::FileProcessingError(format!(
            "Blender exited with {}\n{}\n{}",
            run.status, run.stdout_tail, run.stderr_tail
        )));
    }

    if !output_path.is_file() {
        return Err(AppError::FileProcessingError(format!(
            "Blender finished but no image was written to {}",
            output_path.display()
        )));
    }

    Ok(())
}
