use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, Arc, Mutex},
    time::{Duration, Instant},
};

use tauri::AppHandle;
use tauri_specta::Event;
use tokio::sync::Semaphore;

use crate::{
    error::AppError,
    models::events::{CompressionStatus, ProgressStatus, StartedStatus},
    settings::get_optimal_thread_count,
};

use super::{
    compressors,
    storage::{self, collect_files_for_compression},
    utils::calculate_total_size,
};

/// Minimum time between Progress emissions, so a 100k-file release doesn't
/// flood the webview with one IPC event per file.
const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(200);

/// Integer percentage with u64 math: total can be 0 (a release of only
/// sub-1KiB files truncates to 0 KiB) and `part * 100` overflows u32 past
/// ~41 GiB. Capped at 100: manifest.json is generated mid-job, after the
/// totals were counted, so the processed side can legitimately run one
/// small file past the estimate.
fn percent(part: u32, total: u32) -> u32 {
    if total == 0 {
        100
    } else {
        (((part as u64 * 100) / total as u64) as u32).min(100)
    }
}

struct ProgressTracker {
    app_handle: AppHandle,
    job_id: String,
    total_files: u32,
    total_size_kb: u32,
    processed_files: u32,
    processed_size_kb: u32,
    last_percent_size: u32,
    last_percent_files: u32,
    last_emit: Instant,
}

impl ProgressTracker {
    fn new(app_handle: AppHandle, job_id: String, total_files: u32, total_size_kb: u32) -> Self {
        Self {
            app_handle,
            job_id,
            total_files,
            total_size_kb,
            processed_files: 0,
            processed_size_kb: 0,
            last_percent_size: u32::MAX,
            last_percent_files: u32::MAX,
            last_emit: Instant::now(),
        }
    }

    /// Record one processed file and emit Progress when something visible
    /// changed. Callers hold the tracker lock only for this call — never
    /// across compression work, which would serialize all workers.
    fn record_file(&mut self, file_size_kb: u32, current_file: &str) {
        self.processed_size_kb = self.processed_size_kb.saturating_add(file_size_kb);
        self.processed_files = self.processed_files.saturating_add(1);

        let percent_size = percent(self.processed_size_kb, self.total_size_kb);
        let percent_files = percent(self.processed_files, self.total_files);
        let now = Instant::now();
        let finished = self.processed_files >= self.total_files;

        if !finished
            && percent_size == self.last_percent_size
            && percent_files == self.last_percent_files
            && now.duration_since(self.last_emit) < PROGRESS_EMIT_INTERVAL
        {
            return;
        }
        self.last_percent_size = percent_size;
        self.last_percent_files = percent_files;
        self.last_emit = now;

        CompressionStatus::Progress(ProgressStatus {
            job_id: self.job_id.clone(),
            processed_files: self.processed_files,
            total_files: self.total_files,
            processed_size_kb: self.processed_size_kb,
            total_size_kb: self.total_size_kb,
            percent_size,
            percent_files,
            current_file: current_file.to_string(),
        })
        .emit(&self.app_handle)
        .ok();
    }
}

pub async fn perform_compression(
    app_handle: AppHandle,
    job_id: String,
    release_dir_path: PathBuf,
    cancel_token: Arc<AtomicBool>,
) -> Result<(u32, u32, PathBuf), AppError> {
    let target_base_path = storage::get_target_path(&app_handle)?;

    let release_name = release_dir_path
        .file_name()
        .ok_or_else(|| AppError::ConfigError("Invalid release directory name".to_string()))?
        .to_string_lossy()
        .to_string();

    let target_dir_path = target_base_path.join(&release_name);
    fs::create_dir_all(&target_dir_path)
        .map_err(|e| AppError::IoError(format!("Failed to create target directory: {}", e)))?;
    let extension = compressors::get_extension_for_compression_type();

    let (group_and_model_dirs, files_for_3pk, files_for_zip) =
        collect_files_for_compression(&release_dir_path)?;

    let (total_size_kb, total_files) =
        calculate_total_size(&group_and_model_dirs, &files_for_3pk, &files_for_zip)?;

    CompressionStatus::Started(StartedStatus {
        job_id: job_id.clone(),
        total_files,
        total_size_kb,
    })
    .emit(&app_handle)
    .ok();

    let progress_tracker = Arc::new(Mutex::new(ProgressTracker::new(
        app_handle.clone(),
        job_id,
        total_files,
        total_size_kb,
    )));

    let app_version = app_handle.package_info().version.to_string();
    // Resolved once here (needs the AppHandle) and just loaded — never
    // generated — inside the blocking task: packing is never the place a
    // signing key gets created, only Settings' ensure_signing_key is.
    let signing_key_path = crate::signing::key_path(&app_handle)?;
    run_compression_tasks(
        progress_tracker,
        &release_dir_path,
        &target_dir_path,
        &extension,
        &app_version,
        &signing_key_path,
        &group_and_model_dirs,
        &files_for_3pk,
        &files_for_zip,
        cancel_token.clone(),
    )
    .await?;

    if cancel_token.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(AppError::UserCancelled(
            "Compression was cancelled by user".into(),
        ));
    }

    fs::remove_dir_all(&release_dir_path)
        .map_err(|e| AppError::IoError(format!("Failed to clean up release directory: {}", e)))?;

    Ok((total_files, total_size_kb, target_dir_path))
}

