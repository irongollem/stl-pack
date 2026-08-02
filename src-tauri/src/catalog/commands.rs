use crate::error::AppError;
use crate::models::events::{
    DuplicateCancelledStatus, DuplicateCompletedStatus, DuplicateFailedStatus,
    DuplicateProgressStatus, DuplicateStartedStatus, DuplicateStatus, GeometryCancelledStatus,
    GeometryCompletedStatus, GeometryFailedStatus, GeometryProgressStatus, GeometryStartedStatus,
    GeometryStatus, PackCancelledStatus, PackCompletedStatus, PackFailedStatus,
    PackProgressStatus, PackStartedStatus, PackStatus, ScanCancelledStatus, ScanCompletedStatus,
    ScanFailedStatus, ScanProgressStatus, ScanStartedStatus, ScanStatus,
};
use once_cell::sync::Lazy;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use uuid::Uuid;

use super::{
    db, dups, geometry, normalize, pack, scanner, BatchOutcome, CatalogEntry, CatalogFile,
    CatalogGroupResult, CatalogSearchResult, CatalogStats, DesignerCount, DuplicateGroup,
    EnsureOutcome, FileVariant, GroupOrigin, ModelFileGeometry, ModelMetaUpdate, MoveOperation,
    NormalizeOp, NormalizePlan, ReleaseSummary, TagCount,
};

/// Scan and duplicate jobs share one registry; both cancel through
/// cancel_catalog_job.
static ACTIVE_CATALOG_JOBS: Lazy<Mutex<HashMap<String, Arc<AtomicBool>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(200);

fn db_path(app_handle: &AppHandle) -> Result<PathBuf, AppError> {
    Ok(app_handle
        .path()
        .app_data_dir()
        .map_err(|e| AppError::ConfigError(format!("No app data dir: {}", e)))?
        .join("catalog.db"))
}

pub(crate) fn open_db(app_handle: &AppHandle) -> Result<Connection, AppError> {
    db::open(&db_path(app_handle)?)
}

fn register_job(job_id: &str) -> Result<Arc<AtomicBool>, AppError> {
    let cancel = Arc::new(AtomicBool::new(false));
    ACTIVE_CATALOG_JOBS
        .lock()
        .map_err(|e| AppError::ConfigError(format!("Job registry unavailable: {}", e)))?
        .insert(job_id.to_string(), Arc::clone(&cancel));
    Ok(cancel)
}

fn unregister_job(job_id: &str) {
    if let Ok(mut jobs) = ACTIVE_CATALOG_JOBS.lock() {
        jobs.remove(job_id);
    }
}

/// Job ids are prefixed by kind ("scan:", "dup:", "pack:", "extract:") so
/// mutually-unsafe kinds can exclude each other: a scan's replace_catalog
/// wholesale-rewrites the very rows a pack job updates in place, so letting
/// them overlap leaves the index claiming loose files that only exist inside
/// an archive.
pub(crate) fn job_active(prefix: &str) -> bool {
    ACTIVE_CATALOG_JOBS
        .lock()
        .map(|jobs| jobs.keys().any(|id| id.starts_with(prefix)))
        .unwrap_or(false)
}

/// Trailing-separator-insensitive form of a root path — must agree with the
/// scoping in db::replace_catalog or the same folder scans as two roots.
/// pub(crate): basecutter::commands::export_cuts_to_catalog reuses this to
/// check its `root` argument against the same normalized catalog_roots list,
/// rather than growing a second trimming convention.
pub(crate) fn normalized_root(path: &str) -> String {
    let trimmed = path.trim_end_matches(std::path::MAIN_SEPARATOR);
    if trimmed.is_empty() {
        path.to_string()
    } else {
        trimmed.to_string()
    }
}

/// The configured roots list, normalized. Settings migration (single
/// catalog_root seeding the list) already happened inside get_settings.
fn normalized_roots(settings: &crate::models::Settings) -> Vec<String> {
    settings
        .catalog_roots
        .clone()
        .unwrap_or_default()
        .iter()
        .map(|r| normalized_root(r))
        .collect()
}

/// The configured roots as paths, for callers that resolve dirs against
/// the whole list (the normalizer) rather than scanning one root.
async fn configured_roots(app_handle: &AppHandle) -> Result<Vec<PathBuf>, AppError> {
    let settings = crate::settings::get_settings(app_handle.clone())
        .await
        .map_err(AppError::ConfigError)?;
    Ok(normalized_roots(&settings)
        .into_iter()
        .map(PathBuf::from)
        .collect())
}

/// The configured primary (staging) folder, but only while it's actually
/// one of the roots — a stale setting pointing at a removed folder must
/// read as "no primary", not send cleanups into a folder we don't index.
fn valid_primary(settings: &crate::models::Settings, roots: &[String]) -> Option<String> {
    settings
        .catalog_primary_root
        .as_deref()
        .map(normalized_root)
        .filter(|p| roots.iter().any(|r| r == p))
}

/// Persist a changed roots list. catalog_root mirrors the first entry so a
/// pre-multi-root build (or not-yet-migrated UI code) reading the same
/// store stays coherent instead of resurrecting a removed folder.
async fn save_roots(
    app_handle: &AppHandle,
    mut settings: crate::models::Settings,
    roots: Vec<String>,
) -> Result<(), AppError> {
    settings.catalog_root = roots.first().cloned();
    settings.catalog_roots = if roots.is_empty() { None } else { Some(roots) };
    crate::settings::set_settings(app_handle.clone(), settings)
        .await
        .map_err(AppError::ConfigError)
}

