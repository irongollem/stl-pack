//! Provisioning a managed Blender: knowing which versions are good enough,
//! and (in later steps) downloading a pinned one when the user has none.
//!
//! Renders are look-locked on Blender 5.1 — older majors light and tone-map
//! the locked look differently. Anything >= 4.2 still *works*, so the gate
//! never blocks: it classifies, and the UI decides how loudly to suggest
//! the managed download.

use crate::error::AppError;
use crate::models::events::{
    BlenderProvisionStatus, ProvisionCancelledStatus, ProvisionCompletedStatus,
    ProvisionExtractingStatus, ProvisionFailedStatus, ProvisionProgressStatus,
    ProvisionStartedStatus,
};
use crate::models::{BlenderCheck, BlenderInfo, BlenderVerdict};
use crate::render::engine;
use once_cell::sync::{Lazy, OnceCell};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use tokio::io::AsyncWriteExt;
use tokio::sync::Notify;

/// The exact version the download pipeline installs. Bumping this is the
/// whole release procedure: the mirror's .sha256 sidecar is fetched at
/// download time, so no hashes live here.
pub const MANAGED_VERSION: &str = "5.1.2";

/// The mirror's release directories group by series: Blender5.1/ holds
/// every 5.1.x artifact and its checksum sidecars.
pub const MANAGED_SERIES: &str = "5.1";

const MIRROR_BASE: &str = "https://download.blender.org/release/";

/// Below this (major, minor) rendering is unsupported outright.
pub const MIN_VERSION: (u32, u32) = (4, 2);

/// Below this the locked look renders differently; the UI suggests (but
/// never forces) the managed download.
pub const RECOMMENDED_VERSION: (u32, u32) = (5, 1);

/// Where managed installs live. Seeded once at startup because the
/// detector's candidate_paths() is synchronous and handle-less — the same
/// reason SETTINGS_CACHE exists. NOT the exe-relative Resources dir the
/// detector also probes: that one is for a Blender *bundled into* the app,
/// and writing there at runtime would break macOS code signing.
static APP_DATA_DIR: OnceCell<PathBuf> = OnceCell::new();

/// Seed APP_DATA_DIR and sweep staging leftovers from crashed downloads.
pub fn init_app_data_dir(app: &AppHandle) {
    if let Ok(dir) = app.path().app_data_dir() {
        let _ = APP_DATA_DIR.set(dir);
    }
    if let Some(root) = managed_root() {
        sweep_stale_staging(&root);
    }
}

/// `<app_data>/blender` — one version dir per managed install inside.
pub fn managed_root() -> Option<PathBuf> {
    APP_DATA_DIR.get().map(|dir| dir.join("blender"))
}

/// The platform binary inside a managed version dir, mirroring the layout
/// each official archive extracts to.
fn binary_in_version_dir(dir: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    return dir.join("Blender.app/Contents/MacOS/Blender");
    #[cfg(target_os = "linux")]
    return dir.join("blender");
    #[cfg(target_os = "windows")]
    return dir.join("blender.exe");
}

/// The binary of the highest-versioned managed install that actually has
/// one — a half-written dir (no binary) never wins.
pub fn managed_binary() -> Option<PathBuf> {
    managed_binary_in(&managed_root()?)
}

fn managed_binary_in(root: &Path) -> Option<PathBuf> {
    let mut versions: Vec<(u32, u32, u32, PathBuf)> = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let (major, minor, patch) = parse_blender_version(&name.to_string_lossy())?;
            let binary = binary_in_version_dir(&entry.path());
            binary.is_file().then_some((major, minor, patch, binary))
        })
        .collect();
    versions.sort();
    versions.pop().map(|(_, _, _, binary)| binary)
}