#[allow(clippy::too_many_arguments)]
async fn run_compression_tasks(
    progress_tracker: Arc<Mutex<ProgressTracker>>,
    release_dir_path: &Path,
    target_dir_path: &Path,
    extension: &str,
    app_version: &str,
    signing_key_path: &Path,
    group_and_model_dirs: &[PathBuf],
    files_for_3pk: &[PathBuf],
    files_for_zip: &[PathBuf],
    cancel_token: Arc<AtomicBool>,
) -> Result<(), AppError> {
    // Bound how many archives are compressed at once
    let max_threads = get_optimal_thread_count() as usize;
    let semaphore = Arc::new(Semaphore::new(max_threads));

    // Stage 1 — component archives, in parallel. They must FINISH before the
    // manifest exists: its component checksums are of the final archive
    // bytes, and its per-file lists come from what each archive stored.
    let mut component_tasks = Vec::new();
    for path in group_and_model_dirs {
        let dir_name = path
            .file_name()
            .ok_or_else(|| AppError::ConfigError("Invalid directory name".to_string()))?
            .to_string_lossy()
            .into_owned();
        let archive_path = target_dir_path.join(format!("{}.{}", dir_name, extension));

        component_tasks.push((
            dir_name,
            archive_path.clone(),
            spawn_compression_task(
                vec![path.clone()],
                archive_path,
                Arc::clone(&progress_tracker),
                Arc::clone(&semaphore),
                Arc::clone(&cancel_token),
            ),
        ));
    }
    let mut components = Vec::with_capacity(component_tasks.len());
    for (name, archive_path, task) in component_tasks {
        let entries = task
            .await
            .map_err(|e| AppError::IoError(format!("Task join error: {}", e)))??;
        components.push(super::pack_manifest::PackedComponent {
            name,
            archive_path,
            entries,
        });
    }

    // Stage 2 — the manifest, written into the release dir so it rides
    // inside release.3pk with the rest of the release-level files
    let mut files_for_3pk = files_for_3pk.to_vec();
    if !components.is_empty() {
        let manifest =
            super::pack_manifest::build_manifest(release_dir_path, &components, app_version)?;
        // The exact bytes written here are what gets zipped verbatim AND
        // (below) what gets signed — no re-serialization between the two,
        // so a signature always covers precisely what a reader will hash.
        let manifest_bytes = manifest.to_json()?.into_bytes();
        let manifest_path = release_dir_path.join("manifest.json");
        fs::write(&manifest_path, &manifest_bytes)
            .map_err(|e| AppError::IoError(format!("Failed to write manifest: {}", e)))?;
        files_for_3pk.push(manifest_path);

        // No key = unsigned pack, silently fine — signing is opt-in via
        // Settings' ensure_signing_key, never a gate on packing.
        if let Some(signing_key) = crate::signing::load_key(signing_key_path)? {
            let signature = crate::signing::sign_manifest(&signing_key, &manifest_bytes);
            let signature_path = release_dir_path.join("manifest.sig");
            fs::write(&signature_path, serde_json::to_string_pretty(&signature)?)
                .map_err(|e| AppError::IoError(format!("Failed to write manifest.sig: {}", e)))?;
            files_for_3pk.push(signature_path);
        }
    }

    // Stage 3 — release.3pk (manifest + images + jsons) and the legacy
    // loose-file release.zip, in parallel
    let mut container_tasks = Vec::new();
    if !files_for_3pk.is_empty() {
        container_tasks.push(spawn_compression_task(
            files_for_3pk,
            target_dir_path.join("release.3pk"),
            Arc::clone(&progress_tracker),
            Arc::clone(&semaphore),
            Arc::clone(&cancel_token),
        ));
    }
    if !files_for_zip.is_empty() {
        container_tasks.push(spawn_compression_task(
            files_for_zip.to_vec(),
            target_dir_path.join(format!("release.{}", extension)),
            Arc::clone(&progress_tracker),
            Arc::clone(&semaphore),
            Arc::clone(&cancel_token),
        ));
    }
    for task in container_tasks {
        task.await
            .map_err(|e| AppError::IoError(format!("Task join error: {}", e)))??;
    }

    Ok(())
}

fn spawn_compression_task(
    files: Vec<PathBuf>,
    output_path: PathBuf,
    progress_tracker: Arc<Mutex<ProgressTracker>>,
    semaphore: Arc<Semaphore>,
    cancel_token: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<Result<Vec<compressors::ArchiveFileEntry>, AppError>> {
    tokio::spawn(async move {
        let _permit = semaphore
            .acquire()
            .await
            .map_err(|e| AppError::ConfigError(format!("Semaphore acquisition error: {}", e)))?;

        if cancel_token.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(AppError::UserCancelled(
                "Compression cancelled by user".into(),
            ));
        }

        tokio::task::spawn_blocking(move || -> Result<Vec<compressors::ArchiveFileEntry>, AppError> {
            let output_file = File::create(&output_path).map_err(|e| {
                AppError::IoError(format!(
                    "Failed to create archive file '{}': {}",
                    output_path.display(),
                    e
                ))
            })?;

            // Displayed as "currently processing"; the first path names the
            // archive's content (the model dir, or the first loose file)
            let current_file = files
                .first()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| output_path.display().to_string());

            // The callback locks the shared tracker only per invocation and
            // returns false to abort the archive as soon as the user cancels.
            let callback = move |file_size_kb: u32| -> bool {
                if cancel_token.load(std::sync::atomic::Ordering::SeqCst) {
                    return false;
                }
                if let Ok(mut tracker) = progress_tracker.lock() {
                    tracker.record_file(file_size_kb, &current_file);
                }
                true
            };

            compressors::compress_files(&files, output_file, Some(callback))
        })
        .await
        .map_err(|e| AppError::IoError(format!("Task panicked: {}", e)))?
    })
}
