//! Compressed-at-rest packing: a model directory's DIRECT model files
//! bundled into `model.plinthpack` (Zstd zip) plus a loose `pack.json`
//! sidecar, so a rescan reads one small JSON instead of opening the
//! archive — which matters over SMB.
//!
//! Non-recursive: a nested variant dir packs itself, so no archive can
//! capture another model's files. Only MODEL_EXTENSIONS are packed;
//! images and `model.json` stay loose.
//!
//! State transitions are crash-safe by ordering, not by locks:
//! - pack: compress to temp → verify → rename archive → rename sidecar →
//!   delete originals. Interrupted at any point, a re-run finishes it.
//! - unpack: extract → delete archive + sidecar. Re-runnable; the
//!   scanner's loose-wins rule keeps the catalog correct mid-way.

use crate::catalog::MODEL_EXTENSIONS;
use crate::catalog::scanner::{is_hidden, read_json};
use crate::error::AppError;
use crate::file::compressors::{ArchivePlan, CompressOptions, compress_planned};
use crate::manifest::{self, ManifestFile};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub const PACK_ARCHIVE_NAME: &str = "model.plinthpack";
pub const PACK_SIDECAR_NAME: &str = "pack.json";
pub const PACK_FORMAT: &str = "plinth-pack";
/// Bump on a breaking change; readers reject unknown versions.
pub const PACK_VERSION: u32 = 1;

/// The loose sidecar written next to `model.plinthpack`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PackSidecar {
    pub format: String,
    pub version: u32,
    /// e.g. "plinth/0.1.0" — provenance, not load-bearing.
    pub generator: String,
    /// Archive filename (always PACK_ARCHIVE_NAME; recorded for forward compat).
    pub archive: String,
    /// `blake3:<hex>` of the archive bytes.
    pub archive_checksum: String,
    pub archive_size_bytes: u64,
    /// Unix seconds; when the pack finished.
    pub packed_at: i64,
    pub files: Vec<PackFileEntry>,
}

/// One packed file. Mirrors compressors::ArchiveFileEntry plus the original
/// mtime, which the index preserves so packed rows look exactly like the
/// loose rows they replaced.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PackFileEntry {
    /// Model-dir-relative path with '/' separators (== archive entry name).
    pub name: String,
    /// `blake3:<hex>` of the file's content.
    pub checksum: String,
    pub size_bytes: u64,
    /// Unix seconds, from the original loose file.
    pub modified_at: i64,
    /// False when the bytes were dedup-elided into a sibling entry with the
    /// same checksum (see compressors::compress_files).
    pub stored: bool,
}

impl PackSidecar {
    pub fn is_readable(&self) -> bool {
        self.format == PACK_FORMAT && self.version == PACK_VERSION
    }
}

impl PackFileEntry {
    fn to_manifest_file(&self) -> ManifestFile {
        ManifestFile {
            name: self.name.clone(),
            checksum: self.checksum.clone(),
            size_bytes: self.size_bytes,
            pose: None,
            support_status: None,
        }
    }
}

/// What pack_model did. `kept` lists loose files that changed between
/// compression and the delete pass — they stay on disk (the archive holds
/// the version that was verified) and the caller should surface them.
#[derive(Debug)]
pub struct PackOutcome {
    pub sidecar: PackSidecar,
    pub kept: Vec<String>,
}

/// Which stage a progress tick belongs to; the compress and verify stages
/// each stream the full payload once, so a caller wanting one percentage can
/// treat the total as 2x the model's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackPhase {
    Compress,
    Verify,
}