/// Downloads assemble under dot-prefixed staging dirs and only get renamed
/// into place whole; anything still staging-named at startup is debris from
/// a crash mid-download.
fn sweep_stale_staging(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(".staging-") {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Pull (major, minor, patch) out of a `--version` banner such as
/// "Blender 4.2.1 LTS" or "Blender 5.1.2". Missing components default to 0;
/// a banner with no leading number at all is None.
pub fn parse_blender_version(banner: &str) -> Option<(u32, u32, u32)> {
    let numbers = banner
        .trim_start_matches("Blender")
        .split_whitespace()
        .next()?;
    let mut parts = numbers.split('.');
    let major: u32 = parts.next()?.trim().parse().ok()?;
    let minor: u32 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let patch: u32 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    Some((major, minor, patch))
}

/// Verdict for a *found* install — the caller maps detection failure to
/// Missing. An unparseable banner passes as Ok: never block a working
/// Blender on a banner format change.
pub fn classify(banner: &str) -> BlenderVerdict {
    match parse_blender_version(banner) {
        Some((major, minor, _)) if (major, minor) < MIN_VERSION => BlenderVerdict::TooOld,
        Some((major, minor, _)) if (major, minor) < RECOMMENDED_VERSION => BlenderVerdict::Outdated,
        _ => BlenderVerdict::Ok,
    }
}

/// The official artifact name for one (os, arch, version) — pure so every
/// platform's mapping is testable on one host. None = Blender doesn't ship
/// a portable build for that platform (5.x dropped Intel macOS entirely).
pub fn archive_name(os: &str, arch: &str, version: &str) -> Option<String> {
    let suffix = match (os, arch) {
        ("windows", "x86_64") => "windows-x64.zip",
        ("windows", "aarch64") => "windows-arm64.zip",
        ("linux", "x86_64") => "linux-x64.tar.xz",
        ("macos", "aarch64") => "macos-arm64.dmg",
        _ => return None,
    };
    Some(format!("blender-{}-{}", version, suffix))
}

fn release_dir_url() -> String {
    format!("{}Blender{}/", MIRROR_BASE, MANAGED_SERIES)
}

pub fn download_url() -> Result<(String, String), AppError> {
    let name = archive_name(std::env::consts::OS, std::env::consts::ARCH, MANAGED_VERSION)
        .ok_or_else(|| {
            AppError::ConfigError(format!(
                "No portable Blender {} build exists for {} on {}",
                MANAGED_VERSION,
                std::env::consts::OS,
                std::env::consts::ARCH
            ))
        })?;
    Ok((format!("{}{}", release_dir_url(), name), name))
}

/// The mirror publishes one sidecar per release ("blender-5.1.2.sha256")
/// listing "<hex>  <artifact>" for every artifact in the release.
pub fn parse_sha256_sidecar(text: &str, file_name: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (hash, name) = line.trim().split_once("  ")?;
        (name.trim() == file_name).then(|| hash.to_ascii_lowercase())
    })
}

