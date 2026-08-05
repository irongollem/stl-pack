use super::storage;
use super::writer;
use crate::error::AppError;
use crate::file::compression_jobs;
use crate::file::utils::clean_name;
use crate::models::events::CancelledStatus;
use crate::models::events::CompletedStatus;
use crate::models::events::CompressionStatus;
use crate::models::events::FailedStatus;
use crate::models::{ModelLocation, ModelReference, Release, ReleaseDraftSummary, StlModel};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use uuid::Uuid;

static ACTIVE_COMPRESSIONS: Lazy<Mutex<HashMap<String, Arc<AtomicBool>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Stage the whole draft into the release directory in CANONICAL layout
/// (Model/Supported|Unsupported[/Variant], sidecar per leaf — see
/// file/stage.rs), then record every leaf in release.json in one write.
/// One command for the whole batch on purpose: members sharing a canonical
/// leaf must merge (poses become file-level metadata), and the old
/// per-model command raced its own release.json read-modify-write.
#[tauri::command]
#[specta::specta]
pub async fn add_models(
    models: Vec<StlModel>,
    release_dir: String,
) -> Result<Vec<(StlModel, String)>, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let release_path = PathBuf::from(&release_dir);
        let release_json_path = release_path.join("release.json");
        let mut release: Release = serde_json::from_str(
            &fs::read_to_string(&release_json_path)
                .map_err(|e| AppError::NotFoundError(format!("No release.json: {}", e)))?,
        )?;

        let staged = super::stage::stage_models(&release_path, &release, &models)?;

        for (model, sidecar_rel) in &staged {
            let model_id = model.id.ok_or_else(|| {
                AppError::ConfigError("Staged model is missing its id".to_string())
            })?;
            release.model_references.push(ModelReference {
                id: model_id,
                location: ModelLocation::Local(sidecar_rel.clone()),
            });
            if let Some(group) = &model.group {
                if !release.groups.contains(group) {
                    release.groups.push(group.clone());
                }
            }
        }
        writer::write_json(serde_json::to_string_pretty(&release)?, release_json_path)?;

        Ok(staged)
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Staging task failed: {}", e)))?
}

/// WIP releases that never got packed — a successful finalize removes the
/// scratch folder (compression_jobs::perform_compression), so any directory
/// still holding a release.json here is by definition unfinished.
#[tauri::command]
#[specta::specta]
pub async fn list_release_drafts(
    app_handle: AppHandle,
) -> Result<Vec<ReleaseDraftSummary>, AppError> {
    let scratch_root = storage::get_scratch_path(&app_handle)?;
    let entries = match fs::read_dir(&scratch_root) {
        Ok(entries) => entries,
        // Nothing has ever been staged there yet — not an error
        Err(_) => return Ok(Vec::new()),
    };

    let mut drafts: Vec<ReleaseDraftSummary> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| {
            let release_json = fs::read_to_string(entry.path().join("release.json")).ok()?;
            let release: Release = serde_json::from_str(&release_json).ok()?;
            Some(ReleaseDraftSummary {
                release_dir: entry.path().to_string_lossy().into_owned(),
                name: release.name,
                designer: release.designer,
                model_count: release.model_references.len() as u32,
            })
        })
        .collect();
    drafts.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(drafts)
}

/// Rehydrate a WIP release for the builder UI: release.json only carries
/// {id, path} per model (ModelReference), so the rich curation the UI needs
/// (designer/pose/variant/tags/file_poses…) is read back from each model's
/// own model.json sidecar — the same file add_models staged it to.
#[tauri::command]
#[specta::specta]
pub async fn load_release_draft(release_dir: String) -> Result<(Release, Vec<StlModel>), AppError> {
    let release_path = PathBuf::from(&release_dir);
    let release_json = fs::read_to_string(release_path.join("release.json")).map_err(|e| {
        AppError::NotFoundError(format!(
            "No release.json in '{}': {}",
            release_path.display(),
            e
        ))
    })?;
    let release: Release = serde_json::from_str(&release_json)?;

    let mut models = Vec::with_capacity(release.model_references.len());
    for reference in &release.model_references {
        let ModelLocation::Local(relative_path) = &reference.location else {
            // Nothing on this disk to read back for an external reference
            continue;
        };
        let model_json_path = release_path.join(relative_path);
        let read = fs::read_to_string(&model_json_path)
            .map_err(|e| e.to_string())
            .and_then(|json| serde_json::from_str::<StlModel>(&json).map_err(|e| e.to_string()));
        match read {
            Ok(model) => models.push(model),
            // One missing/corrupt sidecar (deleted by hand, interrupted
            // write) must not block resuming everything else in the draft
            Err(e) => eprintln!(
                "[load_release_draft] skipping '{}': {}",
                model_json_path.display(),
                e
            ),
        }
    }

    Ok((release, models))
}