/// Reject a root that nests inside (or swallows) a configured one — two
/// overlapping roots would index the same dirs twice and their scoped
/// scans would fight over the shared rows.
fn overlap_error(roots: &[String], root: &str) -> Result<(), AppError> {
    if let Some(overlap) = roots
        .iter()
        .find(|r| normalize::is_under(root, r) || normalize::is_under(r, root))
    {
        return Err(AppError::ConfigError(format!(
            "'{}' overlaps the configured catalog folder '{}' — the same models would be indexed twice",
            root, overlap
        )));
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn start_catalog_scan(app_handle: AppHandle, root: String) -> Result<String, AppError> {
    if !Path::new(&root).is_dir() {
        return Err(AppError::NotFoundError(format!(
            "Catalog root '{}' is not a directory",
            root
        )));
    }

    if job_active("pack:") {
        return Err(AppError::InvalidInput(
            "A pack job is running — rescan when it finishes".to_string(),
        ));
    }
    if crate::render::batch::batch_render_active() {
        // replace_catalog would rewrite the very rows the batch updates
        // (previews, measured geometry) per finished model
        return Err(AppError::InvalidInput(
            "A batch render is running — rescan when it finishes".to_string(),
        ));
    }

    // Settings are read up front (async store) so the blocking scan can
    // borrow the designer lexicon — and so the root joins the configured
    // list before any rows land under it.
    let settings = crate::settings::get_settings(app_handle.clone())
        .await
        .map_err(AppError::ConfigError)?;
    let designers = settings
        .known_designers
        .clone()
        .filter(|list| !list.is_empty())
        .unwrap_or_else(crate::settings::default_designers);

    // Scanning a folder registers it: the "add one designer folder at a
    // time" flow is pick-and-scan, not a separate management step.
    let root = normalized_root(&root);
    let mut roots = normalized_roots(&settings);
    if !roots.contains(&root) {
        overlap_error(&roots, &root)?;
        roots.push(root.clone());
        save_roots(&app_handle, settings, roots).await?;
    }

    let job_id = format!("scan:{}", Uuid::new_v4());
    let cancel = register_job(&job_id)?;
    let job_id_clone = job_id.clone();

    tauri::async_runtime::spawn_blocking(move || {
        let started = Instant::now();
        ScanStatus::Started(ScanStartedStatus {
            job_id: job_id_clone.clone(),
            root: root.clone(),
        })
        .emit(&app_handle)
        .ok();

        let result = (|| -> Result<(u32, u32), AppError> {
            let mut last_emit = Instant::now();
            let progress_app = app_handle.clone();
            let progress_job = job_id_clone.clone();
            let outcome = scanner::scan(
                Path::new(&root),
                &cancel,
                &designers,
                |files_indexed, current_dir| {
                    if last_emit.elapsed() >= PROGRESS_EMIT_INTERVAL {
                        last_emit = Instant::now();
                        ScanStatus::Progress(ScanProgressStatus {
                            job_id: progress_job.clone(),
                            files_indexed,
                            current_dir: current_dir.to_string(),
                        })
                        .emit(&progress_app)
                        .ok();
                    }
                },
            )?;

            let mut conn = open_db(&app_handle)?;
            db::replace_catalog(
                &mut conn,
                &root,
                &outcome.files,
                &outcome.models,
                &outcome.metadata_tags,
                &outcome.metadata_file_variants,
                &outcome.packs,
            )?;
            Ok((outcome.files.len() as u32, outcome.models.len() as u32))
        })();

        unregister_job(&job_id_clone);
        match result {
            Ok((total_files, total_models)) => {
                ScanStatus::Completed(ScanCompletedStatus {
                    job_id: job_id_clone,
                    total_files,
                    total_models,
                    elapsed_seconds: started.elapsed().as_secs_f64(),
                })
                .emit(&app_handle)
                .ok();
            }
            Err(AppError::UserCancelled(_)) => {
                ScanStatus::Cancelled(ScanCancelledStatus {
                    job_id: job_id_clone,
                })
                .emit(&app_handle)
                .ok();
            }
            Err(e) => {
                ScanStatus::Failed(ScanFailedStatus {
                    job_id: job_id_clone,
                    error: e.to_string(),
                })
                .emit(&app_handle)
                .ok();
            }
        }
    });

    Ok(job_id)
}

/// The configured catalog folders with their indexed footprint. Folders the
/// user added but never scanned report zero counts and no last_scan.
#[tauri::command]
#[specta::specta]
pub async fn list_catalog_roots(
    app_handle: AppHandle,
) -> Result<Vec<super::CatalogRootSummary>, AppError> {
    let settings = crate::settings::get_settings(app_handle.clone())
        .await
        .map_err(AppError::ConfigError)?;
    let roots = normalized_roots(&settings);
    let primary = valid_primary(&settings, &roots);
    let conn = open_db(&app_handle)?;
    let scan_times: HashMap<String, i64> = db::root_scan_times(&conn)?.into_iter().collect();
    let mut out = Vec::with_capacity(roots.len());
    for root in roots {
        let (model_count, file_count, total_bytes) = db::root_summary(&conn, &root)?;
        out.push(super::CatalogRootSummary {
            last_scan_epoch: scan_times.get(&root).map(|t| *t as f64),
            primary: primary.as_deref() == Some(root.as_str()),
            root,
            model_count,
            file_count,
            total_size_bytes: total_bytes as f64,
        });
    }
    Ok(out)
}

/// Register a catalog folder without scanning it yet. Returns the updated
/// list. Adding is idempotent; a folder nested in (or swallowing) a
/// configured one is rejected — overlapping roots would double-index.
#[tauri::command]
#[specta::specta]
pub async fn add_catalog_root(
    app_handle: AppHandle,
    path: String,
) -> Result<Vec<String>, AppError> {
    if !Path::new(&path).is_dir() {
        return Err(AppError::NotFoundError(format!(
            "Catalog root '{}' is not a directory",
            path
        )));
    }
    let root = normalized_root(&path);
    let settings = crate::settings::get_settings(app_handle.clone())
        .await
        .map_err(AppError::ConfigError)?;
    let mut roots = normalized_roots(&settings);
    if roots.contains(&root) {
        return Ok(roots);
    }
    overlap_error(&roots, &root)?;
    roots.push(root);
    save_roots(&app_handle, settings, roots.clone()).await?;
    Ok(roots)
}

/// Drop a catalog folder and purge its slice of the index. The disk is
/// never touched; user tags/metadata for the purged models go with them
/// (their durable home is the model.json sidecars), so re-adding the
/// folder later just means a rescan.
#[tauri::command]
#[specta::specta]
pub async fn remove_catalog_root(
    app_handle: AppHandle,
    path: String,
) -> Result<Vec<String>, AppError> {
    let root = normalized_root(&path);
    let mut settings = crate::settings::get_settings(app_handle.clone())
        .await
        .map_err(AppError::ConfigError)?;
    let mut roots = normalized_roots(&settings);
    roots.retain(|r| r != &root);
    // A primary that leaves the catalog stops being the staging target —
    // a dangling value would silently do nothing (valid_primary filters
    // it), but showing it as still-set in the UI would lie.
    if settings
        .catalog_primary_root
        .as_deref()
        .map(normalized_root)
        .as_deref()
        == Some(root.as_str())
    {
        settings.catalog_primary_root = None;
    }
    save_roots(&app_handle, settings, roots.clone()).await?;
    let mut conn = open_db(&app_handle)?;
    db::purge_root(&mut conn, &root)?;
    Ok(roots)
}

/// Choose (or clear, with None) the staging folder Clean up drains the
/// other folders into. Must already be a configured catalog folder.
#[tauri::command]
#[specta::specta]
pub async fn set_primary_catalog_root(
    app_handle: AppHandle,
    path: Option<String>,
) -> Result<(), AppError> {
    let mut settings = crate::settings::get_settings(app_handle.clone())
        .await
        .map_err(AppError::ConfigError)?;
    let primary = match path.as_deref().map(normalized_root) {
        Some(p) => {
            let roots = normalized_roots(&settings);
            if !roots.iter().any(|r| r == &p) {
                return Err(AppError::InvalidInput(format!(
                    "'{}' is not a configured catalog folder",
                    p
                )));
            }
            Some(p)
        }
        None => None,
    };
    settings.catalog_primary_root = primary;
    crate::settings::set_settings(app_handle, settings)
        .await
        .map_err(AppError::ConfigError)
}

#[tauri::command]
#[specta::specta]
pub async fn start_duplicate_scan(app_handle: AppHandle) -> Result<String, AppError> {
    if job_active("pack:") {
        // the dup scan reads file bytes a pack job is busy deleting
        return Err(AppError::InvalidInput(
            "A pack job is running — scan for duplicates when it finishes".to_string(),
        ));
    }
    let job_id = format!("dup:{}", Uuid::new_v4());
    let cancel = register_job(&job_id)?;
    let job_id_clone = job_id.clone();

    tauri::async_runtime::spawn_blocking(move || {
        DuplicateStatus::Started(DuplicateStartedStatus {
            job_id: job_id_clone.clone(),
        })
        .emit(&app_handle)
        .ok();

        let result = (|| -> Result<Vec<DuplicateGroup>, AppError> {
            let conn = open_db(&app_handle)?;
            let mut last_emit = Instant::now();
            let progress_app = app_handle.clone();
            let progress_job = job_id_clone.clone();
            dups::find_duplicates(&conn, &cancel, |processed, total| {
                if last_emit.elapsed() >= PROGRESS_EMIT_INTERVAL {
                    last_emit = Instant::now();
                    DuplicateStatus::Progress(DuplicateProgressStatus {
                        job_id: progress_job.clone(),
                        processed,
                        total,
                    })
                    .emit(&progress_app)
                    .ok();
                }
            })
        })();

        unregister_job(&job_id_clone);
        match result {
            Ok(groups) => {
                let wasted: f64 = groups
                    .iter()
                    .map(|g| g.size_bytes * (g.paths.len().saturating_sub(1)) as f64)
                    .sum();
                DuplicateStatus::Completed(DuplicateCompletedStatus {
                    job_id: job_id_clone,
                    group_count: groups.len() as u32,
                    wasted_bytes: wasted,
                })
                .emit(&app_handle)
                .ok();
            }
            Err(AppError::UserCancelled(_)) => {
                DuplicateStatus::Cancelled(DuplicateCancelledStatus {
                    job_id: job_id_clone,
                })
                .emit(&app_handle)
                .ok();
            }
            Err(e) => {
                DuplicateStatus::Failed(DuplicateFailedStatus {
                    job_id: job_id_clone,
                    error: e.to_string(),
                })
                .emit(&app_handle)
                .ok();
            }
        }
    });

    Ok(job_id)
}

/// Backend mining stage of issue #15: parse every loose STL the catalog
/// knows about for bbox/volume/open-edge facts, one row per distinct
/// content hash. Mirrors start_duplicate_scan's shape exactly — same job
/// registry, same "pack:" exclusion (a pack job deletes the loose bytes
/// mining is busy reading), same throttled progress stream.
#[tauri::command]
#[specta::specta]
pub async fn start_geometry_scan(app_handle: AppHandle) -> Result<String, AppError> {
    if job_active("pack:") {
        // mining reads file bytes a pack job is busy deleting
        return Err(AppError::InvalidInput(
            "A pack job is running — mine geometry when it finishes".to_string(),
        ));
    }
    // Edge-stats cap from settings (async store read) before the blocking
    // job — mirrors the pack_level read above start_pack. Falls back to the
    // machine-derived recommendation if settings can't be read at all, same
    // as a first-load seed would, rather than failing the whole scan.
    let edge_cap = crate::settings::get_settings(app_handle.clone())
        .await
        .ok()
        .and_then(|s| s.edge_stats_max_tris)
        .unwrap_or_else(geometry::recommended_edge_cap);

    let job_id = format!("geom:{}", Uuid::new_v4());
    let cancel = register_job(&job_id)?;
    let job_id_clone = job_id.clone();

    tauri::async_runtime::spawn_blocking(move || {
        GeometryStatus::Started(GeometryStartedStatus {
            job_id: job_id_clone.clone(),
        })
        .emit(&app_handle)
        .ok();

        let result = (|| -> Result<geometry::GeometryOutcome, AppError> {
            let conn = open_db(&app_handle)?;
            let mut last_emit = Instant::now();
            let progress_app = app_handle.clone();
            let progress_job = job_id_clone.clone();
            geometry::mine_geometry(&conn, &cancel, edge_cap, |processed, total| {
                if last_emit.elapsed() >= PROGRESS_EMIT_INTERVAL {
                    last_emit = Instant::now();
                    GeometryStatus::Progress(GeometryProgressStatus {
                        job_id: progress_job.clone(),
                        processed,
                        total,
                    })
                    .emit(&progress_app)
                    .ok();
                }
            })
        })();

        unregister_job(&job_id_clone);
        match result {
            Ok(outcome) => {
                GeometryStatus::Completed(GeometryCompletedStatus {
                    job_id: job_id_clone,
                    mined: outcome.mined,
                    already_known: outcome.already_known,
                    failed: outcome.failed,
                })
                .emit(&app_handle)
                .ok();
            }
            Err(AppError::UserCancelled(_)) => {
                GeometryStatus::Cancelled(GeometryCancelledStatus {
                    job_id: job_id_clone,
                })
                .emit(&app_handle)
                .ok();
            }
            Err(e) => {
                GeometryStatus::Failed(GeometryFailedStatus {
                    job_id: job_id_clone,
                    error: e.to_string(),
                })
                .emit(&app_handle)
                .ok();
            }
        }
    });

    Ok(job_id)
}

/// The edge-stats triangle cap this machine's RAM would suggest, for the
/// settings UI's "Auto" control to show/restore without first saving a
/// value — settings::get_settings seeds edge_stats_max_tris to the same
/// number on first load, but a user who already has a stored value needs a
/// way to see (and revert to) what "Auto" would pick without overwriting
/// their setting first.
#[tauri::command]
#[specta::specta]
pub async fn get_recommended_edge_cap() -> Result<u32, AppError> {
    Ok(geometry::recommended_edge_cap())
}

#[tauri::command]
#[specta::specta]
pub async fn cancel_catalog_job(job_id: String) -> Result<(), AppError> {
    let jobs = ACTIVE_CATALOG_JOBS
        .lock()
        .map_err(|e| AppError::ConfigError(format!("Job registry unavailable: {}", e)))?;
    match jobs.get(&job_id) {
        Some(cancel) => {
            cancel.store(true, Ordering::SeqCst);
            Ok(())
        }
        None => Err(AppError::NotFoundError(format!(
            "No active catalog job with ID: {}",
            job_id
        ))),
    }
}

/// Compress each model dir into a model.plinthpack (compressed at rest),
/// sequentially — per-model atomicity keeps the crash surface to one model,
/// and a cancelled or crashed batch resumes by re-running the same selection
/// (already-packed models just finish their bookkeeping and count as done).
/// Cancel via cancel_catalog_job; progress via PackStatus events.
#[tauri::command]
#[specta::specta]
pub async fn pack_models(
    app_handle: AppHandle,
    model_dirs: Vec<String>,
) -> Result<String, AppError> {
    // Dedup while preserving order: a selection can name the same dir twice
    // (drawer + bulk overlap), and packing one dir twice in a batch is at
    // best wasted repair work
    let mut seen_dirs = std::collections::HashSet::new();
    let model_dirs: Vec<String> = model_dirs
        .into_iter()
        .filter(|d| seen_dirs.insert(d.clone()))
        .collect();
    if model_dirs.is_empty() {
        return Err(AppError::InvalidInput("No models to pack".to_string()));
    }
    if job_active("scan:") {
        return Err(AppError::InvalidInput(
            "A catalog scan is running — pack when it finishes".to_string(),
        ));
    }
    if job_active("pack:") {
        return Err(AppError::InvalidInput(
            "A pack job is already running".to_string(),
        ));
    }
    if crate::render::batch::batch_render_active() {
        // packing deletes the loose STLs Blender is reading mid-batch
        return Err(AppError::InvalidInput(
            "A batch render is running — pack when it finishes".to_string(),
        ));
    }
    // Zstd level from settings (async store read) before the blocking job.
    // Clamped to zstd's actual range — the zip writer errors on anything
    // outside it, and a hand-edited settings.json shouldn't brick packing.
    let level = crate::settings::get_settings(app_handle.clone())
        .await
        .ok()
        .and_then(|s| s.pack_level)
        .map(|l| i64::from(l).clamp(-7, 22));
    let app_version = app_handle.package_info().version.to_string();

    let job_id = format!("pack:{}", Uuid::new_v4());
    let cancel = register_job(&job_id)?;
    let total_models = model_dirs.len() as u32;
    PackStatus::Started(PackStartedStatus {
        job_id: job_id.clone(),
        action: "pack".to_string(),
        total_models,
    })
    .emit(&app_handle)
    .ok();

    let job_id_clone = job_id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let started = Instant::now();
        let mut succeeded = 0u32;
        let mut kept_files: Vec<String> = Vec::new();
        let result: Result<(), AppError> = (|| {
            let mut conn = open_db(&app_handle)?;
            // Batch-wide percent: compress + verify each stream the model's
            // bytes once, so the denominator is twice the loose total
            let mut total_kb: u64 = 0;
            for dir in &model_dirs {
                total_kb += (db::dir_size_bytes(&conn, dir)?.max(0) as u64) / 1024;
            }
            let total_kb = (total_kb * 2).max(1);
            let mut processed_kb: u64 = 0;
            let mut last_emit = Instant::now() - PROGRESS_EMIT_INTERVAL;

            for (index, dir) in model_dirs.iter().enumerate() {
                if cancel.load(Ordering::SeqCst) {
                    return Err(AppError::UserCancelled("Pack cancelled".to_string()));
                }
                let outcome = pack::pack_model(
                    &app_version,
                    Path::new(dir),
                    level,
                    &cancel,
                    |phase, kb| {
                        processed_kb += kb as u64;
                        if last_emit.elapsed() >= PROGRESS_EMIT_INTERVAL {
                            last_emit = Instant::now();
                            PackStatus::Progress(PackProgressStatus {
                                job_id: job_id_clone.clone(),
                                phase: match phase {
                                    pack::PackPhase::Compress => "compress".to_string(),
                                    pack::PackPhase::Verify => "verify".to_string(),
                                },
                                current_model: dir.clone(),
                                model_index: index as u32 + 1,
                                total_models,
                                processed_size_kb: processed_kb.min(u32::MAX as u64) as u32,
                                total_size_kb: total_kb.min(u32::MAX as u64) as u32,
                                percent: ((processed_kb * 100) / total_kb).min(100) as u32,
                            })
                            .emit(&app_handle)
                            .ok();
                        }
                        true
                    },
                )?;
                db::mark_packed(&mut conn, dir, &outcome.sidecar, &outcome.kept)?;
                kept_files.extend(outcome.kept);
                succeeded += 1;
            }
            Ok(())
        })();

        unregister_job(&job_id_clone);
        match result {
            Ok(()) => {
                PackStatus::Completed(PackCompletedStatus {
                    job_id: job_id_clone,
                    action: "pack".to_string(),
                    succeeded,
                    total_models,
                    kept_files,
                    elapsed_seconds: started.elapsed().as_secs_f64(),
                })
                .emit(&app_handle)
                .ok();
            }
            Err(AppError::UserCancelled(_)) => {
                PackStatus::Cancelled(PackCancelledStatus {
                    job_id: job_id_clone,
                    succeeded,
                })
                .emit(&app_handle)
                .ok();
            }
            Err(e) => {
                PackStatus::Failed(PackFailedStatus {
                    job_id: job_id_clone,
                    error: e.to_string(),
                    succeeded,
                })
                .emit(&app_handle)
                .ok();
            }
        }
    });

    Ok(job_id)
}

