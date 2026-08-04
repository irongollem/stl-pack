//! Batch preview rendering: many catalog models in ONE Blender launch,
//! because `blender -b` startup cost would otherwise be paid per model.

use crate::catalog::{self, db};
use crate::error::AppError;
use crate::models::events::{
    BatchRenderCancelledStatus, BatchRenderCompletedStatus, BatchRenderFailedStatus,
    BatchRenderModelStatus, BatchRenderProgressStatus, BatchRenderStartedStatus, BatchRenderStatus,
};
use crate::render::commands::ACTIVE_RENDERS;
use crate::render::engine::{self, BatchEntry, BatchLine, BatchManifest};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;
use tauri::AppHandle;
use tauri_specta::Event;
use tokio::sync::Notify;
use uuid::Uuid;

/// Batch job ids carry this prefix in ACTIVE_RENDERS, so
/// batch_render_active() can recognize them.
const BATCH_PREFIX: &str = "batch-render:";

pub fn batch_render_active() -> bool {
    ACTIVE_RENDERS
        .lock()
        .map(|jobs| jobs.keys().any(|id| id.starts_with(BATCH_PREFIX)))
        .unwrap_or(false)
}

/// rotation is the stored "x,y,z" or null → 90,0,0.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct BatchRenderTarget {
    pub dir_path: String,
    pub variant_key: Option<String>,
    pub name: String,
    pub parts: Vec<String>,
    pub rotation: Option<String>,
}

fn parse_rotation(rotation: Option<&str>) -> (f64, f64, f64) {
    let Some(raw) = rotation else {
        return (90.0, 0.0, 0.0);
    };
    let values: Vec<f64> = raw
        .split(',')
        .filter_map(|v| v.trim().parse::<f64>().ok())
        .collect();
    match values.as_slice() {
        [x, y, z] => (*x, *y, *z),
        _ => (90.0, 0.0, 0.0),
    }
}

#[tauri::command]
#[specta::specta]
pub async fn start_batch_render(
    app_handle: AppHandle,
    targets: Vec<BatchRenderTarget>,
) -> Result<String, AppError> {
    // Invalid targets are dropped, not fatal: a file deleted since the
    // candidate list was built shouldn't kill a 500-model sweep.
    let targets: Vec<BatchRenderTarget> = targets
        .into_iter()
        .filter(|t| {
            !t.parts.is_empty()
                && t.parts
                    .iter()
                    .all(|p| p.ends_with(".stl") && Path::new(p).is_file())
        })
        .collect();
    if targets.is_empty() {
        return Err(AppError::InvalidInput(
            "No renderable models in the selection".to_string(),
        ));
    }
    // A scan rewrites the models rows we update per finished model; a pack
    // job deletes the loose STLs Blender is about to read.
    if catalog::commands::job_active("scan:") {
        return Err(AppError::InvalidInput(
            "A catalog scan is running — render previews when it finishes".to_string(),
        ));
    }
    if catalog::commands::job_active("pack:") {
        return Err(AppError::InvalidInput(
            "A pack job is running — render previews when it finishes".to_string(),
        ));
    }
    if batch_render_active() {
        return Err(AppError::InvalidInput(
            "A batch render is already running".to_string(),
        ));
    }

    let blender = engine::detect_blender_cached().await?;
    let script = engine::materialize_render_script(&app_handle)?;

    let job_id = format!("{}{}", BATCH_PREFIX, Uuid::new_v4());
    let scratch = engine::batch_scratch_dir(&app_handle, &job_id)?;
    let manifest = BatchManifest {
        entries: targets
            .iter()
            .enumerate()
            .map(|(index, target)| BatchEntry {
                parts: target.parts.clone(),
                out: scratch
                    .join(format!("{}.png", index))
                    .to_string_lossy()
                    .into_owned(),
                rotate: parse_rotation(target.rotation.as_deref()),
            })
            .collect(),
    };
    let manifest_path = engine::write_batch_manifest(&scratch, &manifest)?;

    let cancel_token = Arc::new(Notify::new());
    if let Ok(mut jobs) = ACTIVE_RENDERS.lock() {
        jobs.insert(job_id.clone(), Arc::clone(&cancel_token));
    }
    BatchRenderStatus::Started(BatchRenderStartedStatus {
        job_id: job_id.clone(),
        total_models: targets.len() as u32,
    })
    .emit(&app_handle)
    .ok();

    let job = job_id.clone();
    tokio::spawn(async move {
        run_batch_job(
            app_handle,
            job,
            blender,
            script,
            targets,
            manifest_path,
            scratch,
            cancel_token,
        )
        .await;
    });
    Ok(job_id)
}

#[derive(Default, Clone)]
struct ModelScratch {
    dims_mm: Option<String>,
    part_count: Option<u32>,
}