/// Temp names carry PID + a process-wide sequence: PID alone collided when
/// two jobs in one process (a pack and an extract, or a double-clicked pack)
/// touched the same dir — one truncating the other's live temp.
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn temp_name(kind: &str) -> String {
    format!(
        ".plinth-{}-{}-{}.tmp",
        kind,
        std::process::id(),
        TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// The files.content_hash column stores BARE hex (the duplicate scanner's
/// format); sidecars and manifests carry "blake3:<hex>". Strip at the index
/// boundary or packed files never group with their loose twins.
pub fn bare_hash(checksum: &str) -> &str {
    checksum.strip_prefix("blake3:").unwrap_or(checksum)
}

/// A packed entry's real on-disk location: entry names use '/' regardless
/// of platform, so split into components rather than joining the raw
/// string. Traversal segments are dropped defensively — a sidecar is
/// just a JSON file anyone can edit, and "../../x" must not resolve
/// outside the model dir.
pub fn entry_disk_path(model_dir: &Path, name: &str) -> PathBuf {
    let mut path = model_dir.to_path_buf();
    for segment in name
        .split('/')
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
    {
        path.push(segment);
    }
    path
}

/// Bundle `model_dir`'s model files into model.plinthpack + pack.json, then
/// delete the loose originals. Idempotent: re-running after a crash or
/// cancel finishes whatever step was interrupted. The progress callback
/// receives (phase, KiB just processed) and returns whether to CONTINUE;
/// cancellation is honoured up to the verify step — after that the pack
/// finishes (renames + deletes are quick and interrupting them buys nothing).
pub fn pack_model(
    app_version: &str,
    model_dir: &Path,
    level: Option<i64>,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(PackPhase, u32) -> bool,
) -> Result<PackOutcome, AppError> {
    if !model_dir.is_dir() {
        return Err(AppError::NotFoundError(format!(
            "'{}' is not a directory",
            model_dir.display()
        )));
    }
    let archive_path = model_dir.join(PACK_ARCHIVE_NAME);
    let sidecar_path = model_dir.join(PACK_SIDECAR_NAME);
    sweep_stale_temps(model_dir);

    // Already packed (or a crash after both renames): verify what's there and
    // finish the delete pass. This branch is why bulk re-runs resume cleanly.
    if archive_path.is_file() && sidecar_path.is_file() {
        let sidecar: PackSidecar = read_json(&sidecar_path)?;
        if !sidecar.is_readable() {
            return Err(AppError::InvalidInput(format!(
                "'{}' has an unreadable pack.json (format {} v{})",
                model_dir.display(),
                sidecar.format,
                sidecar.version
            )));
        }
        if manifest::hash_file(&archive_path)? != sidecar.archive_checksum {
            return Err(AppError::InvalidInput(format!(
                "'{}' does not match its pack.json checksum — not deleting anything. \
                 Unpack manually or remove the archive and re-pack.",
                archive_path.display()
            )));
        }
        let kept = delete_packed_originals(model_dir, &sidecar.files)?;
        return Ok(PackOutcome { sidecar, kept });
    }

    // Collect the dir's DIRECT model files only. Not recursive on purpose:
    // any subdirectory that holds model files is a catalog member in its
    // own right and packs itself — recursing here would swallow a nested
    // variant's files into the parent's archive. Images, jsons and the
    // archive itself stay loose by design (extension filter).
    let mut plan: ArchivePlan = Vec::new();
    let mut stats_by_name: HashMap<String, (u64, i64)> = HashMap::new();
    let read_dir = fs::read_dir(model_dir)
        .map_err(|e| AppError::IoError(format!("Failed to read {}: {}", model_dir.display(), e)))?;
    for entry in read_dir.filter_map(|e| e.ok()) {
        if cancel.load(Ordering::SeqCst) {
            return Err(AppError::UserCancelled("Pack cancelled".to_string()));
        }
        let path = entry.path();
        if is_hidden(&path) || !path.is_file() {
            continue;
        }
        let extension = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if !MODEL_EXTENSIONS.contains(&extension.as_str()) {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str().map(str::to_owned))
            .ok_or_else(|| {
                AppError::FileProcessingError(format!(
                    "Invalid UTF-8 in file name: {}",
                    path.display()
                ))
            })?;
        let metadata = path
            .metadata()
            .map_err(|e| AppError::IoError(format!("Failed to stat {}: {}", path.display(), e)))?;
        let modified_at = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        stats_by_name.insert(name.clone(), (metadata.len(), modified_at));
        plan.push((path, name));
    }

    if plan.is_empty() {
        if archive_path.is_file() {
            // Archive without sidecar and without loose files: we can't
            // verify anything, so refuse rather than guess.
            return Err(AppError::InvalidInput(format!(
                "'{}' exists without a pack.json and the model files are gone — cannot verify it",
                archive_path.display()
            )));
        }
        return Err(AppError::InvalidInput(format!(
            "No model files to pack in '{}'",
            model_dir.display()
        )));
    }
    if archive_path.is_file() {
        // Archive without sidecar, loose files present — two different
        // crash states look identical here (a clean crash before the
        // sidecar rename vs. a LOST sidecar with a stray loose file).
        // Only the archive can tell them apart (orphan_covered_by_loose).
        if orphan_covered_by_loose(model_dir, &archive_path) {
            fs::remove_file(&archive_path)?;
        } else {
            let sidecar = rebuild_sidecar(model_dir)?;
            let kept = delete_packed_originals(model_dir, &sidecar.files)?;
            return Ok(PackOutcome { sidecar, kept });
        }
    }

    // Compress to a hidden temp (invisible to the scanner) in the model dir —
    // same volume, so the final rename is a metadata op even on SMB
    let temp_archive = model_dir.join(temp_name("pack"));
    let writer = fs::File::create(&temp_archive)
        .map_err(|e| AppError::IoError(format!("Failed to create pack temp: {}", e)))?;
    let compressed = compress_planned(
        plan,
        &[],
        writer,
        CompressOptions::zstd(level),
        Some(|kb: u32| {
            !cancel.load(Ordering::SeqCst) && on_progress(PackPhase::Compress, kb)
        }),
    );
    let entries = match compressed {
        Ok(entries) => entries,
        Err(e) => {
            fs::remove_file(&temp_archive).ok();
            return Err(e);
        }
    };

    // Verify before any original is touched: every stored entry decompresses
    // back to the exact bytes we hashed on the way in
    if let Err(e) = verify_archive(&temp_archive, &entries, cancel, &mut on_progress) {
        fs::remove_file(&temp_archive).ok();
        return Err(e);
    }
    let archive_checksum = manifest::hash_file(&temp_archive)?;
    let archive_size_bytes = fs::metadata(&temp_archive)?.len();

    let sidecar = PackSidecar {
        format: PACK_FORMAT.to_string(),
        version: PACK_VERSION,
        generator: format!("plinth/{}", app_version),
        archive: PACK_ARCHIVE_NAME.to_string(),
        archive_checksum,
        archive_size_bytes,
        packed_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
        files: entries
            .iter()
            .map(|e| {
                let (size_bytes, modified_at) =
                    stats_by_name.get(&e.name).copied().unwrap_or((e.size_bytes, 0));
                PackFileEntry {
                    name: e.name.clone(),
                    checksum: e.checksum.clone(),
                    size_bytes,
                    modified_at,
                    stored: e.stored,
                }
            })
            .collect(),
    };

    // Point of no return: archive first, sidecar second. An archive without a
    // sidecar is a recognizable orphan (handled above); a sidecar without an
    // archive would look like a packed model with its data missing.
    fs::rename(&temp_archive, &archive_path)
        .map_err(|e| AppError::IoError(format!("Failed to finalize archive: {}", e)))?;
    write_sidecar(model_dir, &sidecar)?;

    let kept = delete_packed_originals(model_dir, &sidecar.files)?;
    Ok(PackOutcome { sidecar, kept })
}

/// Whether every file entry in a sidecar-less archive already exists loose
/// with a matching size — the signature of a crash between the archive and
/// sidecar renames, where the archive is a discardable duplicate. Names and
/// sizes come from the central directory, so nothing is decompressed. An
/// unreadable archive is trivially "covered": it holds nothing recoverable.
fn orphan_covered_by_loose(model_dir: &Path, archive_path: &Path) -> bool {
    let Ok(file) = fs::File::open(archive_path) else {
        return true;
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return true;
    };
    for i in 0..archive.len() {
        let Ok(entry) = archive.by_index(i) else {
            return true;
        };
        if entry.is_dir() {
            continue;
        }
        let path = entry_disk_path(model_dir, entry.name());
        match fs::metadata(&path) {
            Ok(metadata) if metadata.len() == entry.size() => continue,
            _ => return false,
        }
    }
    true
}

/// Atomically place a pack.json next to the archive (hidden temp + rename;
/// an existing sidecar is removed first because rename-over-existing fails
/// on Windows).
fn write_sidecar(model_dir: &Path, sidecar: &PackSidecar) -> Result<(), AppError> {
    let sidecar_path = model_dir.join(PACK_SIDECAR_NAME);
    let temp_sidecar = model_dir.join(temp_name("packjson"));
    let json = serde_json::to_string_pretty(sidecar)
        .map_err(|e| AppError::JsonError(format!("Failed to encode pack.json: {}", e)))?;
    fs::write(&temp_sidecar, json)?;
    fs::remove_file(&sidecar_path).ok();
    fs::rename(&temp_sidecar, &sidecar_path)
        .map_err(|e| AppError::IoError(format!("Failed to finalize pack.json: {}", e)))?;
    Ok(())
}

/// Self-heal: rebuild a lost or unreadable pack.json from the archive
/// itself. The zip's central directory gives the names; checksums are
/// recomputed by decompressing each entry (slow — only for a damaged
/// dir). Two inherent limitations: dedup-elided names are gone (their
/// bytes remain under the twin's name), and mtimes aren't archived, so
/// entries take the archive's own mtime.
pub fn rebuild_sidecar(model_dir: &Path) -> Result<PackSidecar, AppError> {
    let archive_path = model_dir.join(PACK_ARCHIVE_NAME);
    let metadata = fs::metadata(&archive_path)
        .map_err(|e| AppError::IoError(format!("Failed to stat {}: {}", archive_path.display(), e)))?;
    let modified_at = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let file = fs::File::open(&archive_path)
        .map_err(|e| AppError::IoError(format!("Failed to open {}: {}", archive_path.display(), e)))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| AppError::InvalidInput(format!("Not a readable archive: {}", e)))?;

    let mut files: Vec<PackFileEntry> = Vec::new();
    for i in 0..archive.len() {
        let mut zf = archive
            .by_index(i)
            .map_err(|e| AppError::FileProcessingError(format!("Archive entry {}: {}", i, e)))?;
        if zf.is_dir() {
            continue;
        }
        let name = zf.name().to_string();
        // an entry that would escape the model dir is hostile or corrupt —
        // refuse the whole rebuild rather than record it
        if name.split('/').any(|s| s == "..") || name.starts_with('/') {
            return Err(AppError::InvalidInput(format!(
                "Archive entry '{}' escapes the model dir — refusing to rebuild pack.json",
                name
            )));
        }
        let mut hasher = blake3::Hasher::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut size_bytes: u64 = 0;
        loop {
            let read = zf.read(&mut buffer).map_err(|e| {
                AppError::FileProcessingError(format!("Failed reading '{}': {}", name, e))
            })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            size_bytes += read as u64;
        }
        files.push(PackFileEntry {
            name,
            checksum: format!("blake3:{}", hasher.finalize().to_hex()),
            size_bytes,
            modified_at,
            stored: true,
        });
    }
    if files.is_empty() {
        return Err(AppError::InvalidInput(format!(
            "'{}' holds no files — nothing to rebuild",
            archive_path.display()
        )));
    }

    let sidecar = PackSidecar {
        format: PACK_FORMAT.to_string(),
        version: PACK_VERSION,
        // provenance, not load-bearing: marks that this sidecar was
        // reconstructed rather than written at pack time
        generator: "plinth/sidecar-rebuild".to_string(),
        archive: PACK_ARCHIVE_NAME.to_string(),
        archive_checksum: manifest::hash_file(&archive_path)?,
        archive_size_bytes: metadata.len(),
        packed_at: modified_at,
        files,
    };
    write_sidecar(model_dir, &sidecar)?;
    Ok(sidecar)
}

/// What unpack_model did: the sidecar's entries, plus any diverged loose
/// files that were moved aside as "<name> (edited)" instead of being
/// truncated by the extraction.
#[derive(Debug)]
pub struct UnpackOutcome {
    pub files: Vec<PackFileEntry>,
    pub preserved: Vec<String>,
}

/// Restore a packed model to loose files and remove the archive + sidecar.
/// Idempotent: identical already-present loose files are overwritten with
/// the verified archive bytes, and dedup-elided names are rematerialized.
/// A loose file that DIVERGED from its archived version (user edits, saved
/// supports — exactly what the pack delete-guard kept) is moved aside as
/// "<name> (edited)" first, never truncated.
pub fn unpack_model(model_dir: &Path) -> Result<UnpackOutcome, AppError> {
    let archive_path = model_dir.join(PACK_ARCHIVE_NAME);
    let sidecar_path = model_dir.join(PACK_SIDECAR_NAME);
    if !sidecar_path.is_file() {
        return Err(AppError::NotFoundError(format!(
            "'{}' is not packed (no pack.json)",
            model_dir.display()
        )));
    }
    let sidecar: PackSidecar = read_json(&sidecar_path)?;
    if !sidecar.is_readable() {
        return Err(AppError::InvalidInput(format!(
            "'{}' has an unreadable pack.json (format {} v{})",
            model_dir.display(),
            sidecar.format,
            sidecar.version
        )));
    }
    let mut preserved: Vec<String> = Vec::new();
    for entry in &sidecar.files {
        let path = entry_disk_path(model_dir, &entry.name);
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        let identical = metadata.len() == entry.size_bytes
            && manifest::hash_file(&path)? == entry.checksum;
        if identical {
            continue;
        }
        let aside = edited_aside_path(&path);
        fs::rename(&path, &aside).map_err(|e| {
            AppError::IoError(format!(
                "Failed to preserve edited '{}': {}",
                path.display(),
                e
            ))
        })?;
        preserved.push(aside.to_string_lossy().into_owned());
    }
    let files: Vec<ManifestFile> = sidecar
        .files
        .iter()
        .map(PackFileEntry::to_manifest_file)
        .collect();
    // Zip per-entry CRCs guard the extraction itself; elided names come back
    // as hardlinks/copies of their checksum twin
    manifest::extract_component_archive(&archive_path, model_dir, &files)?;
    fs::remove_file(&archive_path)?;
    fs::remove_file(&sidecar_path)?;
    // Anything extracted ephemerally from this dir is now a permanent loose
    // file — forget it, or a later cleanup would delete real data
    if let Ok(mut registry) = EPHEMERAL_EXTRACTS.lock() {
        let prefix = model_dir.to_string_lossy().into_owned();
        registry.retain(|path, _| {
            Path::new(path)
                .parent()
                .map(|p| p.to_string_lossy() != prefix)
                .unwrap_or(true)
        });
    }
    Ok(UnpackOutcome {
        files: sidecar.files,
        preserved,
    })
}

/// A non-clobbering sibling name for a diverged loose file:
/// "body (edited).stl", numbered when even that exists.
pub(crate) fn edited_aside_path(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let extension = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let parent = path.parent().unwrap_or(Path::new(""));
    let mut candidate = parent.join(format!("{} (edited){}", stem, extension));
    let mut n = 2;
    while candidate.exists() {
        candidate = parent.join(format!("{} (edited {}){}", stem, n, extension));
        n += 1;
    }
    candidate
}

/// Read the sidecar of a packed model dir, if there is one.
pub fn read_sidecar(model_dir: &Path) -> Result<Option<PackSidecar>, AppError> {
    let sidecar_path = model_dir.join(PACK_SIDECAR_NAME);
    if !sidecar_path.is_file() {
        return Ok(None);
    }
    let sidecar: PackSidecar = read_json(&sidecar_path)?;
    Ok(Some(sidecar))
}

/// What extract_paths_ephemeral wrote for one path. Cleanup deletes a file
/// only while its size+mtime still match this record — a file the slicer or
/// user overwrote is theirs now, never silently destroyed. In-memory on
/// purpose: if the app dies, the worst leftover is a loose file the scanner
/// indexes honestly (loose wins) and the next pack re-absorbs.
struct EphemeralRecord {
    size_bytes: u64,
    modified_at: i64,
    /// `blake3:<hex>` of what was extracted — the tie-breaker when the mtime
    /// disagrees or was unreadable (coarse SMB/FAT timestamps).
    checksum: String,
}

static EPHEMERAL_EXTRACTS: Lazy<Mutex<HashMap<String, EphemeralRecord>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

fn record_stat(metadata: &fs::Metadata, checksum: &str) -> EphemeralRecord {
    EphemeralRecord {
        size_bytes: metadata.len(),
        modified_at: metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
        checksum: checksum.to_string(),
    }
}

/// Materialize just the `wanted` paths from a packed model dir's archive
/// — the "print two files from a 40-file bundle" path. The archive and
/// sidecar stay untouched and authoritative; extracted files are tracked
/// for cleanup_ephemeral. Paths already on disk are skipped and NOT
/// recorded — a loose file we didn't create is not ours to delete. Cancel
/// rolls back this call's own extracts.
pub fn extract_paths_ephemeral(
    model_dir: &Path,
    wanted: &[String],
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(u32) -> bool,
) -> Result<Vec<String>, AppError> {
    let sidecar = read_sidecar(model_dir)?.ok_or_else(|| {
        AppError::NotFoundError(format!("'{}' is not packed (no pack.json)", model_dir.display()))
    })?;
    if !sidecar.is_readable() {
        return Err(AppError::InvalidInput(format!(
            "'{}' has an unreadable pack.json (format {} v{})",
            model_dir.display(),
            sidecar.format,
            sidecar.version
        )));
    }
    let archive_path = model_dir.join(PACK_ARCHIVE_NAME);
    let file = fs::File::open(&archive_path)
        .map_err(|e| AppError::IoError(format!("Failed to open {}: {}", archive_path.display(), e)))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| AppError::InvalidInput(format!("Not a readable archive: {}", e)))?;

    let by_name: HashMap<&str, &PackFileEntry> =
        sidecar.files.iter().map(|f| (f.name.as_str(), f)).collect();
    // first STORED entry per checksum = where an elided twin's bytes live
    let mut donor_by_checksum: HashMap<&str, &str> = HashMap::new();
    for entry in sidecar.files.iter().filter(|f| f.stored) {
        donor_by_checksum
            .entry(entry.checksum.as_str())
            .or_insert(entry.name.as_str());
    }

    let rollback = |paths: &[String]| {
        cleanup_ephemeral(paths);
    };
    let mut extracted: Vec<String> = Vec::new();
    for path_str in wanted {
        if cancel.load(Ordering::SeqCst) {
            rollback(&extracted);
            return Err(AppError::UserCancelled("Extraction cancelled".to_string()));
        }
        let path = Path::new(path_str);
        if path.exists() {
            continue;
        }
        let rel = path.strip_prefix(model_dir).map_err(|_| {
            AppError::InvalidInput(format!(
                "'{}' is not inside '{}'",
                path.display(),
                model_dir.display()
            ))
        })?;
        let name = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let Some(entry) = by_name.get(name.as_str()) else {
            rollback(&extracted);
            return Err(AppError::NotFoundError(format!(
                "'{}' is not in this model's pack archive",
                name
            )));
        };
        let source_name = if entry.stored {
            entry.name.as_str()
        } else {
            match donor_by_checksum.get(entry.checksum.as_str()) {
                Some(donor) => donor,
                None => {
                    rollback(&extracted);
                    return Err(AppError::InvalidInput(format!(
                        "Archive is missing '{}' and no twin with its checksum exists",
                        entry.name
                    )));
                }
            }
        };

        let temp = model_dir.join(temp_name("extract"));
        let mut write_one = || -> Result<(), AppError> {
            let mut zf = archive.by_name(source_name).map_err(|e| {
                AppError::FileProcessingError(format!("Archive entry '{}': {}", source_name, e))
            })?;
            let mut out = fs::File::create(&temp)?;
            std::io::copy(&mut zf, &mut out)
                .map_err(|e| AppError::IoError(format!("Failed extracting '{}': {}", name, e)))?;
            Ok(())
        };
        if let Err(e) = write_one() {
            fs::remove_file(&temp).ok();
            rollback(&extracted);
            return Err(e);
        }
        if let Err(e) = fs::rename(&temp, path) {
            fs::remove_file(&temp).ok();
            rollback(&extracted);
            return Err(AppError::IoError(format!(
                "Failed to place '{}': {}",
                path.display(),
                e
            )));
        }
        if let (Ok(metadata), Ok(mut registry)) = (fs::metadata(path), EPHEMERAL_EXTRACTS.lock()) {
            registry.insert(path_str.clone(), record_stat(&metadata, &entry.checksum));
        }
        extracted.push(path_str.clone());
        if !on_progress((entry.size_bytes / 1024) as u32) {
            rollback(&extracted);
            return Err(AppError::UserCancelled("Extraction cancelled".to_string()));
        }
    }
    Ok(extracted)
}