/// Make `paths` readable on disk, extracting packed ones from their
/// archives as EPHEMERAL working copies — the archive stays authoritative
/// and cleanup_ephemeral_files takes the copies back. Awaits completion:
/// the promise resolving means the bytes are there, so callers just chain
/// their print/preview/render after it. Progress rides the PackStatus
/// stream (phase "extract"); the Started event carries the job_id the
/// frontend can feed to cancel_catalog_job.
#[tauri::command]
#[specta::specta]
pub async fn ensure_model_files(
    app_handle: AppHandle,
    paths: Vec<String>,
) -> Result<EnsureOutcome, AppError> {
    let job_id = format!("extract:{}", Uuid::new_v4());
    let cancel = register_job(&job_id)?;
    let job_id_clone = job_id.clone();
    let result = tauri::async_runtime::spawn_blocking(move || -> Result<EnsureOutcome, AppError> {
        let started = Instant::now();
        let conn = open_db(&app_handle)?;
        let archives = db::archive_paths_for(&conn, &paths)?;
        let already_loose = (paths.len() - archives.len()) as u32;
        if archives.is_empty() {
            return Ok(EnsureOutcome {
                extracted: Vec::new(),
                already_loose,
            });
        }

        // Group the packed paths per model dir so each archive opens once
        let mut by_dir: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for (path, archive) in &archives {
            let Some(model_dir) = Path::new(archive)
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
            else {
                continue;
            };
            by_dir.entry(model_dir).or_default().push(path.clone());
        }
        let total_models = by_dir.len() as u32;
        PackStatus::Started(PackStartedStatus {
            job_id: job_id_clone.clone(),
            action: "extract".to_string(),
            total_models,
        })
        .emit(&app_handle)
        .ok();

        let run = || -> Result<Vec<String>, AppError> {
            let mut extracted: Vec<String> = Vec::new();
            for (index, (dir, wanted)) in by_dir.iter().enumerate() {
                if cancel.load(Ordering::SeqCst) {
                    return Err(AppError::UserCancelled("Extraction cancelled".to_string()));
                }
                PackStatus::Progress(PackProgressStatus {
                    job_id: job_id_clone.clone(),
                    phase: "extract".to_string(),
                    current_model: dir.clone(),
                    model_index: index as u32 + 1,
                    total_models,
                    processed_size_kb: 0,
                    total_size_kb: 0,
                    percent: (index as u32 * 100) / total_models.max(1),
                })
                .emit(&app_handle)
                .ok();
                let got = pack::extract_paths_ephemeral(Path::new(dir), wanted, &cancel, |_| {
                    !cancel.load(Ordering::SeqCst)
                })?;
                extracted.extend(got);
            }
            Ok(extracted)
        };
        let outcome = run();
        match &outcome {
            Ok(extracted) => {
                PackStatus::Completed(PackCompletedStatus {
                    job_id: job_id_clone.clone(),
                    action: "extract".to_string(),
                    succeeded: extracted.len() as u32,
                    total_models,
                    kept_files: Vec::new(),
                    elapsed_seconds: started.elapsed().as_secs_f64(),
                })
                .emit(&app_handle)
                .ok();
            }
            Err(AppError::UserCancelled(_)) => {
                PackStatus::Cancelled(PackCancelledStatus {
                    job_id: job_id_clone.clone(),
                    succeeded: 0,
                })
                .emit(&app_handle)
                .ok();
            }
            Err(e) => {
                PackStatus::Failed(PackFailedStatus {
                    job_id: job_id_clone.clone(),
                    error: e.to_string(),
                    succeeded: 0,
                })
                .emit(&app_handle)
                .ok();
            }
        }
        Ok(EnsureOutcome {
            extracted: outcome?,
            already_loose,
        })
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Extraction task failed: {}", e)))?;
    unregister_job(&job_id);
    result
}

/// Take back the working copies ensure_model_files materialized — the
/// requested paths, or every live extract when the list is empty (the
/// app-exit sweep). Files that changed since extraction are reported and
/// kept: they're the user's data now (saved supports, edits).
#[tauri::command]
#[specta::specta]
pub async fn cleanup_ephemeral_files(paths: Vec<String>) -> Result<BatchOutcome, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let (removed, kept) = pack::cleanup_ephemeral(&paths);
        Ok(BatchOutcome {
            succeeded: removed.len() as u32,
            errors: kept
                .into_iter()
                .map(|p| format!("{}: changed since extraction — kept on disk", p))
                .collect(),
        })
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Cleanup task failed: {}", e)))?
}

/// Which model folders a bulk pack would touch: everything with loose model
/// files under the given designer facet and/or checked group names. The
/// frontend shows the count in the confirm dialog, then feeds the same list
/// to pack_models — one resumable job for a whole designer.
#[tauri::command]
#[specta::specta]
pub async fn get_pack_candidates(
    app_handle: AppHandle,
    designer: Option<String>,
    groups: Vec<String>,
) -> Result<Vec<String>, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        db::pack_candidate_dirs(&conn, designer.as_deref(), &groups)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Pack candidate task failed: {}", e)))?
}