/// Download, verify, extract, and atomically install the pinned Blender.
/// Everything happens inside a dot-prefixed staging dir on the same
/// filesystem as the final home, so the last rename is atomic and any
/// failure/cancel leaves nothing but sweepable staging debris.
///
/// Progress flows through callbacks — the event plumbing lives with the
/// command layer, keeping this pipeline runnable from a bare test.
pub async fn install_managed_blender(
    cancel: &Notify,
    mut on_download: impl FnMut(u64, Option<u64>),
    mut on_phase: impl FnMut(&'static str),
) -> Result<BlenderInfo, AppError> {
    let root = managed_root().ok_or_else(|| {
        AppError::ConfigError("No writable app data dir for the managed Blender".to_string())
    })?;
    std::fs::create_dir_all(&root)?;
    let staging = root.join(format!(".staging-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&staging)?;

    let result = run_pipeline(&root, &staging, cancel, &mut on_download, &mut on_phase).await;
    let _ = std::fs::remove_dir_all(&staging);
    result?;

    engine::invalidate_detection_cache();
    engine::detect_blender().await
}

async fn run_pipeline(
    root: &Path,
    staging: &Path,
    cancel: &Notify,
    on_download: &mut impl FnMut(u64, Option<u64>),
    on_phase: &mut impl FnMut(&'static str),
) -> Result<(), AppError> {
    let (url, archive_file) = download_url()?;
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| AppError::IoError(e.to_string()))?;

    // Expected hash first — a dead mirror fails fast, before 350 MB
    let sidecar_url = format!("{}blender-{}.sha256", release_dir_url(), MANAGED_VERSION);
    let sidecar = http_text(&client, &sidecar_url).await?;
    let expected = parse_sha256_sidecar(&sidecar, &archive_file).ok_or_else(|| {
        AppError::FileProcessingError(format!("{} is not listed in {}", archive_file, sidecar_url))
    })?;

    // Hash while the bytes arrive — no second pass over the archive
    let archive_path = staging.join(&archive_file);
    let actual = download_hashed(&client, &url, &archive_path, cancel, on_download).await?;

    on_phase("verify");
    if actual != expected {
        return Err(AppError::FileProcessingError(format!(
            "Checksum mismatch for {} — expected {}, got {}. Download corrupted or tampered.",
            archive_file, expected, actual
        )));
    }

    on_phase("extract");
    let extracted = extract_archive(&archive_path, staging).await?;

    on_phase("install");
    let final_dir = root.join(MANAGED_VERSION);
    if final_dir.exists() {
        std::fs::remove_dir_all(&final_dir)?;
    }
    std::fs::rename(&extracted, &final_dir)?;
    write_license_notice(&final_dir)?;
    remove_old_versions(root, MANAGED_VERSION);
    Ok(())
}

async fn http_text(client: &reqwest::Client, url: &str) -> Result<String, AppError> {
    let response = client
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| AppError::IoError(format!("GET {} failed: {}", url, e)))?;
    response
        .text()
        .await
        .map_err(|e| AppError::IoError(format!("GET {} failed: {}", url, e)))
}

/// Stream the archive to disk, feeding the hasher and the progress callback
/// per chunk. Returns the hex sha256 of what actually landed on disk.
async fn download_hashed(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    cancel: &Notify,
    on_download: &mut impl FnMut(u64, Option<u64>),
) -> Result<String, AppError> {
    let mut response = client
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| AppError::IoError(format!("GET {} failed: {}", url, e)))?;
    let total = response.content_length();

    let mut file = tokio::fs::File::create(dest).await?;
    let mut hasher = Sha256::new();
    let mut downloaded: u64 = 0;
    let mut cancelled = std::pin::pin!(cancel.notified());
    loop {
        let chunk = tokio::select! {
            _ = &mut cancelled => {
                return Err(AppError::UserCancelled("Blender download cancelled".to_string()));
            }
            chunk = response.chunk() => {
                chunk.map_err(|e| AppError::IoError(format!("GET {} failed: {}", url, e)))?
            }
        };
        let Some(bytes) = chunk else { break };
        hasher.update(&bytes);
        file.write_all(&bytes).await?;
        downloaded += bytes.len() as u64;
        on_download(downloaded, total);
    }
    file.flush().await?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Unpack the archive inside staging and return the directory whose
/// CONTENTS are the final version-dir layout (what binary_in_version_dir
/// expects to find inside <root>/<version>/).
async fn extract_archive(archive: &Path, staging: &Path) -> Result<PathBuf, AppError> {
    let out = staging.join("extracted");
    std::fs::create_dir_all(&out)?;

    #[cfg(target_os = "windows")]
    {
        // The zip holds a single blender-<version>-windows-<arch>/ top dir
        let archive = archive.to_path_buf();
        let out_dir = out.clone();
        tokio::task::spawn_blocking(move || -> Result<(), AppError> {
            let file = std::fs::File::open(&archive)?;
            let mut zip = zip::ZipArchive::new(file)
                .map_err(|e| AppError::FileProcessingError(e.to_string()))?;
            zip.extract(&out_dir)
                .map_err(|e| AppError::FileProcessingError(e.to_string()))
        })
        .await
        .map_err(|e| AppError::IoError(e.to_string()))??;
        sole_subdir(&out)
    }

    #[cfg(target_os = "linux")]
    {
        // tar + xz ship with every desktop distro Blender itself runs on;
        // shelling out beats binding liblzma for this one platform
        let status = tokio::process::Command::new("tar")
            .arg("-xJf")
            .arg(archive)
            .arg("-C")
            .arg(&out)
            .status()
            .await
            .map_err(|e| AppError::IoError(format!("tar unavailable: {}", e)))?;
        if !status.success() {
            return Err(AppError::FileProcessingError(
                "tar failed to extract the Blender archive".to_string(),
            ));
        }
        sole_subdir(&out)
    }

    #[cfg(target_os = "macos")]
    {
        // Mount the dmg and ditto Blender.app out — ditto preserves the
        // code signature and extended attributes, cp -R can break both
        let mount = staging.join("mnt");
        let status = tokio::process::Command::new("hdiutil")
            .args(["attach", "-nobrowse", "-readonly", "-mountpoint"])
            .arg(&mount)
            .arg(archive)
            .status()
            .await
            .map_err(|e| AppError::IoError(format!("hdiutil unavailable: {}", e)))?;
        if !status.success() {
            return Err(AppError::FileProcessingError(
                "hdiutil could not mount the Blender disk image".to_string(),
            ));
        }

        let copy_result = tokio::process::Command::new("ditto")
            .arg(mount.join("Blender.app"))
            .arg(out.join("Blender.app"))
            .status()
            .await;

        // Always unmount, even when the copy failed
        let _ = tokio::process::Command::new("hdiutil")
            .arg("detach")
            .arg(&mount)
            .status()
            .await;

        match copy_result {
            Ok(status) if status.success() => Ok(out),
            Ok(_) => Err(AppError::FileProcessingError(
                "ditto could not copy Blender.app out of the disk image".to_string(),
            )),
            Err(e) => Err(AppError::IoError(format!("ditto unavailable: {}", e))),
        }
    }
}

/// The archive's one top-level directory (zip/tar layouts wrap everything).
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn sole_subdir(dir: &Path) -> Result<PathBuf, AppError> {
    let mut dirs = std::fs::read_dir(dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir());
    let first = dirs.next().ok_or_else(|| {
        AppError::FileProcessingError("Extracted archive is empty".to_string())
    })?;
    if dirs.next().is_some() {
        return Err(AppError::FileProcessingError(
            "Extracted archive has more than one top-level directory".to_string(),
        ));
    }
    Ok(first)
}

/// Blender is GPL: we fetch it from the official mirror at the user's
/// request rather than bundling it, but the install still carries its
/// license notice and a pointer to the source.
fn write_license_notice(dir: &Path) -> Result<(), AppError> {
    std::fs::write(
        dir.join("LICENSE-blender.txt"),
        "Blender is free software licensed under the GNU General Public License.\n\
         This copy was downloaded from the official mirror at\n\
         https://download.blender.org/release/ on your request.\n\n\
         License: https://www.blender.org/about/license/\n\
         Source code: https://projects.blender.org/blender/blender\n",
    )?;
    Ok(())
}

/// One managed install is plenty (~1 GB each): version-named siblings of
/// the one we just placed are previous downloads.
fn remove_old_versions(root: &Path, keep: &str) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name != keep && parse_blender_version(&name).is_some() && entry.path().is_dir() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// A running download's job id and its cancel token.
type ActiveProvisionJob = (String, Arc<Notify>);

/// The one running download, if any — two concurrent downloads of the same
/// pinned Blender could only fight over the same final directory.
static ACTIVE_PROVISION: Lazy<Mutex<Option<ActiveProvisionJob>>> = Lazy::new(|| Mutex::new(None));

/// Kick off the managed download; progress arrives as
/// BlenderProvisionStatus events carrying the returned job_id.
#[tauri::command]
#[specta::specta]
pub async fn download_blender(app_handle: AppHandle) -> Result<String, AppError> {
    let job_id = uuid::Uuid::new_v4().to_string();
    let cancel_token = Arc::new(Notify::new());
    {
        let mut active = ACTIVE_PROVISION
            .lock()
            .map_err(|e| AppError::ConfigError(format!("Provision registry poisoned: {}", e)))?;
        if active.is_some() {
            return Err(AppError::InvalidInput(
                "A Blender download is already running".to_string(),
            ));
        }
        *active = Some((job_id.clone(), Arc::clone(&cancel_token)));
    }

    let job = job_id.clone();
    tokio::spawn(async move {
        run_provision_job(app_handle, job, cancel_token).await;
        if let Ok(mut active) = ACTIVE_PROVISION.lock() {
            *active = None;
        }
    });
    Ok(job_id)
}

#[tauri::command]
#[specta::specta]
pub async fn cancel_blender_download(job_id: String) -> Result<(), AppError> {
    if let Ok(active) = ACTIVE_PROVISION.lock() {
        if let Some((id, token)) = active.as_ref() {
            if *id == job_id {
                token.notify_waiters();
            }
        }
    }
    Ok(())
}

async fn run_provision_job(app_handle: AppHandle, job_id: String, cancel: Arc<Notify>) {
    let _ = BlenderProvisionStatus::Started(ProvisionStartedStatus {
        job_id: job_id.clone(),
        version: MANAGED_VERSION.to_string(),
    })
    .emit(&app_handle);

    // Chunks arrive every few KB; only whole-percent changes cross the IPC
    let mut last_percent = u32::MAX;
    let result = install_managed_blender(
        &cancel,
        |downloaded, total| {
            let total = total.unwrap_or(0);
            let percent = (downloaded * 100).checked_div(total).unwrap_or(0) as u32;
            if percent != last_percent {
                last_percent = percent;
                let _ = BlenderProvisionStatus::Progress(ProvisionProgressStatus {
                    job_id: job_id.clone(),
                    downloaded_bytes: downloaded as f64,
                    total_bytes: total as f64,
                    percent,
                })
                .emit(&app_handle);
            }
        },
        |phase| {
            let _ = BlenderProvisionStatus::Extracting(ProvisionExtractingStatus {
                job_id: job_id.clone(),
                phase: phase.to_string(),
            })
            .emit(&app_handle);
        },
    )
    .await;

    let event = match result {
        Ok(info) => BlenderProvisionStatus::Completed(ProvisionCompletedStatus {
            job_id: job_id.clone(),
            info,
        }),
        Err(AppError::UserCancelled(_)) => {
            BlenderProvisionStatus::Cancelled(ProvisionCancelledStatus {
                job_id: job_id.clone(),
            })
        }
        Err(err) => BlenderProvisionStatus::Failed(ProvisionFailedStatus {
            job_id: job_id.clone(),
            error: err.to_string(),
        }),
    };
    let _ = event.emit(&app_handle);
}

/// Detection plus the version verdict, in one call the first-run dialog,
/// Settings, and the Render view all share.
#[tauri::command]
#[specta::specta]
pub async fn check_blender() -> Result<BlenderCheck, AppError> {
    match engine::detect_blender().await {
        Ok(info) => Ok(BlenderCheck {
            verdict: classify(&info.version),
            is_managed: managed_root()
                .is_some_and(|root| Path::new(&info.path).starts_with(&root)),
            info: Some(info),
            managed_version: MANAGED_VERSION.to_string(),
        }),
        Err(AppError::NotFoundError(_)) => Ok(BlenderCheck {
            verdict: BlenderVerdict::Missing,
            info: None,
            is_managed: false,
            managed_version: MANAGED_VERSION.to_string(),
        }),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_version_banners() {
        assert_eq!(parse_blender_version("Blender 5.1.2"), Some((5, 1, 2)));
        assert_eq!(parse_blender_version("Blender 4.2.1 LTS"), Some((4, 2, 1)));
        assert_eq!(parse_blender_version("Blender 4.5"), Some((4, 5, 0)));
        assert_eq!(parse_blender_version("garbage"), None);
        assert_eq!(parse_blender_version("Blender Foundation"), None);
    }

    #[test]
    fn archive_names_match_the_mirror() {
        assert_eq!(
            archive_name("windows", "x86_64", "5.1.2").as_deref(),
            Some("blender-5.1.2-windows-x64.zip")
        );
        assert_eq!(
            archive_name("windows", "aarch64", "5.1.2").as_deref(),
            Some("blender-5.1.2-windows-arm64.zip")
        );
        assert_eq!(
            archive_name("linux", "x86_64", "5.1.2").as_deref(),
            Some("blender-5.1.2-linux-x64.tar.xz")
        );
        assert_eq!(
            archive_name("macos", "aarch64", "5.1.2").as_deref(),
            Some("blender-5.1.2-macos-arm64.dmg")
        );
        // Blender 5.x dropped Intel macOS — no artifact to point at
        assert_eq!(archive_name("macos", "x86_64", "5.1.2"), None);
        assert_eq!(archive_name("freebsd", "x86_64", "5.1.2"), None);
    }

    #[test]
    fn sidecar_yields_the_right_hash() {
        // Shape lifted from the real blender-5.1.2.sha256
        let sidecar = "f104ffee2ba6aee32328e5c203b7e4608d8a1745f7bbcf2766f3b9777e8fbe17  blender-5.1.2-macos-arm64.dmg\n\
                       aaccb355f50183979b698bcce7467103a76261b5fa59f4972295842662a285fb  blender-5.1.2-linux-x64.tar.xz\n\
                       malformed-line-without-separator\n";
        assert_eq!(
            parse_sha256_sidecar(sidecar, "blender-5.1.2-linux-x64.tar.xz").as_deref(),
            Some("aaccb355f50183979b698bcce7467103a76261b5fa59f4972295842662a285fb")
        );
        assert_eq!(parse_sha256_sidecar(sidecar, "blender-5.1.2-windows-x64.zip"), None);
        assert_eq!(parse_sha256_sidecar("", "anything"), None);
    }

    #[test]
    fn release_urls_have_the_series_layout() {
        assert_eq!(
            release_dir_url(),
            format!("https://download.blender.org/release/Blender{}/", MANAGED_SERIES)
        );
        // The pinned version must belong to the series directory it's
        // fetched from, or every download 404s
        assert!(MANAGED_VERSION.starts_with(MANAGED_SERIES));
    }

    #[test]
    fn old_managed_versions_are_removed_but_nothing_else() {
        let root = std::env::temp_dir().join(format!("plinth-sweep-{}", uuid::Uuid::new_v4()));
        for dir in ["4.2.9", "5.1.2", "not-a-version"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        remove_old_versions(&root, "5.1.2");
        assert!(!root.join("4.2.9").exists());
        assert!(root.join("5.1.2").exists());
        assert!(root.join("not-a-version").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn highest_complete_managed_version_wins() {
        let root = std::env::temp_dir().join(format!("plinth-managed-{}", uuid::Uuid::new_v4()));

        // No root dir at all -> no managed install
        assert_eq!(managed_binary_in(&root), None);

        // 4.2.0 complete, 5.1.2 complete, 6.0.0 half-written (no binary)
        for version in ["4.2.0", "5.1.2"] {
            let binary = binary_in_version_dir(&root.join(version));
            std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
            std::fs::write(&binary, "stub").unwrap();
        }
        std::fs::create_dir_all(root.join("6.0.0")).unwrap();

        let winner = managed_binary_in(&root).expect("a managed binary");
        assert!(winner.starts_with(root.join("5.1.2")));

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The full pipeline against the real mirror: download (~350 MB),
    /// checksum, extract, install into a scratch APP_DATA_DIR.
    /// Run with: cargo test -- --ignored
    #[tokio::test]
    #[ignore = "downloads ~350 MB from download.blender.org"]
    async fn installs_managed_blender_end_to_end() {
        let scratch = std::env::temp_dir().join(format!("plinth-e2e-{}", uuid::Uuid::new_v4()));
        APP_DATA_DIR
            .set(scratch.clone())
            .expect("APP_DATA_DIR unset — run this test alone");

        let cancel = Notify::new();
        let mut last_percent = 0u64;
        let info = install_managed_blender(
            &cancel,
            |done, total| {
                if let Some(total) = total {
                    last_percent = done * 100 / total;
                }
            },
            |phase| println!("phase: {}", phase),
        )
        .await
        .expect("pipeline should install a working Blender");

        assert_eq!(last_percent, 100);
        assert!(info.version.contains(MANAGED_VERSION), "got {}", info.version);
        assert!(Path::new(&info.path).starts_with(&scratch));
        assert!(scratch
            .join("blender")
            .join(MANAGED_VERSION)
            .join("LICENSE-blender.txt")
            .is_file());
        // Staging must not survive success
        assert!(!std::fs::read_dir(scratch.join("blender"))
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with(".staging-")));

        std::fs::remove_dir_all(&scratch).unwrap();
    }

    #[test]
    fn classifies_against_the_gate() {
        assert!(matches!(classify("Blender 4.1.1"), BlenderVerdict::TooOld));
        assert!(matches!(classify("Blender 3.6.5 LTS"), BlenderVerdict::TooOld));
        assert!(matches!(classify("Blender 4.2.0"), BlenderVerdict::Outdated));
        assert!(matches!(classify("Blender 5.0.9"), BlenderVerdict::Outdated));
        assert!(matches!(classify("Blender 5.1.0"), BlenderVerdict::Ok));
        assert!(matches!(classify("Blender 6.0.0"), BlenderVerdict::Ok));
        // Lenient on the unknown: a working Blender is never blocked on
        // a banner we can't read
        assert!(matches!(classify("Blender next"), BlenderVerdict::Ok));
    }
}