/// Delete ephemeral extracts — the requested `paths`, or everything the
/// registry holds when empty. Returns (removed, kept): a file whose size or
/// mtime drifted from its record was overwritten since extraction, so it's
/// kept on disk, reported, and dropped from the registry (it's user data
/// now, not our working copy).
pub fn cleanup_ephemeral(paths: &[String]) -> (Vec<String>, Vec<String>) {
    let Ok(mut registry) = EPHEMERAL_EXTRACTS.lock() else {
        return (Vec::new(), Vec::new());
    };
    let targets: Vec<String> = if paths.is_empty() {
        registry.keys().cloned().collect()
    } else {
        paths.to_vec()
    };
    let mut removed = Vec::new();
    let mut kept = Vec::new();
    for path in targets {
        let Some(record) = registry.get(&path) else {
            continue; // never ours, or already cleaned
        };
        match fs::metadata(&path) {
            Err(_) => {
                registry.remove(&path); // already gone
            }
            Ok(metadata) => {
                let fresh = record_stat(&metadata, &record.checksum);
                // size + mtime match proves it's still our extract; a
                // disagreeing/unreadable mtime falls back to the content
                // hash (coarse SMB/FAT timestamps must not orphan copies)
                let ours = fresh.size_bytes == record.size_bytes
                    && ((fresh.modified_at != 0 && fresh.modified_at == record.modified_at)
                        || manifest::hash_file(Path::new(&path))
                            .map(|h| h == record.checksum)
                            .unwrap_or(false));
                if ours {
                    if fs::remove_file(&path).is_ok() {
                        registry.remove(&path);
                        removed.push(path);
                    }
                    // a locked file (Windows: slicer still reading) stays
                    // registered — the exit sweep retries
                } else {
                    registry.remove(&path);
                    kept.push(path);
                }
            }
        }
    }
    (removed, kept)
}