/// Restore packed models to loose files (archive + sidecar removed), the
/// mirror of pack_models: sequential, cancellable between models, index
/// updated per model so no rescan is needed.
#[tauri::command]
#[specta::specta]
pub async fn unpack_models(
    app_handle: AppHandle,
    model_dirs: Vec<String>,
) -> Result<String, AppError> {
    if model_dirs.is_empty() {
        return Err(AppError::InvalidInput("No models to unpack".to_string()));
    }
    if job_active("scan:") {
        return Err(AppError::InvalidInput(
            "A catalog scan is running — unpack when it finishes".to_string(),
        ));
    }
    if job_active("pack:") {
        return Err(AppError::InvalidInput(
            "A pack job is already running".to_string(),
        ));
    }
    if crate::render::batch::batch_render_active() {
        return Err(AppError::InvalidInput(
            "A batch render is running — unpack when it finishes".to_string(),
        ));
    }
    let job_id = format!("pack:{}", Uuid::new_v4());
    let cancel = register_job(&job_id)?;
    let total_models = model_dirs.len() as u32;
    PackStatus::Started(PackStartedStatus {
        job_id: job_id.clone(),
        action: "unpack".to_string(),
        total_models,
    })
    .emit(&app_handle)
    .ok();

    let job_id_clone = job_id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let started = Instant::now();
        let mut succeeded = 0u32;
        let mut preserved_files: Vec<String> = Vec::new();
        let result: Result<(), AppError> = (|| {
            let mut conn = open_db(&app_handle)?;
            for (index, dir) in model_dirs.iter().enumerate() {
                if cancel.load(Ordering::SeqCst) {
                    return Err(AppError::UserCancelled("Unpack cancelled".to_string()));
                }
                // Extraction has no per-file callback; per-model progress is
                // plenty at this granularity
                PackStatus::Progress(PackProgressStatus {
                    job_id: job_id_clone.clone(),
                    phase: "extract".to_string(),
                    current_model: dir.clone(),
                    model_index: index as u32 + 1,
                    total_models,
                    processed_size_kb: 0,
                    total_size_kb: 0,
                    percent: ((index as u32) * 100) / total_models.max(1),
                })
                .emit(&app_handle)
                .ok();

                let outcome = pack::unpack_model(Path::new(dir))?;
                preserved_files.extend(outcome.preserved.iter().cloned());
                // Fresh stats for the index: extraction stamps new mtimes,
                // and recording them alongside the kept content_hash is what
                // keeps the next rescan from dropping the hash as "changed".
                // A transiently unstatable file (NAS hiccup) falls back to
                // the sidecar's stats — its row MUST still flip to loose,
                // because the archive it pointed at is already gone.
                let fresh: Vec<(String, i64, i64)> = outcome
                    .files
                    .iter()
                    .map(|entry| {
                        let path = pack::entry_disk_path(Path::new(dir), &entry.name);
                        let (size_bytes, modified_at) = match std::fs::metadata(&path) {
                            Ok(metadata) => (
                                metadata.len() as i64,
                                metadata
                                    .modified()
                                    .ok()
                                    .and_then(|t| {
                                        t.duration_since(std::time::UNIX_EPOCH).ok()
                                    })
                                    .map(|d| d.as_secs() as i64)
                                    .unwrap_or(0),
                            ),
                            Err(_) => (entry.size_bytes as i64, entry.modified_at),
                        };
                        (path.to_string_lossy().into_owned(), size_bytes, modified_at)
                    })
                    .collect();
                db::mark_unpacked(&mut conn, dir, &fresh)?;
                succeeded += 1;
            }
            Ok(())
        })();

        unregister_job(&job_id_clone);
        match result {
            Ok(()) => {
                PackStatus::Completed(PackCompletedStatus {
                    job_id: job_id_clone,
                    action: "unpack".to_string(),
                    succeeded,
                    total_models,
                    // diverged loose files moved aside as "(edited)" — the
                    // user must hear about these
                    kept_files: preserved_files,
                    elapsed_seconds: started.elapsed().as_secs_f64(),
                })
                .emit(&app_handle)
                .ok();
            }
            Err(AppError::UserCancelled(_)) => {
                PackStatus::Cancelled(PackCancelledStatus {
                    job_id: job_id_clone,
                    succeeded,
                })
                .emit(&app_handle)
                .ok();
            }
            Err(e) => {
                PackStatus::Failed(PackFailedStatus {
                    job_id: job_id_clone,
                    error: e.to_string(),
                    succeeded,
                })
                .emit(&app_handle)
                .ok();
            }
        }
    });

    Ok(job_id)
}

#[tauri::command]
#[specta::specta]
pub async fn search_catalog(
    app_handle: AppHandle,
    query: String,
    tags: Vec<String>,
    limit: u32,
    offset: u32,
) -> Result<CatalogSearchResult, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        let include_nsfw = crate::content_filter::is_unlocked();
        let page = db::search(&conn, &query, &tags, limit.min(200), offset, include_nsfw)?;
        Ok(CatalogSearchResult {
            entries: page.entries,
            total: page.total,
        })
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Search task failed: {}", e)))?
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
#[specta::specta]
pub async fn search_catalog_groups(
    app_handle: AppHandle,
    query: String,
    tags: Vec<String>,
    designer: Option<String>,
    sort: Option<String>,
    limit: u32,
    offset: u32,
) -> Result<CatalogGroupResult, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        let include_nsfw = crate::content_filter::is_unlocked();
        let page = db::search_groups(
            &conn,
            &query,
            &tags,
            designer.as_deref(),
            sort.as_deref().unwrap_or("name"),
            limit.min(200),
            offset,
            include_nsfw,
        )?;
        Ok(CatalogGroupResult {
            groups: page.groups,
            total: page.total,
        })
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Group search task failed: {}", e)))?
}

#[tauri::command]
#[specta::specta]
pub async fn get_catalog_group_members(
    app_handle: AppHandle,
    group_name: String,
) -> Result<Vec<CatalogEntry>, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        let include_nsfw = crate::content_filter::is_unlocked();
        db::group_members(&conn, &group_name, include_nsfw)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Group member task failed: {}", e)))?
}

#[tauri::command]
#[specta::specta]
pub async fn rename_catalog_group(
    app_handle: AppHandle,
    group_name: String,
    new_name: String,
) -> Result<(), AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        db::rename_group(&conn, &group_name, &new_name)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Group rename task failed: {}", e)))?
}

/// The scanner-level groups shown under one card. Length > 1 means the card
/// was combined (or renamed into a collision) and can be split — the UI
/// offers "split" exactly then, and rename-to-empty performs it.
#[tauri::command]
#[specta::specta]
pub async fn get_catalog_group_sources(
    app_handle: AppHandle,
    group_name: String,
) -> Result<Vec<String>, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        db::group_sources(&conn, &group_name)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Group source task failed: {}", e)))?
}

/// Where a rename/combine of `group_name` would actually reach — call this
/// before committing one, so the UI can warn when a generic scanner-derived
/// name ("Spear") turns out to also belong to an unrelated designer/release
/// (see the group_renames CREATE TABLE comment: no root/designer scoping).
#[tauri::command]
#[specta::specta]
pub async fn get_group_rename_origins(
    app_handle: AppHandle,
    group_name: String,
) -> Result<Vec<GroupOrigin>, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        db::group_rename_origins(&conn, &group_name)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Group origin task failed: {}", e)))?
}

/// Remove one mis-combined source from a card (its rename row) so it comes
/// back as its own card — the surgical undo, next to full split.
#[tauri::command]
#[specta::specta]
pub async fn detach_catalog_group_source(
    app_handle: AppHandle,
    group_name: String,
    source_group: String,
) -> Result<(), AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        db::detach_group_source(&conn, &group_name, &source_group)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Detach task failed: {}", e)))?
}

/// Reset a card's auto-config: clear the scanner-inferred variant/pose on
/// every member and drop every per-file pose assignment, collapsing the card
/// to one flat file list the user can re-file by hand. Returns the number of
/// file assignments cleared. Nothing moves on disk.
#[tauri::command]
#[specta::specta]
pub async fn flatten_catalog_group(
    app_handle: AppHandle,
    group_name: String,
) -> Result<u32, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        db::flatten_group(&conn, &group_name)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Flatten task failed: {}", e)))?
}

/// Pick which member's image fronts the group's card.
#[tauri::command]
#[specta::specta]
pub async fn set_group_cover(
    app_handle: AppHandle,
    group_name: String,
    dir_path: String,
    variant_key: Option<String>,
) -> Result<(), AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        db::set_group_cover(&conn, &group_name, &dir_path, variant_key.as_deref())
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Cover task failed: {}", e)))?
}