#[tauri::command]
#[specta::specta]
pub async fn create_release(
    app_handle: AppHandle,
    release: Release,
    image_paths: Vec<String>,
    other_file_paths: Vec<String>,
    licence_path: Option<String>,
) -> Result<String, AppError> {
    let release_name = clean_name(&release.name);
    let designer_name = clean_name(&release.designer);

    // The directory name doubles as the catalog's on-disk key; refuse a
    // malformed date instead of silently stamping a wrong one
    let release_date = {
        let invalid = || {
            AppError::InvalidInput(format!(
                "Invalid release date '{}': expected MM/YYYY",
                release.date
            ))
        };
        let date_parts = release.date.split('/').collect::<Vec<_>>();
        match date_parts.as_slice() {
            [month, year] => {
                let month: u8 = month.trim().parse().map_err(|_| invalid())?;
                let year: u16 = year.trim().parse().map_err(|_| invalid())?;
                if !(1..=12).contains(&month) {
                    return Err(invalid());
                }
                format!("{:02}-{}", month, year)
            }
            _ => return Err(invalid()),
        }
    };

    let release_dir_name = format!("{}-{}-{}", designer_name, release_date, release_name);
    let release_path = storage::create_dir_on_scratch(&app_handle, release_dir_name.clone())?;

    let copied_images = storage::copy_images(&image_paths, &release_path, &release_name)?;
    let mut copied_files = storage::copy_files(&other_file_paths, &release_path)?;
    // The creator's licence lands as canonical licence.<ext> so it rides
    // inside release.3pk
    if let Some(licence) = licence_path {
        copied_files.push(storage::copy_licence(&licence, &release_path)?);
    }

    let relative_image_paths = storage::convert_to_relative_paths(&copied_images, &release_path)?;
    let relative_file_paths = storage::convert_to_relative_paths(&copied_files, &release_path)?;

    // The backend owns the directory name — persisting a frontend-computed
    // copy invites the two to drift apart
    let release_with_paths = Release {
        images: relative_image_paths,
        other_files: relative_file_paths,
        release_dir: release_dir_name,
        ..release
    };

    let release_json = serde_json::to_string_pretty(&release_with_paths)?;

    fs::write(release_path.join("release.json"), release_json)?;

    if let Some(window) = app_handle.get_webview_window("main") {
        window.set_title(&format!(
            "Plinth - Creating release: {}",
            release_with_paths.name
        ))?;
    }

    Ok(release_path.to_string_lossy().into_owned())
}