/// App-exit sweep: this session's leftover extracts go away when the user
/// keeps cleanup-after on (the default). Best-effort by design — anything
/// that survives is just a loose file the catalog shows honestly.
pub fn sweep_ephemeral_on_exit() {
    let cleanup = crate::settings::SETTINGS_CACHE
        .lock()
        .ok()
        .and_then(|s| s.pack_cleanup_after)
        .unwrap_or(true);
    if cleanup {
        cleanup_ephemeral(&[]);
    }
}

/// Decompress every stored entry and compare against the checksum taken
/// from the source bytes — proof the archive round-trips before originals
/// are deleted. One extra full read of the model, by design: never trust
/// an unverified archive.
fn verify_archive(
    archive_path: &Path,
    entries: &[crate::file::compressors::ArchiveFileEntry],
    cancel: &AtomicBool,
    on_progress: &mut impl FnMut(PackPhase, u32) -> bool,
) -> Result<(), AppError> {
    let file = fs::File::open(archive_path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| AppError::FileProcessingError(format!("Pack verify failed to open: {}", e)))?;
    for entry in entries.iter().filter(|e| e.stored) {
        if cancel.load(Ordering::SeqCst) {
            return Err(AppError::UserCancelled("Pack cancelled".to_string()));
        }
        let mut zf = archive.by_name(&entry.name).map_err(|e| {
            AppError::FileProcessingError(format!("Pack is missing '{}': {}", entry.name, e))
        })?;
        let mut hasher = blake3::Hasher::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = zf.read(&mut buffer).map_err(|e| {
                AppError::FileProcessingError(format!(
                    "Pack verify failed reading '{}': {}",
                    entry.name, e
                ))
            })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        if format!("blake3:{}", hasher.finalize().to_hex()) != entry.checksum {
            return Err(AppError::FileProcessingError(format!(
                "Pack verification failed for '{}' — archive discarded, originals untouched",
                entry.name
            )));
        }
        if !on_progress(PackPhase::Verify, (entry.size_bytes / 1024) as u32) {
            return Err(AppError::UserCancelled("Pack cancelled".to_string()));
        }
    }
    Ok(())
}