#[tauri::command]
#[specta::specta]
pub async fn combine_catalog_groups(
    app_handle: AppHandle,
    group_names: Vec<String>,
    target_name: String,
) -> Result<(), AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut conn = open_db(&app_handle)?;
        db::combine_groups(&mut conn, &group_names, &target_name)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Group combine task failed: {}", e)))?
}

#[tauri::command]
#[specta::specta]
pub async fn get_catalog_tags(app_handle: AppHandle) -> Result<Vec<TagCount>, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        Ok(db::list_tags_for_browse(
            &conn,
            crate::content_filter::is_unlocked(),
        )?
            .into_iter()
            .map(|(tag, count)| TagCount { tag, count })
            .collect())
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Tag task failed: {}", e)))?
}

#[tauri::command]
#[specta::specta]
pub async fn add_catalog_tag(
    app_handle: AppHandle,
    dir_path: String,
    tag: String,
) -> Result<(), AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        db::add_tag(&conn, &dir_path, &tag)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Tag task failed: {}", e)))?
}

#[tauri::command]
#[specta::specta]
pub async fn remove_catalog_tag(
    app_handle: AppHandle,
    dir_path: String,
    tag: String,
) -> Result<(), AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        db::remove_tag(&conn, &dir_path, &tag)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Tag task failed: {}", e)))?
}

#[tauri::command]
#[specta::specta]
pub async fn get_catalog_model_files(
    app_handle: AppHandle,
    dir_path: String,
    variant_key: Option<String>,
) -> Result<Vec<CatalogFile>, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        db::model_files(&conn, &dir_path, variant_key.as_deref())
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("File task failed: {}", e)))?
}

/// A model dir's mined per-file geometry (issue #15) — only files whose
/// content hash has been mined show up; run start_geometry_scan first to
/// populate file_geometry.
#[tauri::command]
#[specta::specta]
pub async fn get_model_geometry(
    app_handle: AppHandle,
    dir_path: String,
) -> Result<Vec<ModelFileGeometry>, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        db::model_geometry(&conn, &dir_path)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Geometry listing task failed: {}", e)))?
}

#[tauri::command]
#[specta::specta]
pub async fn get_catalog_stats(app_handle: AppHandle) -> Result<CatalogStats, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        db::stats(&conn)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Stats task failed: {}", e)))?
}

#[tauri::command]
#[specta::specta]
pub async fn get_duplicate_groups(app_handle: AppHandle) -> Result<Vec<DuplicateGroup>, AppError> {
    if !crate::content_filter::is_unlocked() {
        return Ok(Vec::new());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        db::duplicate_groups(&conn)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Duplicate task failed: {}", e)))?
}

#[tauri::command]
#[specta::specta]
pub async fn get_catalog_releases(app_handle: AppHandle) -> Result<Vec<ReleaseSummary>, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        db::list_releases_for_browse(&conn, crate::content_filter::is_unlocked())
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Release listing task failed: {}", e)))?
}

#[tauri::command]
#[specta::specta]
pub async fn get_catalog_designers(
    app_handle: AppHandle,
) -> Result<Vec<DesignerCount>, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        db::designers_for_browse(&conn, crate::content_filter::is_unlocked())
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Designer listing task failed: {}", e)))?
}

#[tauri::command]
#[specta::specta]
pub async fn rename_catalog_designer(
    app_handle: AppHandle,
    old_name: String,
    new_name: String,
) -> Result<u32, AppError> {
    let old_name = old_name.trim().to_string();
    let new_name = new_name.trim().to_string();
    if old_name.is_empty() || new_name.is_empty() {
        return Err(AppError::InvalidInput(
            "Designer names cannot be empty".into(),
        ));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let mut conn = open_db(&app_handle)?;
        db::rename_designer(&mut conn, &old_name, &new_name)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Designer rename task failed: {e}")))?
}

#[tauri::command]
#[specta::specta]
pub async fn rename_catalog_release(
    app_handle: AppHandle,
    designer: String,
    old_name: String,
    new_name: String,
) -> Result<u32, AppError> {
    let designer = designer.trim().to_string();
    let old_name = old_name.trim().to_string();
    let new_name = new_name.trim().to_string();
    if designer.is_empty() || old_name.is_empty() || new_name.is_empty() {
        return Err(AppError::InvalidInput(
            "Designer and release names cannot be empty".into(),
        ));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let mut conn = open_db(&app_handle)?;
        db::rename_release(&mut conn, &designer, &old_name, &new_name)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Release rename task failed: {e}")))?
}

/// Update one member's metadata, then propagate the shared facets (variant,
/// pose, scale) to its support twins — the supported/unsupported builds of
/// the same sculpt, matched by exact path structure. Returns how many twins
/// received the edit so the UI can say so. Only Some values propagate;
/// clears stay local to the edited member.
#[tauri::command]
#[specta::specta]
pub async fn update_model_metadata(
    app_handle: AppHandle,
    dir_path: String,
    meta: ModelMetaUpdate,
) -> Result<u32, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        // Input hygiene lives at the boundary, whoever the caller is:
        // stray leading/trailing whitespace must never reach the catalog
        // (a value of only spaces means "not set"), and variant casing is
        // the tool's convention (Title Case) so 'sword' and 'SWORD' can
        // never coexist.
        let mut meta = meta;
        meta.custom_name = tidy(meta.custom_name);
        meta.variant = tidy(meta.variant).map(|v| super::layout::title_case(&v));
        meta.pose = tidy(meta.pose);
        meta.scale = tidy(meta.scale);
        meta.support_status = tidy(meta.support_status);
        meta.release_date = tidy(meta.release_date);
        meta.designer = tidy(meta.designer);
        meta.sculptor = tidy(meta.sculptor);
        meta.release_name = tidy(meta.release_name);
        // canonical dimension strings: "25" or "60x35" (oval/rectangle),
        // unit implied — junk becomes "not set" rather than stored garbage
        meta.base_round_mm = meta.base_round_mm.and_then(|v| canonical_mm(&v));
        meta.base_square_mm = meta.base_square_mm.and_then(|v| canonical_mm(&v));
        db::update_model_user_meta(
            &conn,
            &dir_path,
            meta.custom_name,
            meta.pose.clone(),
            meta.scale.clone(),
            meta.support_status,
            meta.release_date.clone(),
            meta.designer.clone(),
            meta.sculptor.clone(),
            meta.release_name.clone(),
            meta.variant.clone(),
            meta.base_round_mm,
            meta.base_square_mm,
        )?;
        // designer/sculptor/release are facts about the MODEL — they apply
        // to every member of the group, not just the one being edited
        let mut touched = 0u32;
        if meta.designer.is_some()
            || meta.sculptor.is_some()
            || meta.release_name.is_some()
            || meta.release_date.is_some()
        {
            touched += db::propagate_group_meta(
                &conn,
                &dir_path,
                meta.designer.as_deref(),
                meta.sculptor.as_deref(),
                meta.release_name.as_deref(),
                meta.release_date.as_deref(),
            )?;
        }
        // the per-sculpt facets still sync only to the support twins
        if meta.variant.is_some() || meta.pose.is_some() || meta.scale.is_some() {
            let twins = db::support_twins(&conn, &dir_path)?;
            for twin in &twins {
                db::update_model_facets(
                    &conn,
                    twin,
                    meta.variant.as_deref(),
                    meta.pose.as_deref(),
                    meta.scale.as_deref(),
                )?;
            }
            touched = touched.max(twins.len() as u32);
        }
        Ok(touched)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Metadata update failed: {}", e)))?
}

/// Tag/untag every member of a group at once — a tag describes the mini,
/// not one support build of it.
#[tauri::command]
#[specta::specta]
pub async fn add_group_tag(
    app_handle: AppHandle,
    group_name: String,
    tag: String,
) -> Result<(), AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        db::add_group_tag(&conn, &group_name, &tag)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Tag task failed: {}", e)))?
}

#[tauri::command]
#[specta::specta]
pub async fn remove_group_tag(
    app_handle: AppHandle,
    group_name: String,
    tag: String,
) -> Result<(), AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        db::remove_group_tag(&conn, &group_name, &tag)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Tag task failed: {}", e)))?
}

/// Assign files to a pose (with optional per-file support) so a dump
/// folder can be split into pose members. Metadata only — nothing moves on
/// disk. Returns the number of known files assigned.
#[tauri::command]
#[specta::specta]
pub async fn assign_files_to_pose(
    app_handle: AppHandle,
    paths: Vec<String>,
    variant: Option<String>,
    pose: Option<String>,
    support_status: Option<String>,
) -> Result<u32, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut conn = open_db(&app_handle)?;
        // whitespace hygiene + casing convention at the boundary
        let variant = tidy(variant).map(|v| super::layout::title_case(&v));
        let pose = tidy(pose);
        let support_status = tidy(support_status);
        db::set_file_variants(&mut conn, &paths, variant, pose, support_status)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Assign task failed: {}", e)))?
}

/// "25", "60x35", "60 X 35", "60×35" -> canonical "25"/"60x35";
/// anything else (units, words, zeros) is "not set".
fn canonical_mm(value: &str) -> Option<String> {
    let cleaned = value.trim().to_lowercase().replace('×', "x");
    let parts: Vec<&str> = cleaned.split('x').map(str::trim).collect();
    let nums: Vec<u32> = parts
        .iter()
        .map(|p| p.parse::<u32>().ok().filter(|n| *n > 0))
        .collect::<Option<Vec<_>>>()?;
    match nums.as_slice() {
        [d] => Some(d.to_string()),
        [a, b] => Some(format!("{}x{}", a, b)),
        _ => None,
    }
}