#[allow(clippy::too_many_arguments)]
async fn run_batch_job(
    app_handle: AppHandle,
    job_id: String,
    blender: crate::models::BlenderInfo,
    script: std::path::PathBuf,
    targets: Vec<BatchRenderTarget>,
    manifest_path: std::path::PathBuf,
    scratch: std::path::PathBuf,
    cancel_token: Arc<Notify>,
) {
    let started = std::time::Instant::now();
    let total_models = targets.len() as u32;
    let mut succeeded: u32 = 0;
    let mut failed: u32 = 0;

    let result = run_batch_child(
        &app_handle,
        &job_id,
        &blender,
        &script,
        &targets,
        &manifest_path,
        &scratch,
        &cancel_token,
        &mut succeeded,
        &mut failed,
    )
    .await;

    if let Ok(mut jobs) = ACTIVE_RENDERS.lock() {
        jobs.remove(&job_id);
    }
    // The persisted previews were COPIED into app_data by persist_preview —
    // the scratch PNGs and manifest are safe to drop.
    std::fs::remove_dir_all(&scratch).ok();

    match result {
        Ok(()) => {
            BatchRenderStatus::Completed(BatchRenderCompletedStatus {
                job_id,
                succeeded,
                failed,
                total_models,
                elapsed_seconds: started.elapsed().as_secs_f64(),
            })
            .emit(&app_handle)
            .ok();
        }
        Err(AppError::UserCancelled(_)) => {
            BatchRenderStatus::Cancelled(BatchRenderCancelledStatus { job_id, succeeded })
                .emit(&app_handle)
                .ok();
        }
        Err(e) => {
            BatchRenderStatus::Failed(BatchRenderFailedStatus {
                job_id,
                error: e.to_string(),
                succeeded,
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
        // finished models are already persisted — the cancel only abandons
        // the ones not yet rendered
        Cancelled { .. } | AbortedByCaller { .. } => {
            AppError::UserCancelled("Batch render cancelled".to_string())
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_batch_child(
    app_handle: &AppHandle,
    job_id: &str,
    blender: &crate::models::BlenderInfo,
    script: &Path,
    targets: &[BatchRenderTarget],
    manifest_path: &Path,
    scratch: &Path,
    cancel_token: &Notify,
    succeeded: &mut u32,
    failed: &mut u32,
) -> Result<(), AppError> {
    let cmd = engine::build_batch_render_command(blender, script, manifest_path);

    let total_models = targets.len() as u32;
    let mut current_index: u32 = 0;
    let mut finished: u32 = 0;
    let mut last_percent: u32 = 0;
    let mut model_scratch: Vec<ModelScratch> = vec![ModelScratch::default(); targets.len()];

    let run = engine::run_blender_lines(cmd, Some(cancel_token), |line| {
        if let Some(batch_line) = engine::parse_batch_line(line) {
            handle_batch_line(
                app_handle, job_id, targets, scratch, batch_line,
                &mut current_index, &mut finished, &mut last_percent,
                &mut model_scratch, succeeded, failed,
            );
        } else if let Some((current, total)) = engine::parse_sample_progress(line) {
            let model_percent = (current * 100) / total;
            let percent = (finished * 100 + model_percent) / total_models.max(1);
            if percent != last_percent {
                last_percent = percent;
                let name = targets
                    .get(current_index as usize)
                    .map(|t| t.name.clone())
                    .unwrap_or_default();
                BatchRenderStatus::Progress(BatchRenderProgressStatus {
                    job_id: job_id.to_string(),
                    current_model: name,
                    model_index: current_index + 1,
                    total_models,
                    model_percent,
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
    Ok(())
}

/// Completion work (preview copy and DB writes) is synchronous file/SQLite
/// IO measured in milliseconds against renders measured in seconds — done
/// inline, no task juggling.
#[allow(clippy::too_many_arguments)]
fn handle_batch_line(
    app_handle: &AppHandle,
    job_id: &str,
    targets: &[BatchRenderTarget],
    scratch: &Path,
    line: BatchLine,
    current_index: &mut u32,
    finished: &mut u32,
    last_percent: &mut u32,
    model_scratch: &mut [ModelScratch],
    succeeded: &mut u32,
    failed: &mut u32,
) {
    match line {
        BatchLine::Start { .. } => {}
        BatchLine::Model { index } => {
            *current_index = index;
        }
        BatchLine::Measured {
            index,
            dims_mm,
            parts,
        } => {
            if let Some(slot) = model_scratch.get_mut(index as usize) {
                slot.dims_mm = Some(format!(
                    "{:.1}x{:.1}x{:.1}",
                    dims_mm[0], dims_mm[1], dims_mm[2]
                ));
                slot.part_count = Some(parts);
            }
        }
        BatchLine::Done { index, ok, error } => {
            *finished += 1;
            let Some(target) = targets.get(index as usize) else {
                return;
            };
            let out = scratch.join(format!("{}.png", index));
            let mut ok = ok && out.is_file();
            let mut error = error;
            if ok {
                if let Err(e) = persist_finished_model(
                    app_handle,
                    target,
                    &out,
                    model_scratch.get(index as usize),
                ) {
                    ok = false;
                    error = Some(e.to_string());
                }
            }
            if ok {
                *succeeded += 1;
            } else {
                *failed += 1;
            }
            *last_percent = (*finished * 100) / (targets.len() as u32).max(1);
            BatchRenderStatus::ModelFinished(BatchRenderModelStatus {
                job_id: job_id.to_string(),
                model_index: index + 1,
                dir_path: target.dir_path.clone(),
                variant_key: target.variant_key.clone(),
                ok,
                error,
            })
            .emit(app_handle)
            .ok();
        }
    }
}

/// An EXISTING model.json is enriched in place — never created, since
/// creating one would flip the folder to sidecar authority.
fn persist_finished_model(
    app_handle: &AppHandle,
    target: &BatchRenderTarget,
    image: &Path,
    scratch: Option<&ModelScratch>,
) -> Result<(), AppError> {
    catalog::commands::persist_preview(
        app_handle,
        &target.dir_path,
        target.variant_key.as_deref(),
        &image.to_string_lossy(),
    )?;
    let conn = catalog::commands::open_db(app_handle)?;
    if let Some(scratch) = scratch {
        if let (Some(dims), Some(parts)) = (scratch.dims_mm.as_deref(), scratch.part_count) {
            db::set_measured(&conn, &target.dir_path, dims, parts)?;
            catalog::sidecar::merge_measured_into_sidecar(
                Path::new(&target.dir_path),
                dims,
                parts,
                target.rotation.as_deref(),
            )
            .ok(); // sidecar enrichment is best-effort; the index has the data
        }
    }
    Ok(())
}