#[tauri::command]
#[specta::specta]
pub async fn finalize_release(
    app_handle: AppHandle,
    release_dir: String,
) -> Result<String, AppError> {
    let release_dir_path = PathBuf::from(&release_dir);

    if !release_dir_path.exists() {
        return Err(AppError::NotFoundError(format!(
            "Release directory '{}' not found",
            release_dir_path.display()
        )));
    }

    let job_id = Uuid::new_v4().to_string();
    let cancel_token = Arc::new(AtomicBool::new(false));
    {
        let mut compressions = ACTIVE_COMPRESSIONS.lock().map_err(|e| {
            AppError::ConfigError(format!("Failed to access compressions registry: {}", e))
        })?;
        compressions.insert(job_id.clone(), Arc::clone(&cancel_token));
    }

    let app_handle_clone = app_handle.clone();
    let job_id_clone = job_id.clone();
    tokio::spawn(async move {
        let start_time = std::time::Instant::now();

        let result = compression_jobs::perform_compression(
            app_handle_clone.clone(),
            job_id_clone.clone(),
            release_dir_path.clone(),
            cancel_token,
        )
        .await;

        if let Ok(mut compressions) = ACTIVE_COMPRESSIONS.lock() {
            compressions.remove(&job_id_clone);
        }

        match result {
            Ok((files, size, target_dir)) => {
                let elapsed = start_time.elapsed().as_secs_f64();
                CompressionStatus::Completed(CompletedStatus {
                    job_id: job_id_clone,
                    total_files: files,
                    total_size_kb: size,
                    elapsed_seconds: elapsed,
                    folder_path: target_dir.to_string_lossy().into_owned(),
                })
                .emit(&app_handle_clone)
                .ok();
            }
            // Match the variant, not the message text: error strings can
            // legitimately contain the word "cancelled" (e.g. in a path)
            Err(AppError::UserCancelled(_)) => {
                CompressionStatus::Cancelled(CancelledStatus {
                    job_id: job_id_clone,
                })
                .emit(&app_handle_clone)
                .ok();
            }
            Err(e) => {
                CompressionStatus::Failed(FailedStatus {
                    job_id: job_id_clone,
                    error: e.to_string(),
                })
                .emit(&app_handle_clone)
                .ok();
            }
        }
    });

    Ok(job_id)
}

/// Import a packed release into the library: verify component checksums,
/// extract (rematerializing dedup-elided names), land it under the same
/// naming scheme create_release uses. A catalog scan afterwards restores
/// the packed curation. `components` limits the run to the named components
/// (selective import / update); None imports everything.
#[tauri::command]
#[specta::specta]
pub async fn import_release(
    package_path: String,
    library_dir: String,
    components: Option<Vec<String>>,
) -> Result<super::import::ImportOutcome, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        super::import::import_release(
            std::path::Path::new(&package_path),
            std::path::Path::new(&library_dir),
            components,
        )
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Import task failed: {}", e)))?
}

/// "Import what you own": for each named component, extract the sibling
/// archive the normal way when it checks out, otherwise materialize
/// whatever this library already holds by checksum — including a component
/// whose archive isn't present next to the .3pk at all. A partial result
/// still keeps normal catalog rows for what landed; nothing is invented for
/// what didn't, and re-running after the rest turns up completes it.
#[tauri::command]
#[specta::specta]
pub async fn recompile_release_from_library(
    app_handle: AppHandle,
    package_path: String,
    library_dir: String,
    components: Option<Vec<String>>,
) -> Result<super::import::RecompileOutcome, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = crate::catalog::commands::open_db(&app_handle)?;
        super::import::recompile_release(
            &conn,
            std::path::Path::new(&package_path),
            std::path::Path::new(&library_dir),
            components,
        )
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Recompile task failed: {}", e)))?
}

/// Diff a `release.3pk` against the library without touching anything: per
/// component, is it new, changed, unchanged, packed at rest, or missing its
/// archive — and, regardless of that state, how much of it this library
/// already owns SOMEWHERE by checksum. Feeds the selective-import dialog
/// shown before an import runs.
#[tauri::command]
#[specta::specta]
pub async fn inspect_release_package(
    app_handle: AppHandle,
    package_path: String,
    library_dir: String,
) -> Result<super::import::PackageInspection, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        let conn = crate::catalog::commands::open_db(&app_handle)?;
        super::import::inspect_package(
            std::path::Path::new(&package_path),
            std::path::Path::new(&library_dir),
            &conn,
        )
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Inspect task failed: {}", e)))?
}