/// Trim a user-entered optional value; whitespace-only means "not set".
fn tidy(value: Option<String>) -> Option<String> {
    value
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Revert files to plain members of their folder. Returns how many
/// assignments existed — 0 tells the UI the selection was never filed.
#[tauri::command]
#[specta::specta]
pub async fn clear_file_pose(app_handle: AppHandle, paths: Vec<String>) -> Result<u32, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        db::clear_file_variants(&conn, &paths)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Clear task failed: {}", e)))?
}

/// The pose assignments under a folder, for the split UI.
#[tauri::command]
#[specta::specta]
pub async fn get_file_variants(
    app_handle: AppHandle,
    dir_path: String,
) -> Result<Vec<FileVariant>, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        db::get_file_variants(&conn, &dir_path)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Assignment read task failed: {}", e)))?
}

/// Copy an image into the app's previews dir and point the model at it.
/// The copy (not a reference) is deliberate: render outputs and picked
/// images live wherever the user left them and may be cleaned up; the
/// catalog preview must not die with them. The filename is a stable
/// per-model hash plus a timestamp — a fresh URL each time, because the
/// webview caches aggressively by URL, with older copies swept first.
#[tauri::command]
#[specta::specta]
pub async fn set_model_preview(
    app_handle: AppHandle,
    dir_path: String,
    image_path: String,
    // A fanned-out member (one pose/variant of a dump folder) passes its
    // variant_key so the preview lands per-variant instead of clobbering the
    // whole folder. Whole-folder models pass null.
    variant_key: Option<String>,
) -> Result<String, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        persist_preview(&app_handle, &dir_path, variant_key.as_deref(), &image_path)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Preview task failed: {}", e)))?
}

/// The preview-persistence body, callable outside the command (the batch
/// render job stores one preview per finished model): copy the image into
/// app_data/previews under a per-member hash prefix, sweep older copies of
/// the same member, record the copy in the index.
pub(crate) fn persist_preview(
    app_handle: &AppHandle,
    dir_path: &str,
    variant_key: Option<&str>,
    image_path: &str,
) -> Result<String, AppError> {
    if !Path::new(image_path).is_file() {
        return Err(AppError::NotFoundError(format!(
            "Image not found: {}",
            image_path
        )));
    }
    let previews_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| AppError::ConfigError(format!("No app data dir: {}", e)))?
        .join("previews");
    std::fs::create_dir_all(&previews_dir)
        .map_err(|e| AppError::IoError(format!("Failed to create previews dir: {}", e)))?;

    // Hash the variant_key when present so each member's preview gets its
    // own on-disk file: sharing a dir_path-only prefix would make one
    // pose's render sweep away a sibling pose's still-referenced image.
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    variant_key.unwrap_or(dir_path).hash(&mut hasher);
    let prefix = format!("{:016x}", hasher.finish());

    if let Ok(entries) = std::fs::read_dir(&previews_dir) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&prefix) {
                std::fs::remove_file(entry.path()).ok();
            }
        }
    }

    let extension = Path::new(image_path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_else(|| "png".to_string());
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dest = previews_dir.join(format!("{}-{}.{}", prefix, stamp, extension));
    std::fs::copy(image_path, &dest)
        .map_err(|e| AppError::IoError(format!("Failed to copy preview: {}", e)))?;

    let dest_str = dest.to_string_lossy().into_owned();
    let conn = open_db(app_handle)?;
    db::set_preview(&conn, dir_path, variant_key, &dest_str)?;
    Ok(dest_str)
}

/// Persist the orientation a render used ("the render IS the chosen
/// orientation") — the studio calls this after a successful catalog-preview
/// render, and batch renders read it back so re-renders never need
/// repositioning.
#[tauri::command]
#[specta::specta]
pub async fn set_model_rotation(
    app_handle: AppHandle,
    dir_path: String,
    rotation: (f64, f64, f64),
) -> Result<(), AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        db::set_rotation(
            &conn,
            &dir_path,
            &format!("{},{},{}", rotation.0, rotation.1, rotation.2),
        )
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Rotation task failed: {}", e)))?
}

/// One member the batch renderer could target. `parts` are the member's
/// loose .stl paths; packed members surface with `packed: true` and no
/// extraction (batch is a throughput feature for loose libraries — packed
/// models render fine individually via the drawer).
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, specta::Type)]
pub struct RenderCandidate {
    pub dir_path: String,
    pub variant_key: Option<String>,
    pub name: String,
    pub parts: Vec<String>,
    /// Stored orientation "x,y,z", if the studio ever saved one.
    pub rotation: Option<String>,
    pub has_preview: bool,
    pub packed: bool,
}

/// Everything the batch confirm dialog needs, resolved through
/// group_members (NOT raw SQL: expand_file_variants is what gives fanned
/// members their own variant_key + per-variant preview state).
#[tauri::command]
#[specta::specta]
pub async fn get_render_candidates(
    app_handle: AppHandle,
    designer: Option<String>,
    groups: Vec<String>,
) -> Result<Vec<RenderCandidate>, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        let group_names = db::render_scope_groups(&conn, designer.as_deref(), &groups)?;
        let mut candidates = Vec::new();
        for group_name in group_names {
            // Data op, not a browse surface: pack/render scope must never
            // silently skip a hidden model.
            for member in db::group_members(&conn, &group_name, true)? {
                let files =
                    db::model_files(&conn, &member.dir_path, member.variant_key.as_deref())?;
                let parts: Vec<String> = files
                    .iter()
                    .filter(|f| f.extension == "stl" && !f.packed)
                    .map(|f| f.path.clone())
                    .collect();
                if parts.is_empty() && !member.packed {
                    continue; // nothing renderable (.obj/.3mf-only members)
                }
                candidates.push(RenderCandidate {
                    dir_path: member.dir_path.clone(),
                    variant_key: member.variant_key.clone(),
                    name: member.name.clone(),
                    parts,
                    rotation: member.rotation.clone(),
                    has_preview: member.preview_path.is_some(),
                    packed: member.packed,
                });
            }
        }
        Ok(candidates)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Candidate task failed: {}", e)))?
}

#[tauri::command]
#[specta::specta]
pub async fn delete_duplicate_files(
    app_handle: AppHandle,
    file_paths: Vec<String>,
) -> Result<BatchOutcome, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut removed: Vec<String> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        // A packed file has no loose bytes to delete — removing its index row
        // here would desync it from the archive, so refuse per path
        let packed = {
            let conn = open_db(&app_handle)?;
            db::archive_paths_for(&conn, &file_paths)?
        };
        for path in file_paths {
            if packed.contains_key(&path) {
                errors.push(format!(
                    "{}: packed (compressed at rest) — unpack the model first",
                    path
                ));
                continue;
            }
            match std::fs::remove_file(&path) {
                Ok(()) => removed.push(path),
                // Already gone from disk still means gone from the catalog
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => removed.push(path),
                Err(e) => errors.push(format!("{}: {}", path, e)),
            }
        }
        // Prune the index too, or the duplicate groups keep showing the
        // deleted copies until the next full rescan
        if !removed.is_empty() {
            let mut conn = open_db(&app_handle)?;
            db::remove_files(&mut conn, &removed)?;
        }
        Ok(BatchOutcome {
            succeeded: removed.len() as u32,
            errors,
        })
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("File deletion task failed: {}", e)))?
}

/// True when `child` is `parent` itself or filed anywhere under it.
/// Byte-prefix plus a separator check instead of Path::starts_with: the
/// index stores paths as the strings the scanner produced, and this must
/// match the DB's substr() scoping exactly — Path's component-wise rules
/// (case folding never, but verbatim prefixes and trailing separators yes)
/// would disagree with SQL at the edges.
fn path_within(child: &str, parent: &str) -> bool {
    child == parent
        || child
            .strip_prefix(parent)
            .is_some_and(|rest| rest.starts_with(std::path::MAIN_SEPARATOR))
}

/// Dedupe a dir list and drop entries nested under another entry — every
/// consumer is prefix-scoped, so a nested dir is already covered by its
/// ancestor and processing it separately would double-count or double-trash.
fn top_level_dirs(mut dirs: Vec<String>) -> Vec<String> {
    dirs.sort();
    dirs.dedup();
    dirs.iter()
        .filter(|d| {
            !dirs
                .iter()
                .any(|p| p.as_str() != d.as_str() && path_within(d, p))
        })
        .cloned()
        .collect()
}

/// Grow the doomed dirs into the largest folders that are safe to trash
/// wholesale. A card's members are leaf dirs (raw/, supported/), but they
/// live inside a shell folder holding the model.json and cover images the
/// index never rows — trashing only the leaves would leave husks the user
/// has to go clean up by hand, exactly the filesystem chore the catalog
/// exists to end. A parent is promoted only when (a) it sits strictly
/// inside a catalog root, (b) every model the INDEX knows under it is
/// being deleted anyway, (c) every subdirectory ON DISK is already part of
/// the delete — an unindexed stranger folder vetoes the whole promotion —
/// and (d) it isn't a release folder (release.json marks organizational
/// levels that outlive their members). Loose files in a promoted parent
/// (sidecars, covers) ride along into the trash, where they're still
/// recoverable.
fn consolidate_trash_units(
    conn: &Connection,
    doomed: &HashSet<String>,
    roots: &[String],
) -> Vec<String> {
    let mut units: HashSet<String> = doomed.clone();
    loop {
        let parents: HashSet<String> = units
            .iter()
            .filter_map(|d| Path::new(d).parent())
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let mut promoted = false;
        for parent in parents {
            if !roots
                .iter()
                .any(|r| parent.as_str() != r.as_str() && path_within(&parent, r))
            {
                continue;
            }
            let Ok(indexed) = db::model_dirs_under(conn, &parent) else {
                continue;
            };
            if indexed
                .iter()
                .any(|d| !doomed.iter().any(|x| path_within(d, x)))
            {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&parent) else {
                continue;
            };
            let mut safe = true;
            for entry in entries.flatten() {
                let entry_path = entry.path();
                if entry_path.is_dir() {
                    if !units.contains(&entry_path.to_string_lossy().into_owned()) {
                        safe = false;
                        break;
                    }
                } else if entry.file_name().to_string_lossy().eq_ignore_ascii_case("release.json") {
                    safe = false;
                    break;
                }
            }
            if !safe {
                continue;
            }
            units.retain(|u| !path_within(u, &parent));
            units.insert(parent);
            promoted = true;
        }
        if !promoted {
            break;
        }
    }
    units.into_iter().collect()
}