/// Delete the loose originals the (verified) archive now owns. Deletion
/// requires proof the file is the one that was archived: size + mtime match,
/// or — when the mtime disagrees or is unreadable (rebuilt sidecars, coarse
/// SMB/FAT timestamps) — a content-hash match. Anything unproven is kept
/// and reported, never silently destroyed.
fn delete_packed_originals(
    model_dir: &Path,
    files: &[PackFileEntry],
) -> Result<Vec<String>, AppError> {
    let mut kept = Vec::new();
    for entry in files {
        let path = entry_disk_path(model_dir, &entry.name);
        let Ok(metadata) = fs::metadata(&path) else {
            continue; // already gone (e.g. resumed run)
        };
        let modified_at = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let size_matches = metadata.len() == entry.size_bytes;
        let proven = size_matches
            && ((modified_at != 0 && modified_at == entry.modified_at)
                || manifest::hash_file(&path)? == entry.checksum);
        if proven {
            fs::remove_file(&path)
                .map_err(|e| AppError::IoError(format!("Failed to remove {}: {}", path.display(), e)))?;
        } else {
            kept.push(path.to_string_lossy().into_owned());
        }
    }
    Ok(kept)
}

/// Remove leftover pack temps from crashed runs (any PID — a temp is only
/// ever live while its pack_model call is running, and packs are
/// serialized per model dir).
fn sweep_stale_temps(model_dir: &Path) {
    let Ok(read_dir) = fs::read_dir(model_dir) else {
        return;
    };
    for entry in read_dir.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if (name.starts_with(".plinth-pack-") || name.starts_with(".plinth-packjson-"))
            && name.ends_with(".tmp")
        {
            fs::remove_file(entry.path()).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("stlpack_pack_{}_{}", tag, std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn no_progress(_: PackPhase, _: u32) -> bool {
        true
    }

    /// A model dir with three files, two of them byte-identical so the
    /// archive's checksum dedup (stored=false) is exercised too.
    fn seed_model(dir: &Path) {
        fs::write(dir.join("body.stl"), b"unique-body-bytes-here").unwrap();
        fs::write(dir.join("sword.stl"), b"shared-arm-bytes!").unwrap();
        fs::write(dir.join("shield.stl"), b"shared-arm-bytes!").unwrap();
        // stays loose: not a model extension
        fs::write(dir.join("render.png"), b"not-a-real-png").unwrap();
    }

    #[test]
    fn pack_then_unpack_round_trips_bytes() {
        let dir = temp_dir("roundtrip");
        seed_model(&dir);
        let cancel = AtomicBool::new(false);

        let outcome = pack_model("0.0.0-test", &dir, None, &cancel, no_progress).unwrap();
        assert!(outcome.kept.is_empty());
        assert_eq!(outcome.sidecar.files.len(), 3);
        assert!(
            outcome.sidecar.files.iter().any(|f| !f.stored),
            "identical twins dedup inside the archive"
        );
        // model files gone, archive + sidecar + non-model files present
        assert!(!dir.join("body.stl").exists());
        assert!(!dir.join("sword.stl").exists());
        assert!(dir.join(PACK_ARCHIVE_NAME).is_file());
        assert!(dir.join(PACK_SIDECAR_NAME).is_file());
        assert!(dir.join("render.png").is_file(), "images stay loose");

        let outcome = unpack_model(&dir).unwrap();
        assert_eq!(outcome.files.len(), 3);
        assert!(outcome.preserved.is_empty());
        assert!(!dir.join(PACK_ARCHIVE_NAME).exists());
        assert!(!dir.join(PACK_SIDECAR_NAME).exists());
        assert_eq!(
            fs::read(dir.join("body.stl")).unwrap(),
            b"unique-body-bytes-here"
        );
        assert_eq!(
            fs::read(dir.join("sword.stl")).unwrap(),
            fs::read(dir.join("shield.stl")).unwrap(),
            "the elided twin rematerializes byte-identical"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn packed_model_rescans_from_sidecar_and_loose_wins() {
        let root = temp_dir("rescan");
        let model = root.join("Knight");
        fs::create_dir_all(&model).unwrap();
        seed_model(&model);
        let cancel = AtomicBool::new(false);

        pack_model("0.0.0-test", &model, None, &cancel, no_progress).unwrap();

        let outcome = super::super::scanner::scan(&root, &cancel, &[], |_, _| {}).unwrap();
        let packed_rows: Vec<_> = outcome
            .files
            .iter()
            .filter(|f| f.archive_path.is_some())
            .collect();
        assert_eq!(packed_rows.len(), 3, "every entry synthesized from pack.json");
        assert!(
            packed_rows.iter().all(|f| f.content_hash.is_some()),
            "pack checksums seed dup detection"
        );
        assert_eq!(outcome.packs.len(), 1);
        assert_eq!(
            outcome.models.len(),
            1,
            "the packed dir still assembles as one model"
        );
        assert_eq!(outcome.models[0].file_count, 3);

        // loose wins: a file materialized on disk (ephemeral extract /
        // crash-mid-pack) is indexed as loose, not from the sidecar
        fs::write(model.join("body.stl"), b"unique-body-bytes-here").unwrap();
        let outcome = super::super::scanner::scan(&root, &cancel, &[], |_, _| {}).unwrap();
        let body = outcome
            .files
            .iter()
            .find(|f| f.file_name == "body.stl")
            .unwrap();
        assert!(body.archive_path.is_none(), "walked file beats pack entry");
        assert_eq!(
            outcome.files.iter().filter(|f| f.file_name == "body.stl").count(),
            1,
            "no duplicate row for the same path"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rerunning_pack_finishes_deletes_but_keeps_changed_files() {
        let dir = temp_dir("repair");
        seed_model(&dir);
        let cancel = AtomicBool::new(false);

        pack_model("0.0.0-test", &dir, None, &cancel, no_progress).unwrap();
        // Simulate a crash that left one original behind — and the user (or
        // slicer) has since CHANGED it, so the repair pass must not delete it
        fs::write(dir.join("body.stl"), b"edited-after-packing-so-longer").unwrap();

        let outcome = pack_model("0.0.0-test", &dir, None, &cancel, no_progress).unwrap();
        assert_eq!(outcome.kept.len(), 1);
        assert!(outcome.kept[0].ends_with("body.stl"));
        assert!(
            dir.join("body.stl").is_file(),
            "changed file survives the delete pass"
        );
        assert!(dir.join(PACK_ARCHIVE_NAME).is_file(), "archive untouched");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn orphan_archive_without_sidecar_is_discarded_and_repacked() {
        let dir = temp_dir("orphan");
        seed_model(&dir);
        // a crash between the archive and sidecar renames leaves this
        fs::write(dir.join(PACK_ARCHIVE_NAME), b"garbage-not-a-zip").unwrap();
        let cancel = AtomicBool::new(false);

        let outcome = pack_model("0.0.0-test", &dir, None, &cancel, no_progress).unwrap();
        assert_eq!(outcome.sidecar.files.len(), 3);
        assert!(dir.join(PACK_SIDECAR_NAME).is_file());
        // the fresh archive is real: unpack restores everything
        unpack_model(&dir).unwrap();
        assert!(dir.join("body.stl").is_file());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ephemeral_extract_materializes_only_what_was_asked() {
        let dir = temp_dir("ephemeral");
        seed_model(&dir);
        let cancel = AtomicBool::new(false);
        pack_model("0.0.0-test", &dir, None, &cancel, no_progress).unwrap();

        // ask for the elided twin specifically: bytes must come from its
        // stored checksum donor without materializing the donor itself
        let shield = dir.join("shield.stl").to_string_lossy().into_owned();
        let extracted =
            extract_paths_ephemeral(&dir, std::slice::from_ref(&shield), &cancel, |_| true)
                .unwrap();
        assert_eq!(extracted, vec![shield.clone()]);
        assert_eq!(fs::read(dir.join("shield.stl")).unwrap(), b"shared-arm-bytes!");
        assert!(!dir.join("body.stl").exists(), "unrequested files stay packed");
        assert!(!dir.join("sword.stl").exists(), "the donor is not materialized");
        assert!(dir.join(PACK_ARCHIVE_NAME).is_file(), "archive stays authoritative");
        assert!(dir.join(PACK_SIDECAR_NAME).is_file());

        // untouched extract cleans up; nothing else is harmed
        let (removed, kept) = cleanup_ephemeral(std::slice::from_ref(&shield));
        assert_eq!(removed, vec![shield]);
        assert!(kept.is_empty());
        assert!(!dir.join("shield.stl").exists());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cleanup_keeps_files_that_changed_since_extraction() {
        let dir = temp_dir("ephemeral_kept");
        seed_model(&dir);
        let cancel = AtomicBool::new(false);
        pack_model("0.0.0-test", &dir, None, &cancel, no_progress).unwrap();

        let body = dir.join("body.stl").to_string_lossy().into_owned();
        extract_paths_ephemeral(&dir, std::slice::from_ref(&body), &cancel, |_| true).unwrap();
        // the slicer saved supports over our working copy
        fs::write(dir.join("body.stl"), b"user-edited-bytes-now-much-longer").unwrap();

        let (removed, kept) = cleanup_ephemeral(std::slice::from_ref(&body));
        assert!(removed.is_empty());
        assert_eq!(kept, vec![body.clone()]);
        assert!(dir.join("body.stl").is_file(), "changed file survives");
        // dropped from the registry: a second cleanup won't touch it either
        let (removed, kept) = cleanup_ephemeral(&[body]);
        assert!(removed.is_empty() && kept.is_empty());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn full_unpack_forgets_ephemeral_records_for_the_dir() {
        let dir = temp_dir("ephemeral_unpack");
        seed_model(&dir);
        let cancel = AtomicBool::new(false);
        pack_model("0.0.0-test", &dir, None, &cancel, no_progress).unwrap();

        let body = dir.join("body.stl").to_string_lossy().into_owned();
        extract_paths_ephemeral(&dir, std::slice::from_ref(&body), &cancel, |_| true).unwrap();
        unpack_model(&dir).unwrap();

        // the file is permanent now — cleanup must not delete it
        let (removed, kept) = cleanup_ephemeral(&[body]);
        assert!(removed.is_empty() && kept.is_empty());
        assert!(dir.join("body.stl").is_file());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unpack_preserves_a_diverged_loose_file_as_edited() {
        let dir = temp_dir("unpack_preserve");
        seed_model(&dir);
        let cancel = AtomicBool::new(false);
        pack_model("0.0.0-test", &dir, None, &cancel, no_progress).unwrap();
        // the user's newer version of a packed file (e.g. slicer-saved
        // supports written over an ephemeral extract that cleanup kept)
        fs::write(dir.join("body.stl"), b"user-edited-newer-bytes!!").unwrap();

        let outcome = unpack_model(&dir).unwrap();
        assert_eq!(outcome.preserved.len(), 1);
        assert!(outcome.preserved[0].ends_with("body (edited).stl"));
        assert_eq!(
            fs::read(dir.join("body (edited).stl")).unwrap(),
            b"user-edited-newer-bytes!!",
            "the user's newer bytes survive, moved aside"
        );
        assert_eq!(
            fs::read(dir.join("body.stl")).unwrap(),
            b"unique-body-bytes-here",
            "the archived version extracts under the original name"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lost_sidecar_with_stray_loose_file_never_discards_the_archive() {
        let dir = temp_dir("lost_sidecar");
        seed_model(&dir);
        let cancel = AtomicBool::new(false);
        pack_model("0.0.0-test", &dir, None, &cancel, no_progress).unwrap();
        // the damage scenario: pack.json lost while ONE stray loose model
        // file exists — the archive holds the only copy of everything else
        fs::remove_file(dir.join(PACK_SIDECAR_NAME)).unwrap();
        fs::write(dir.join("stray.stl"), b"user-dropped-this-in").unwrap();

        let outcome = pack_model("0.0.0-test", &dir, None, &cancel, no_progress).unwrap();
        // archive survives, sidecar is rebuilt from it, the stray stays loose
        assert!(dir.join(PACK_ARCHIVE_NAME).is_file(), "archive never deleted");
        assert!(dir.join(PACK_SIDECAR_NAME).is_file(), "sidecar rebuilt");
        assert!(dir.join("stray.stl").is_file(), "stray file untouched");
        assert_eq!(outcome.sidecar.generator, "plinth/sidecar-rebuild");
        // and the archive still round-trips its contents
        let unpacked = unpack_model(&dir).unwrap();
        assert_eq!(unpacked.files.len(), 2, "stored entries recovered");
        assert_eq!(
            fs::read(dir.join("body.stl")).unwrap(),
            b"unique-body-bytes-here"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn true_crash_orphan_with_complete_loose_set_is_discarded_and_repacked() {
        let dir = temp_dir("true_orphan");
        seed_model(&dir);
        let cancel = AtomicBool::new(false);
        // build a REAL archive of the same files (crash after archive
        // rename, before sidecar rename: loose set complete)
        pack_model("0.0.0-test", &dir, None, &cancel, no_progress).unwrap();
        let unpacked = unpack_model(&dir).unwrap();
        assert!(unpacked.preserved.is_empty());
        // recreate the crash state: archive present, no sidecar, loose complete
        pack_model("0.0.0-test", &dir, None, &cancel, no_progress).unwrap();
        let archive_bytes = fs::read(dir.join(PACK_ARCHIVE_NAME)).unwrap();
        let outcome = unpack_model(&dir).unwrap();
        assert!(outcome.preserved.is_empty());
        fs::write(dir.join(PACK_ARCHIVE_NAME), archive_bytes).unwrap();

        let outcome = pack_model("0.0.0-test", &dir, None, &cancel, no_progress).unwrap();
        assert_ne!(outcome.sidecar.generator, "plinth/sidecar-rebuild");
        assert_eq!(outcome.sidecar.files.len(), 3, "fresh pack of the loose set");
        assert!(!dir.join("body.stl").exists(), "originals deleted after repack");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scanner_rebuilds_a_lost_sidecar_from_the_archive() {
        let root = temp_dir("selfheal");
        let model = root.join("Knight");
        fs::create_dir_all(&model).unwrap();
        seed_model(&model);
        let cancel = AtomicBool::new(false);
        pack_model("0.0.0-test", &model, None, &cancel, no_progress).unwrap();
        fs::remove_file(model.join(PACK_SIDECAR_NAME)).unwrap();

        let outcome = super::super::scanner::scan(&root, &cancel, &[], |_, _| {}).unwrap();
        // the elided twin's name lived only in the lost sidecar — the rebuild
        // recovers the two stored entries, not the third name
        assert_eq!(outcome.packs.len(), 1, "model healed back into the catalog");
        assert_eq!(outcome.models.len(), 1);
        assert_eq!(outcome.models[0].file_count, 2);
        let healed = read_sidecar(&model).unwrap().expect("sidecar rewritten to disk");
        assert_eq!(healed.generator, "plinth/sidecar-rebuild");
        assert!(healed.files.iter().all(|f| f.stored));

        // and the healed pack still round-trips
        unpack_model(&model).unwrap();
        assert_eq!(fs::read(model.join("body.stl")).unwrap(), b"unique-body-bytes-here");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn crash_orphan_with_loose_files_is_not_healed() {
        let root = temp_dir("selfheal_orphan");
        let model = root.join("Knight");
        fs::create_dir_all(&model).unwrap();
        seed_model(&model);
        // crash between the archive and sidecar renames: archive + all
        // loose files, no pack.json — pack_model's discard path owns this
        fs::write(model.join(PACK_ARCHIVE_NAME), b"garbage-not-a-zip").unwrap();
        let cancel = AtomicBool::new(false);

        let outcome = super::super::scanner::scan(&root, &cancel, &[], |_, _| {}).unwrap();
        assert!(outcome.packs.is_empty(), "loose files veto the rebuild");
        assert!(
            !model.join(PACK_SIDECAR_NAME).exists(),
            "no sidecar materializes for the orphan"
        );
        assert_eq!(outcome.models[0].file_count, 3, "the loose files are the model");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn archive_without_sidecar_or_loose_files_is_refused() {
        let dir = temp_dir("unverifiable");
        fs::write(dir.join(PACK_ARCHIVE_NAME), b"who-knows-whats-in-here").unwrap();
        let cancel = AtomicBool::new(false);

        let err = pack_model("0.0.0-test", &dir, None, &cancel, no_progress).unwrap_err();
        assert!(matches!(err, AppError::InvalidInput(_)));
        assert!(
            dir.join(PACK_ARCHIVE_NAME).is_file(),
            "nothing gets deleted when we can't verify"
        );

        fs::remove_dir_all(&dir).ok();
    }
}