/// Open files with their OS-default application — the "print" hand-off to
/// whatever slicer owns the extension. NOT the opener plugin: its open_path
/// is fire-and-forget (open::that_detached), which reports success even
/// when the OS has no app for the file type, leaving the user with a button
/// that silently does nothing. Launching the OS opener ourselves captures
/// that failure so the UI can react (fall back to revealing the folder).
#[tauri::command]
#[specta::specta]
pub async fn open_with_default_app(paths: Vec<String>) -> Result<(), AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        if paths.is_empty() {
            return Err(AppError::InvalidInput("Nothing to open".into()));
        }
        #[cfg(target_os = "macos")]
        {
            // one `open` call: multiple files reach the slicer as one batch
            let output = crate::process::new_command("open")
                .args(&paths)
                .output()
                .map_err(|e| AppError::IoError(format!("Failed to run open: {}", e)))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(AppError::ConfigError(format!(
                    "No app is set up to open these files: {}",
                    stderr.trim()
                )));
            }
        }
        #[cfg(target_os = "windows")]
        for path in &paths {
            // without CREATE_NO_WINDOW this cmd shim would flash a console
            // window on every PRINT click
            let status = crate::process::new_command("cmd")
                .args(["/C", "start", "", path])
                .status()
                .map_err(|e| AppError::IoError(format!("Failed to run start: {}", e)))?;
            if !status.success() {
                return Err(AppError::ConfigError(format!(
                    "No app is set up to open {}",
                    path
                )));
            }
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        for path in &paths {
            let status = crate::process::new_command("xdg-open")
                .arg(path)
                .status()
                .map_err(|e| AppError::IoError(format!("Failed to run xdg-open: {}", e)))?;
            if !status.success() {
                return Err(AppError::ConfigError(format!(
                    "No app is set up to open {}",
                    path
                )));
            }
        }
        Ok(())
    })
    .await
    .map_err(|e| AppError::ConfigError(format!("Open task failed: {}", e)))?
}

#[tauri::command]
#[specta::specta]
pub async fn cancel_compression(job_id: String) -> Result<(), AppError> {
    let compressions = ACTIVE_COMPRESSIONS.lock().map_err(|e| {
        AppError::ConfigError(format!("Failed to access compressions registry: {}", e))
    })?;

    if let Some(token) = compressions.get(&job_id) {
        token.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    } else {
        Err(AppError::NotFoundError(format!(
            "No active compression job with ID: {}",
            job_id
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ModelReference;

    fn bare_model(name: &str) -> StlModel {
        StlModel {
            id: Some(Uuid::new_v4()),
            name: name.to_string(),
            description: None,
            tags: vec![],
            images: vec![],
            model_files: vec![],
            group: None,
            variant: None,
            pose: Some("standing".to_string()),
            scale: None,
            support_status: None,
            release_date: None,
            designer: None,
            sculptor: None,
            release_name: None,
            base_round_mm: None,
            base_square_mm: None,
            file_poses: vec![],
        }
    }

    /// A WIP release folder as `create_release` + `add_models` actually leave
    /// it: release.json referencing per-model sidecars by relative path.
    /// load_release_draft must rebuild the full StlModels from those
    /// sidecars, and skip (not fail on) one that went missing.
    #[tokio::test]
    async fn load_release_draft_rehydrates_models_and_skips_missing_sidecar() {
        let dir = std::env::temp_dir().join(format!(
            "stlpack_load_draft_{}_{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(dir.join("knight")).unwrap();

        let present = bare_model("knight");
        fs::write(
            dir.join("knight/model.json"),
            serde_json::to_string(&present).unwrap(),
        )
        .unwrap();

        let missing_id = Uuid::new_v4();
        let release = Release {
            name: "Test Release".to_string(),
            designer: "Some Designer".to_string(),
            description: String::new(),
            date: "01-2026".to_string(),
            version: "1.0.0".to_string(),
            model_references: vec![
                ModelReference {
                    id: present.id.unwrap(),
                    location: ModelLocation::Local("knight/model.json".to_string()),
                },
                ModelReference {
                    id: missing_id,
                    location: ModelLocation::Local("ghost/model.json".to_string()),
                },
            ],
            groups: vec![],
            release_dir: dir.to_string_lossy().into_owned(),
            images: vec![],
            other_files: vec![],
        };
        fs::write(
            dir.join("release.json"),
            serde_json::to_string(&release).unwrap(),
        )
        .unwrap();

        let (loaded_release, models) =
            load_release_draft(dir.to_string_lossy().into_owned())
                .await
                .unwrap();

        assert_eq!(loaded_release.name, "Test Release");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, "knight");
        assert_eq!(models[0].pose.as_deref(), Some("standing"));

        fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn load_release_draft_errors_when_release_json_is_absent() {
        let dir = std::env::temp_dir().join(format!(
            "stlpack_load_draft_missing_{}_{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();

        let result = load_release_draft(dir.to_string_lossy().into_owned()).await;
        assert!(result.is_err());

        fs::remove_dir_all(&dir).ok();
    }
}