/// Delete the app-data preview copies persist_preview made for these hash
/// keys (dir_paths and variant_keys). Best-effort by design: a missed sweep
/// leaks one thumbnail file, never breaks catalog state.
fn sweep_preview_files(app_handle: &AppHandle, keys: &[String]) {
    let Ok(app_data) = app_handle.path().app_data_dir() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(app_data.join("previews")) else {
        return;
    };
    use std::hash::{Hash, Hasher};
    let prefixes: HashSet<String> = keys
        .iter()
        .map(|key| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            key.hash(&mut hasher);
            format!("{:016x}", hasher.finish())
        })
        .collect();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if prefixes.iter().any(|p| name.starts_with(p.as_str())) {
            std::fs::remove_file(entry.path()).ok();
        }
    }
}

/// What a pending model deletion covers, for the confirmation dialog:
/// counted from the index with the same prefix scoping the deletion uses,
/// so the dialog describes exactly what delete_models will take.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, specta::Type)]
pub struct DeleteSummary {
    pub dir_count: u32,
    pub file_count: u32,
    pub total_bytes: i64,
}

#[tauri::command]
#[specta::specta]
pub async fn summarize_model_dirs(
    app_handle: AppHandle,
    dir_paths: Vec<String>,
) -> Result<DeleteSummary, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let dirs = top_level_dirs(dir_paths);
        let conn = open_db(&app_handle)?;
        let (file_count, total_bytes) = db::dirs_summary(&conn, &dirs)?;
        Ok(DeleteSummary {
            dir_count: dirs.len() as u32,
            file_count,
            total_bytes,
        })
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Summary task failed: {}", e)))?
}

/// How a model deletion went. BatchOutcome plus one distinction the UI must
/// surface: hard_deleted counts folders that skipped the trash. The confirm
/// dialog promises recoverability, so when a volume couldn't deliver it the
/// user hears that it didn't — after the fact, but truthfully.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, specta::Type)]
pub struct DeleteOutcome {
    pub succeeded: u32,
    pub hard_deleted: u32,
    pub errors: Vec<String>,
}

/// Delete whole models: from the index always, from disk optionally. Disk
/// deletion tries the OS trash / Recycle Bin first so a wrong click stays
/// reversible, and falls back to a permanent delete where no trash exists
/// (network shares, most NAS mounts) — the confirmation dialog is the real
/// safeguard, and refusing to delete on trash-less volumes would strand
/// exactly the libraries this app targets. The index only forgets a model
/// whose disk delete succeeded (or that was already gone), so the catalog
/// keeps telling the truth about what's still on disk. Catalog-only
/// removal (delete_files = false) is a SOFT remove: the folders stay on
/// disk but go on the scan-ignore list, so a rescan doesn't quietly undo
/// the user's decision — Settings shows the list and can take them back.
#[tauri::command]
#[specta::specta]
pub async fn delete_models(
    app_handle: AppHandle,
    dir_paths: Vec<String>,
    delete_files: bool,
) -> Result<DeleteOutcome, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let dirs = top_level_dirs(dir_paths);
        if dirs.is_empty() {
            return Ok(DeleteOutcome {
                succeeded: 0,
                hard_deleted: 0,
                errors: Vec::new(),
            });
        }
        let mut errors: Vec<String> = Vec::new();
        let mut hard_deleted: u32 = 0;
        let removed_dirs: Vec<String> = if delete_files {
            let doomed: HashSet<String> = dirs.iter().cloned().collect();
            let units = {
                let conn = open_db(&app_handle)?;
                let roots = db::known_roots(&conn)?;
                consolidate_trash_units(&conn, &doomed, &roots)
            };
            let mut removed = Vec::new();
            for unit in units {
                let covered: Vec<String> = dirs
                    .iter()
                    .filter(|d| path_within(d, &unit))
                    .cloned()
                    .collect();
                if !Path::new(&unit).exists() {
                    // Already gone from disk still means gone from the catalog
                    removed.extend(covered);
                    continue;
                }
                match trash::delete(&unit) {
                    Ok(()) => removed.extend(covered),
                    // Trash refused — permission problems and no-trash volumes
                    // land here alike. remove_dir_all sorts them out: a
                    // permission error fails again and surfaces; a trash-less
                    // volume deletes for real, counted so the UI can say so.
                    Err(trash_err) => match std::fs::remove_dir_all(&unit) {
                        Ok(()) => {
                            hard_deleted += covered.len() as u32;
                            removed.extend(covered);
                        }
                        Err(rm_err) => errors.push(format!(
                            "{}: couldn't move to trash ({}) or delete ({})",
                            unit, trash_err, rm_err
                        )),
                    },
                }
            }
            removed
        } else {
            dirs
        };
        if !removed_dirs.is_empty() {
            let mut conn = open_db(&app_handle)?;
            // Sweep keys must be read BEFORE remove_models: the variant_keys
            // live in variant_previews, which remove_models deletes
            let sweep_keys = db::preview_sweep_keys(&conn, &removed_dirs)?;
            db::remove_models(&mut conn, &removed_dirs)?;
            if delete_files {
                // A folder gone from disk needs no ignore marker — and a
                // stale one would invisibly block the path if it's ever
                // legitimately recreated
                db::remove_scan_ignores_under(&conn, &removed_dirs)?;
            } else {
                db::add_scan_ignores(&conn, &removed_dirs)?;
            }
            drop(conn);
            sweep_preview_files(&app_handle, &sweep_keys);
        }
        Ok(DeleteOutcome {
            succeeded: removed_dirs.len() as u32,
            hard_deleted,
            errors,
        })
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Model deletion task failed: {}", e)))?
}

/// One soft-removed folder, for the Settings list.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, specta::Type)]
pub struct IgnoredFolder {
    pub dir_path: String,
    pub ignored_at: i64,
}

#[tauri::command]
#[specta::specta]
pub async fn list_ignored_folders(app_handle: AppHandle) -> Result<Vec<IgnoredFolder>, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        Ok(db::list_scan_ignores(&conn)?
            .into_iter()
            .map(|(dir_path, ignored_at)| IgnoredFolder {
                dir_path,
                ignored_at,
            })
            .collect())
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Ignore-list task failed: {}", e)))?
}

/// Take a folder off the soft-remove list. The models come back on the
/// next scan of their root — the marker was the only thing hiding them.
#[tauri::command]
#[specta::specta]
pub async fn unignore_folder(app_handle: AppHandle, dir_path: String) -> Result<(), AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        db::remove_scan_ignores_under(&conn, &[dir_path])
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Ignore-list task failed: {}", e)))?
}

/// Flag or unflag every model behind one or more card names — the drawer's
/// "mark 18+" button and the batch bar's equivalent both land here. Resolves
/// through group_members with include_nsfw=true so an ALREADY-hidden member
/// still gets updated (unmarking a mixed group must reach every variant, not
/// just the ones currently visible).
#[tauri::command]
#[specta::specta]
pub async fn set_group_nsfw(
    app_handle: AppHandle,
    group_names: Vec<String>,
    nsfw: bool,
) -> Result<(), AppError> {
    if !nsfw && !crate::content_filter::is_unlocked() {
        return Err(AppError::InvalidInput(
            "Unlock mature content before removing an 18+ mark".into(),
        ));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        let mut dirs: HashSet<String> = HashSet::new();
        for group_name in &group_names {
            for member in db::group_members(&conn, group_name, true)? {
                dirs.insert(member.dir_path);
            }
        }
        db::set_models_nsfw(
            &conn,
            &dirs.into_iter().collect::<Vec<_>>(),
            Some(nsfw),
        )
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("18+ flag task failed: {}", e)))?
}

#[tauri::command]
#[specta::specta]
pub async fn list_nsfw_designers(app_handle: AppHandle) -> Result<Vec<String>, AppError> {
    if !crate::content_filter::is_unlocked() {
        return Err(AppError::InvalidInput(
            "Unlock mature content to manage its designer list".into(),
        ));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        db::list_nsfw_designers(&conn)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("18+ designer task failed: {}", e)))?
}

#[tauri::command]
#[specta::specta]
pub async fn set_designer_nsfw(
    app_handle: AppHandle,
    designer: String,
    nsfw: bool,
) -> Result<(), AppError> {
    if !nsfw && !crate::content_filter::is_unlocked() {
        return Err(AppError::InvalidInput(
            "Unlock mature content before removing a designer rule".into(),
        ));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        db::set_designer_nsfw(&conn, &designer, nsfw)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("18+ designer task failed: {}", e)))?
}

/// Merge a duplicate group: every path in `duplicate_paths` becomes another
/// name for `keep_path`'s bytes (a hardlink), freeing the copies while every
/// variant keeps a working file. The catalog's identities are updated in
/// place so the group reports "shared" without waiting for a rescan.
#[tauri::command]
#[specta::specta]
pub async fn merge_duplicate_files(
    app_handle: AppHandle,
    keep_path: String,
    duplicate_paths: Vec<String>,
) -> Result<BatchOutcome, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let keep = PathBuf::from(&keep_path);
        // Hardlink merging needs both inodes on disk; a packed side has
        // neither. Refuse packed duplicates per path (the rest still merge),
        // and refuse outright when the keeper itself is packed.
        let (duplicate_paths, mut packed_errors) = {
            let conn = open_db(&app_handle)?;
            let mut check = duplicate_paths.clone();
            check.push(keep_path.clone());
            let packed = db::archive_paths_for(&conn, &check)?;
            if packed.contains_key(&keep_path) {
                return Err(AppError::InvalidInput(
                    "The file to keep is packed (compressed at rest) — unpack the model first"
                        .to_string(),
                ));
            }
            let (packed_dups, loose): (Vec<String>, Vec<String>) = duplicate_paths
                .into_iter()
                .partition(|p| packed.contains_key(p));
            let errors: Vec<String> = packed_dups
                .into_iter()
                .map(|p| format!("{}: packed (compressed at rest) — unpack the model first", p))
                .collect();
            (loose, errors)
        };
        let (merged, mut errors) = dups::merge_duplicates(&keep, &duplicate_paths)?;
        errors.append(&mut packed_errors);
        if !merged.is_empty() {
            if let Some(identity) = dups::file_identity(&keep) {
                // Fresh mtimes ride along: the merged paths now carry the
                // keeper's timestamp, and a stale one in the index would make
                // the next rescan drop their hashes as "changed files"
                let entries: Vec<(String, String, i64)> = merged
                    .iter()
                    .chain(std::iter::once(&keep_path))
                    .map(|p| {
                        let modified_at = std::fs::metadata(p)
                            .and_then(|m| m.modified())
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs() as i64)
                            .unwrap_or(0);
                        (p.clone(), identity.clone(), modified_at)
                    })
                    .collect();
                let conn = open_db(&app_handle)?;
                db::store_merge_results(&conn, &entries)?;
            }
        }
        Ok(BatchOutcome {
            succeeded: merged.len() as u32,
            errors,
        })
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Merge task failed: {}", e)))?
}

/// Probe whether the volume holding `path` supports hardlink merging.
/// Consulted by the duplicates panel so link-less filesystems (exFAT, some
/// NAS mounts) get delete-only instead of a button that can't work.
#[tauri::command]
#[specta::specta]
pub async fn supports_file_links(path: String) -> Result<bool, AppError> {
    tauri::async_runtime::spawn_blocking(move || Ok(dups::supports_links(Path::new(&path))))
        .await
        .map_err(|e| AppError::ConfigError(format!("Probe task failed: {}", e)))?
}

/// Dry-run the normalizer: what would move where to make the disk match
/// the curated catalog. Read-only — nothing happens until apply.
#[tauri::command]
#[specta::specta]
pub async fn plan_normalize(
    app_handle: AppHandle,
    designer: Option<String>,
    group: Option<String>,
) -> Result<NormalizePlan, AppError> {
    // The UI no longer nominates a root: each group's home folder is
    // resolved from its members against the configured list — unless a
    // primary is set, which stages every group into that one folder.
    let settings = crate::settings::get_settings(app_handle.clone())
        .await
        .map_err(AppError::ConfigError)?;
    let root_strs = normalized_roots(&settings);
    let primary = valid_primary(&settings, &root_strs).map(PathBuf::from);
    let roots: Vec<PathBuf> = root_strs.into_iter().map(PathBuf::from).collect();
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        normalize::plan(
            &conn,
            &roots,
            primary.as_deref(),
            designer.as_deref(),
            group.as_deref(),
        )
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Normalize plan task failed: {}", e)))?
}

/// Execute approved normalizer moves. The frontend sends these in chunks
/// so a big NAS batch shows progress and stays cancellable between calls.
#[tauri::command]
#[specta::specta]
pub async fn apply_normalize(
    app_handle: AppHandle,
    ops: Vec<NormalizeOp>,
) -> Result<BatchOutcome, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut conn = open_db(&app_handle)?;
        normalize::apply_ops(&mut conn, &ops)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Normalize apply task failed: {}", e)))?
}

/// After the moves: write authoritative model.json sidecars, sweep emptied
/// dirs, rebuild search. Returns human-readable warnings.
#[tauri::command]
#[specta::specta]
pub async fn finalize_normalize(
    app_handle: AppHandle,
    group_names: Vec<String>,
    old_dirs: Vec<String>,
) -> Result<Vec<String>, AppError> {
    let roots = configured_roots(&app_handle).await?;
    tauri::async_runtime::spawn_blocking(move || {
        let conn = open_db(&app_handle)?;
        normalize::finalize(&conn, &roots, &group_names, &old_dirs)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Normalize finalize task failed: {}", e)))?
}

#[tauri::command]
#[specta::specta]
pub async fn batch_move_models(
    app_handle: AppHandle,
    operations: Vec<MoveOperation>,
) -> Result<BatchOutcome, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut conn = open_db(&app_handle)?;
        let mut succeeded = 0u32;
        let mut errors: Vec<String> = Vec::new();

        for op in operations {
            let from_path = PathBuf::from(&op.from);
            let to_path = PathBuf::from(&op.to);

            if !from_path.exists() {
                errors.push(format!("Source not found: {}", op.from));
                continue;
            }
            // move_model's index re-keying doesn't rewrite archive_path or
            // packs rows, so a packed model would end up pointing at the old
            // location — refuse until it's unpacked
            if db::dir_contains_pack(&conn, &op.from)? {
                errors.push(format!(
                    "{}: packed (compressed at rest) — unpack the model before moving it",
                    op.from
                ));
                continue;
            }
            // rename() onto an existing path is platform-dependent (may
            // clobber a file, may fail on a dir) — refuse up front instead
            if to_path.exists() {
                errors.push(format!("Destination already exists: {}", op.to));
                continue;
            }
            if let Some(parent) = to_path.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    errors.push(format!("Failed to create parent dirs for {}: {}", op.to, e));
                    continue;
                }
            }
            if let Err(e) = std::fs::rename(&from_path, &to_path) {
                errors.push(format!("Failed to move {} to {}: {}", op.from, op.to, e));
                continue;
            }
            // Disk and index must move together: a stale dir_path drops the
            // model's user tags on the next rescan (see db::move_model)
            match db::move_model(&mut conn, &op.from, &op.to) {
                Ok(()) => succeeded += 1,
                Err(e) => errors.push(format!(
                    "Moved {} on disk but failed to update the catalog (rescan to fix): {}",
                    op.to, e
                )),
            }
        }

        Ok(BatchOutcome { succeeded, errors })
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Batch move task failed: {}", e)))?
}

#[cfg(test)]
mod tests {
    use super::{consolidate_trash_units, db, path_within, top_level_dirs, Connection, HashSet};

    #[test]
    fn path_within_respects_component_boundaries() {
        assert!(path_within("/lib/newt", "/lib/newt"));
        assert!(path_within("/lib/newt/supported", "/lib/newt"));
        // A sibling sharing a name prefix is NOT inside: this is what keeps
        // deleting "newt" from also forgetting "newton"
        assert!(!path_within("/lib/newton", "/lib/newt"));
        assert!(!path_within("/lib", "/lib/newt"));
    }

    #[test]
    fn top_level_dirs_drops_covered_children() {
        let dirs = vec![
            "/lib/newt/supported".to_string(),
            "/lib/newt".to_string(),
            "/lib/newt".to_string(),
            "/lib/newton".to_string(),
        ];
        let top = top_level_dirs(dirs);
        assert_eq!(top, vec!["/lib/newt".to_string(), "/lib/newton".to_string()]);
    }

    #[test]
    fn consolidation_climbs_shells_but_stops_at_strangers_and_roots() {
        let base = std::env::temp_dir().join(format!("plinth_del_{}", std::process::id()));
        let root = base.join("library");
        let s = |p: &std::path::Path| p.to_string_lossy().into_owned();

        // designer1/newt/{raw,supported} + shell sidecars: the whole model
        // folder — and then the emptied designer folder — should be one unit
        let newt = root.join("designer1").join("newt");
        // designer2/troll/{raw, wip-sculpts}: an unindexed stranger dir
        // vetoes climbing, so only the doomed leaf is trashed
        let troll = root.join("designer2").join("troll");
        for d in [newt.join("raw"), newt.join("supported"), troll.join("raw"), troll.join("wip-sculpts")] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(newt.join("model.json"), "{}").unwrap();
        std::fs::write(newt.join("cover.jpg"), "x").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        db::test_init(&conn);
        let root_s = s(&root);
        for dir in [newt.join("raw"), newt.join("supported"), troll.join("raw")] {
            conn.execute(
                "INSERT INTO models (dir_path, name, root, indexed_at) VALUES (?1, 'x', ?2, 0)",
                rusqlite::params![s(&dir), root_s],
            )
            .unwrap();
        }

        let doomed: HashSet<String> =
            [s(&newt.join("raw")), s(&newt.join("supported")), s(&troll.join("raw"))]
                .into_iter()
                .collect();
        let mut units = consolidate_trash_units(&conn, &doomed, std::slice::from_ref(&root_s));
        units.sort();
        assert_eq!(
            units,
            vec![s(&root.join("designer1")), s(&troll.join("raw"))],
            "newt's shell and emptied designer folder fold into one unit; \
             troll's stranger subfolder pins deletion to the doomed leaf"
        );

        // Deleting the LAST model of a catalog: the climb eats every emptied
        // shell but the root itself is the hard ceiling
        let root2 = base.join("solo-library");
        let raw2 = root2.join("designer3").join("wisp").join("raw");
        std::fs::create_dir_all(&raw2).unwrap();
        conn.execute(
            "INSERT INTO models (dir_path, name, root, indexed_at) VALUES (?1, 'x', ?2, 0)",
            rusqlite::params![s(&raw2), s(&root2)],
        )
        .unwrap();
        let doomed2: HashSet<String> = [s(&raw2)].into_iter().collect();
        let units = consolidate_trash_units(&conn, &doomed2, &[s(&root2)]);
        assert_eq!(units, vec![s(&root2.join("designer3"))]);

        std::fs::remove_dir_all(&base).ok();
    }
}
