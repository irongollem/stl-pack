//! Import a packed release: read `manifest.json` out of `release.3pk`,
//! verify each sibling component archive against its BLAKE3 checksum, and
//! extract everything into a library folder — rematerializing names a
//! deduplicated archive elided (hardlink where the volume supports it).
//! The extracted tree matches what the builder staged, so a normal catalog
//! scan restores the packed curation via the model.json sidecars.
//!
//! Because the import writes the manifest into the release dir, a SECOND
//! import of the same release becomes an UPDATE: `inspect_package` diffs
//! the incoming component checksums against the local manifest, and
//! `import_release` re-extracts just the selected components — moving
//! locally edited files aside instead of truncating them (the same
//! contract unpack_model honors) and deleting files the new version
//! dropped.

use crate::catalog::db;
use crate::catalog::layout;
use crate::catalog::pack::{self, edited_aside_path, PACK_SIDECAR_NAME};
use crate::error::AppError;
use crate::manifest::{self, Component, Manifest, ManifestFile};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::sync::atomic::AtomicBool;

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct ImportOutcome {
    pub release_name: String,
    pub designer: String,
    /// The directory the release landed in.
    pub dest_dir: String,
    /// True when the release already existed and this run updated it.
    pub updated: bool,
    pub components: u32,
    pub files: u32,
    /// Per-component problems (checksum mismatch, missing archive, packed at
    /// rest); the rest of the release still imports.
    pub errors: Vec<String>,
    /// Non-fatal notes, e.g. locally edited files kept aside as "(edited)".
    pub warnings: Vec<String>,
}

/// How one incoming component compares to what the library already holds.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum ComponentState {
    /// Not in the local release (or the release isn't imported yet).
    New,
    /// Local manifest lists a different archive checksum — an update.
    Changed,
    /// Same checksum both sides; nothing to do.
    Unchanged,
    /// The local copy is packed at rest — unpack before updating.
    Packed,
    /// The component archive isn't next to the .3pk; can't import.
    MissingArchive,
}

/// One manifest file this library doesn't own anywhere by checksum.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct MissingFile {
    pub name: String,
    pub size_bytes: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct ComponentStatus {
    pub name: String,
    pub state: ComponentState,
    /// f64 (not the manifest's u64): specta refuses integer types TypeScript
    /// numbers can't hold, same convention as the catalog's size fields.
    pub size_bytes: f64,
    pub file_count: u32,
    /// Display names of the models inside (custom name when set).
    pub model_names: Vec<String>,
    /// Why the component can't be imported, for Packed/MissingArchive.
    pub detail: Option<String>,
    /// How many of `file_count` this library already owns SOMEWHERE, by
    /// checksum — independent of `state`: even a MissingArchive component's
    /// checksums are known from the manifest itself.
    pub files_owned: u32,
    pub missing_bytes: f64,
    pub missing: Vec<MissingFile>,
}

/// What opening a `release.3pk` would do — feeds the selective-import dialog
/// before anything touches the disk.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct PackageInspection {
    pub release_name: String,
    pub designer: String,
    pub date: String,
    pub version: String,
    pub dest_dir: String,
    /// The release dir already holds a manifest — importing means updating.
    pub is_update: bool,
    /// Set when the destination exists but wasn't written by an import (no
    /// readable manifest.json) — importing is refused rather than guessed at.
    pub blocked: Option<String>,
    pub components: Vec<ComponentStatus>,
}

/// Read `manifest.json` from inside a `release.3pk`.
pub fn read_manifest(package_path: &Path) -> Result<Manifest, AppError> {
    let file = std::fs::File::open(package_path)
        .map_err(|e| AppError::IoError(format!("Cannot open {}: {}", package_path.display(), e)))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| AppError::InvalidInput(format!("Not a readable package: {}", e)))?;
    let mut entry = archive.by_name("manifest.json").map_err(|_| {
        AppError::InvalidInput(
            "No manifest.json inside — this package predates the 3pk manifest".into(),
        )
    })?;
    let mut text = String::new();
    entry
        .read_to_string(&mut text)
        .map_err(|e| AppError::IoError(format!("Failed to read manifest: {}", e)))?;
    let manifest = Manifest::from_json(&text)?;
    if !manifest.is_readable() {
        return Err(AppError::InvalidInput(format!(
            "This package uses 3pk format v{} — this app reads v{}",
            manifest.version,
            manifest::VERSION
        )));
    }
    Ok(manifest)
}

/// The manifest a previous import left in the release dir, if any. Unreadable
/// or foreign-major manifests read as None — the caller then refuses to treat
/// the directory as updatable rather than guessing at its contents.
fn read_local_manifest(dest: &Path) -> Option<Manifest> {
    let text = std::fs::read_to_string(dest.join("manifest.json")).ok()?;
    Manifest::from_json(&text).ok().filter(|m| m.is_readable())
}

/// A manifest-relative name that stays inside its component dir. Hostile
/// names read as None and are ignored wholesale: `is_absolute()` catches
/// POSIX-absolute and `C:\x`, `ParentDir` catches `../x`, and `Prefix`
/// catches what `is_absolute()` misses on Windows — a drive-relative
/// `C:foo` or a `\\?\`/UNC path. `PathBuf::join` treats any `Prefix`
/// component as a full replacement of the base, not an append, so letting
/// one through would silently retarget the whole destination.
///
/// `Component::Prefix` only appears when the *build* targets Windows — on
/// a non-Windows host, `Path::new("C:evil.stl")` parses as a harmless
/// `Normal` component, so the drive prefix and any backslash are also
/// checked textually, so the guard holds no matter which OS built it.
pub(crate) fn safe_relative(name: &str) -> Option<&Path> {
    let bytes = name.as_bytes();
    let has_drive_prefix =
        bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if name.contains('\\') || has_drive_prefix {
        return None;
    }
    let path = Path::new(name);
    if path.is_absolute()
        || path.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::Prefix(_)
            )
        })
    {
        return None;
    }
    Some(path)
}

fn contains_pack_sidecar(dir: &Path) -> bool {
    dir.is_dir()
        && walkdir::WalkDir::new(dir)
            .into_iter()
            .flatten()
            .any(|e| e.file_name() == PACK_SIDECAR_NAME)
}

fn component_file_count(component: &Component) -> u32 {
    component.models.iter().map(|m| m.files.len() as u32).sum()
}

/// Diff one component's manifest files against library-wide ownership —
/// `find_owner` already covers packed-at-rest donors, so this never has to
/// unpack anything. Independent of the component's archive-based `state`:
/// even a MissingArchive component's checksums are known from the manifest.
fn component_completeness(
    conn: &Connection,
    component: &Component,
) -> Result<(u32, f64, Vec<MissingFile>), AppError> {
    let mut owned = 0u32;
    let mut missing_bytes = 0f64;
    let mut missing = Vec::new();
    for file in component.models.iter().flat_map(|m| &m.files) {
        if db::find_owner(conn, pack::bare_hash(&file.checksum))?.is_some() {
            owned += 1;
        } else {
            missing_bytes += file.size_bytes as f64;
            missing.push(MissingFile {
                name: file.name.clone(),
                size_bytes: file.size_bytes as f64,
            });
        }
    }
    Ok((owned, missing_bytes, missing))
}

/// Diff an incoming `release.3pk` against the library without touching disk.
pub fn inspect_package(
    package_path: &Path,
    library_dir: &Path,
    conn: &Connection,
) -> Result<PackageInspection, AppError> {
    let manifest = read_manifest(package_path)?;
    let package_dir = package_path
        .parent()
        .ok_or_else(|| AppError::InvalidInput("Package has no parent directory".into()))?;
    let dest = layout::release_dir(
        library_dir,
        &manifest.release.designer,
        &manifest.release.name,
        Some(&manifest.release.date),
    );
    let local = read_local_manifest(&dest);
    let blocked = if dest.exists() && local.is_none() {
        Some(format!(
            "'{}' already exists but wasn't imported by this app (no readable manifest.json) — remove it first to re-import",
            dest.display()
        ))
    } else {
        None
    };
    let old_by_name: HashMap<&str, &Component> = local
        .as_ref()
        .map(|m| m.components.iter().map(|c| (c.name.as_str(), c)).collect())
        .unwrap_or_default();

    let mut components = Vec::with_capacity(manifest.components.len());
    for component in &manifest.components {
        let component_dest = dest.join(layout::sanitize_segment(&component.name));
        // component.archive is attacker-authorable manifest text; reject
        // before it ever becomes a path so a value like "../../secrets"
        // or "C:evil.zip" can't be opened/hashed/extracted from outside
        // package_dir. Reported through the same MissingArchive state a
        // legitimate absent sibling gets — the outcome for the UI is
        // identical ("can't import this component").
        let (state, detail) = if safe_relative(&component.archive).is_none() {
            (
                ComponentState::MissingArchive,
                Some(format!(
                    "'{}' is not a safe archive path — refusing to import",
                    component.archive
                )),
            )
        } else if !package_dir.join(&component.archive).is_file() {
            (
                ComponentState::MissingArchive,
                Some(format!(
                    "'{}' was not found next to the .3pk",
                    component.archive
                )),
            )
        } else if local.is_some() && contains_pack_sidecar(&component_dest) {
            (
                ComponentState::Packed,
                Some("packed at rest — unpack it in the catalog first".into()),
            )
        } else {
            match old_by_name.get(component.name.as_str()) {
                None => (ComponentState::New, None),
                Some(old) if old.checksum == component.checksum => {
                    (ComponentState::Unchanged, None)
                }
                Some(_) => (ComponentState::Changed, None),
            }
        };
        let (files_owned, missing_bytes, missing) = component_completeness(conn, component)?;
        components.push(ComponentStatus {
            name: component.name.clone(),
            state,
            size_bytes: component.size_bytes as f64,
            file_count: component_file_count(component),
            model_names: component
                .models
                .iter()
                .map(|m| m.custom_name.clone().unwrap_or_else(|| m.name.clone()))
                .collect(),
            detail,
            files_owned,
            missing_bytes,
            missing,
        });
    }

    Ok(PackageInspection {
        release_name: manifest.release.name,
        designer: manifest.release.designer,
        date: manifest.release.date,
        version: manifest.release.version,
        dest_dir: dest.to_string_lossy().into_owned(),
        is_update: local.is_some(),
        blocked,
        components,
    })
}

/// Extract everything in `release.3pk` EXCEPT manifest.json — the manifest is
/// written last from what actually imported, so a component that failed this
/// run still reads as pending on the next inspect instead of "unchanged".
fn extract_release_payload(package_path: &Path, dest: &Path) -> Result<(), AppError> {
    let file = std::fs::File::open(package_path)
        .map_err(|e| AppError::IoError(format!("Cannot open {}: {}", package_path.display(), e)))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| AppError::InvalidInput(format!("Not a readable package: {}", e)))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| AppError::IoError(format!("Failed to read package entry: {}", e)))?;
        // enclosed_name is the zip crate's traversal guard; hostile entries skip
        let Some(rel) = entry.enclosed_name().map(|p| p.to_owned()) else {
            continue;
        };
        if rel == Path::new("manifest.json") {
            continue;
        }
        let out = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)
                .map_err(|e| AppError::IoError(format!("Failed to create dirs: {}", e)))?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AppError::IoError(format!("Failed to create dirs: {}", e)))?;
        }
        let mut out_file = std::fs::File::create(&out)
            .map_err(|e| AppError::IoError(format!("Failed to write {}: {}", out.display(), e)))?;
        std::io::copy(&mut entry, &mut out_file)
            .map_err(|e| AppError::IoError(format!("Failed to write {}: {}", out.display(), e)))?;
    }
    Ok(())
}

/// Before an update overwrites a component, move aside any file the user
/// edited since the last import — a file whose bytes match neither what the
/// last import wrote (old checksum) nor what this one is about to write.
/// Slicer-saved supports survive an update; same rule as unpack_model.
fn preserve_local_edits(
    component_dest: &Path,
    old: &Component,
    new_files: &[ManifestFile],
) -> Result<Vec<String>, AppError> {
    let mut warnings = Vec::new();
    if !component_dest.exists() {
        return Ok(warnings);
    }
    let incoming: HashMap<&str, &str> = new_files
        .iter()
        .map(|f| (f.name.as_str(), f.checksum.as_str()))
        .collect();
    for file in old.models.iter().flat_map(|m| &m.files) {
        let Some(rel) = safe_relative(&file.name) else {
            continue;
        };
        let path = component_dest.join(rel);
        if !path.is_file() {
            continue;
        }
        let actual = manifest::hash_file(&path)?;
        if actual == file.checksum || incoming.get(file.name.as_str()).copied() == Some(&actual) {
            continue;
        }
        let aside = edited_aside_path(&path);
        std::fs::rename(&path, &aside).map_err(|e| {
            AppError::IoError(format!("Failed to preserve edited '{}': {}", file.name, e))
        })?;
        warnings.push(format!(
            "'{}' was edited locally — your copy was kept as '{}'",
            file.name,
            aside.file_name().unwrap_or_default().to_string_lossy()
        ));
    }
    Ok(warnings)
}

/// Delete files the previous import wrote that the incoming manifest no
/// longer lists (renames/removals in the new version), then sweep emptied
/// dirs. Edited copies were already moved aside, and names the user added
/// himself were never in the old manifest — both survive.
fn remove_stale_files(component_dest: &Path, old: &Component, new_files: &[ManifestFile]) {
    let keep: HashSet<&str> = new_files.iter().map(|f| f.name.as_str()).collect();
    for file in old.models.iter().flat_map(|m| &m.files) {
        if keep.contains(file.name.as_str()) {
            continue;
        }
        if let Some(rel) = safe_relative(&file.name) {
            let _ = std::fs::remove_file(component_dest.join(rel));
        }
    }
    for entry in walkdir::WalkDir::new(component_dest)
        .contents_first(true)
        .into_iter()
        .flatten()
    {
        if entry.file_type().is_dir() && entry.path() != component_dest {
            // remove_dir refuses non-empty dirs, so this only sweeps husks
            let _ = std::fs::remove_dir(entry.path());
        }
    }
}

/// Distinguishes a component import failure `recompile_release` can route
/// around from one it can't: a bad/missing/unsafe archive just means the
/// component needs its bytes from somewhere else (the library), but a
/// packed-at-rest component already exists on disk, just compressed —
/// writing donor files next to it would corrupt that state, not complete it.
enum ImportFailure {
    Blocked(String),
    Retryable(String),
}

impl std::fmt::Display for ImportFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Blocked(m) | Self::Retryable(m) => write!(f, "{}", m),
        }
    }
}

impl From<AppError> for ImportFailure {
    fn from(e: AppError) -> Self {
        Self::Retryable(e.to_string())
    }
}

/// The normal per-component import: verify the sibling archive's checksum,
/// extract it (rematerializing any dedup-elided names), preserve local
/// edits and drop stale files against `old` when this is an update. Shared
/// by `import_release` and `recompile_release`, which only reaches for
/// library donors when this fails.
fn import_component(
    package_dir: &Path,
    component: &Component,
    component_dest: &Path,
    old: Option<&Component>,
    updating: bool,
    warnings: &mut Vec<String>,
) -> Result<u32, ImportFailure> {
    if updating && contains_pack_sidecar(component_dest) {
        return Err(ImportFailure::Blocked(
            "packed at rest — unpack it in the catalog first, then update".into(),
        ));
    }
    // Same guard as inspect_package: an attacker-authored archive name must
    // not resolve outside package_dir before we open it.
    if safe_relative(&component.archive).is_none() {
        return Err(ImportFailure::Retryable(format!(
            "archive '{}' is not a safe path — refusing to import",
            component.archive
        )));
    }
    let archive_path = package_dir.join(&component.archive);
    if !archive_path.is_file() {
        return Err(ImportFailure::Retryable(format!(
            "archive '{}' is missing",
            component.archive
        )));
    }
    // The checksum is the integrity promise of the format — a truncated
    // download or bit-rot surfaces here, not as broken STLs later
    let actual = manifest::hash_file(&archive_path)?;
    if actual != component.checksum {
        return Err(ImportFailure::Retryable(
            "checksum mismatch — the archive is corrupted or was modified".into(),
        ));
    }
    let manifest_files: Vec<ManifestFile> = component
        .models
        .iter()
        .flat_map(|m| m.files.iter().cloned())
        .collect();
    if let Some(old) = old {
        warnings.extend(preserve_local_edits(component_dest, old, &manifest_files)?);
    }
    // sanitize_segment (in component_dest): idempotent for our own packages
    // and stops a hostile component name ("../x") from landing outside the
    // release dir
    manifest::extract_component_archive(&archive_path, component_dest, &manifest_files)?;
    if let Some(old) = old {
        remove_stale_files(component_dest, old, &manifest_files);
    }
    Ok(manifest_files.len() as u32)
}

/// Import (or update) a packed release. `selection` limits the run to the
/// named components — None imports everything, the pre-dialog behavior.
pub fn import_release(
    package_path: &Path,
    library_dir: &Path,
    selection: Option<Vec<String>>,
) -> Result<ImportOutcome, AppError> {
    let manifest = read_manifest(package_path)?;
    let package_dir = package_path
        .parent()
        .ok_or_else(|| AppError::InvalidInput("Package has no parent directory".into()))?;

    // Land at the CANONICAL library spot — Designer/YYYY-MM Release — so an
    // imported release drops into the catalog already normal-form and the
    // normalizer has nothing to move. The manifest date is already the
    // sortable YYYY-MM the release segment wants.
    let dest = layout::release_dir(
        library_dir,
        &manifest.release.designer,
        &manifest.release.name,
        Some(&manifest.release.date),
    );
    let local = read_local_manifest(&dest);
    if dest.exists() && local.is_none() {
        return Err(AppError::InvalidInput(format!(
            "'{}' already exists — remove it first to re-import",
            dest.display()
        )));
    }
    let updating = local.is_some();
    let old_components: Vec<Component> = local.map(|m| m.components).unwrap_or_default();
    let old_by_name: HashMap<&str, &Component> = old_components
        .iter()
        .map(|c| (c.name.as_str(), c))
        .collect();
    std::fs::create_dir_all(&dest)
        .map_err(|e| AppError::IoError(format!("Failed to create release dir: {}", e)))?;

    // Release-level payload (images, release.json — the manifest comes last)
    extract_release_payload(package_path, &dest)?;

    let selected = |name: &str| {
        selection
            .as_ref()
            .is_none_or(|s| s.iter().any(|n| n == name))
    };
    let mut succeeded: HashSet<&str> = HashSet::new();
    let mut components = 0u32;
    let mut files = 0u32;
    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    for component in &manifest.components {
        if !selected(&component.name) {
            continue;
        }
        let component_dest = dest.join(layout::sanitize_segment(&component.name));
        let old = old_by_name.get(component.name.as_str()).copied();
        // Errors stay per-component strings (shown verbatim in the UI); the
        // rest of the release still imports.
        match import_component(package_dir, component, &component_dest, old, updating, &mut warnings) {
            Ok(count) => {
                succeeded.insert(component.name.as_str());
                components += 1;
                files += count;
            }
            Err(e) => errors.push(format!("{}: {}", component.name, e)),
        }
    }

    // The written manifest records what is ACTUALLY on disk: new entries for
    // components that imported, the previous entry for ones that failed or
    // were deselected (they still hold the old files), old-only components
    // appended. Update detection stays truthful across partial runs.
    let new_names: HashSet<&str> = manifest
        .components
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    let mut merged: Vec<Component> = manifest
        .components
        .iter()
        .filter_map(|c| {
            if succeeded.contains(c.name.as_str()) {
                Some(c.clone())
            } else {
                old_by_name.get(c.name.as_str()).map(|old| (*old).clone())
            }
        })
        .collect();
    merged.extend(
        old_components
            .iter()
            .filter(|old| !new_names.contains(old.name.as_str()))
            .cloned(),
    );
    let mut final_manifest = manifest.clone();
    final_manifest.components = merged;
    std::fs::write(dest.join("manifest.json"), final_manifest.to_json()?)
        .map_err(|e| AppError::IoError(format!("Failed to write manifest: {}", e)))?;

    Ok(ImportOutcome {
        release_name: manifest.release.name.clone(),
        designer: manifest.release.designer.clone(),
        dest_dir: dest.to_string_lossy().into_owned(),
        updated: updating,
        components,
        files,
        errors,
        warnings,
    })
}

/// Bytes at `path` hash to `checksum` (manifest `blake3:<hex>` form).
fn file_matches(path: &Path, checksum: &str) -> bool {
    manifest::hash_file(path)
        .map(|h| h == checksum)
        .unwrap_or(false)
}

/// Copy/hardlink ONE owned file into place, extracting an ephemeral copy
/// first when its only known donor is packed at rest (cleaned up right
/// after). Verifies the result against `checksum` before reporting success
/// and removes it on mismatch — the index can go stale between a scan and
/// this call, and a silent bad copy would be worse than an honest "still
/// missing".
fn materialize_donor(conn: &Connection, checksum: &str, target: &Path) -> Result<bool, AppError> {
    let Some((donor_path, donor_archive)) = db::find_owner(conn, pack::bare_hash(checksum))? else {
        return Ok(false);
    };
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AppError::IoError(format!("Failed to create dirs: {}", e)))?;
    }
    if let Some(archive_path) = &donor_archive {
        let Some(model_dir) = Path::new(archive_path).parent() else {
            return Ok(false);
        };
        let cancel = AtomicBool::new(false);
        let wanted = [donor_path.clone()];
        if pack::extract_paths_ephemeral(model_dir, &wanted, &cancel, |_| true).is_err() {
            return Ok(false);
        }
    }
    let source = Path::new(&donor_path);
    let copied = std::fs::hard_link(source, target).is_ok() || std::fs::copy(source, target).is_ok();
    if donor_archive.is_some() {
        pack::cleanup_ephemeral(std::slice::from_ref(&donor_path));
    }
    let ok = copied && file_matches(target, checksum);
    if copied && !ok {
        std::fs::remove_file(target).ok();
    }
    Ok(ok)
}

/// Materialize the manifest files the library already owns into
/// `component_dest`, independent of whether the release's own archive is
/// present — the "recompile a partial set" counterpart to
/// `import_component`'s archive-based path, reached when that one can't
/// run. Draws donors from anywhere in the catalog by content_hash, not just
/// this component's own archive; a file with no owner anywhere is left
/// missing rather than erroring the whole component out. Files a previous,
/// interrupted run already landed count as owned without re-fetching a
/// donor, so a rerun after acquiring the rest resumes for free.
fn materialize_owned_files(
    conn: &Connection,
    component_dest: &Path,
    files: &[ManifestFile],
) -> Result<(u32, u32, f64), AppError> {
    std::fs::create_dir_all(component_dest)
        .map_err(|e| AppError::IoError(format!("Failed to create {}: {}", component_dest.display(), e)))?;
    let mut landed = 0u32;
    let mut missing = 0u32;
    let mut missing_bytes = 0f64;
    for file in files {
        let Some(rel) = safe_relative(&file.name) else {
            missing += 1;
            missing_bytes += file.size_bytes as f64;
            continue;
        };
        let target = component_dest.join(rel);
        if target.is_file() {
            if file_matches(&target, &file.checksum) {
                landed += 1;
                continue;
            }
            // Stale bytes from an older version at this path — the donor
            // lookup below may still supply the current release's bytes.
            std::fs::remove_file(&target).ok();
        }
        if materialize_donor(conn, &file.checksum, &target)? {
            landed += 1;
        } else {
            missing += 1;
            missing_bytes += file.size_bytes as f64;
        }
    }
    Ok((landed, missing, missing_bytes))
}

/// One component's outcome from `recompile_release`.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct RecompiledComponent {
    pub name: String,
    /// True when every manifest file for this component landed on disk —
    /// via the archive or via library donors. `files_landed` can be > 0
    /// even when this is false: "not complete" isn't "nothing happened".
    pub complete: bool,
    pub files_landed: u32,
    pub files_missing: u32,
    pub missing_bytes: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct RecompileOutcome {
    pub release_name: String,
    pub designer: String,
    pub dest_dir: String,
    pub components: Vec<RecompiledComponent>,
    /// Components a library donor couldn't help either (e.g. packed at
    /// rest).
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// "Import what you own": for each selected component, extract the normal
/// way when its sibling archive checks out (`import_component`), otherwise
/// materialize whatever the library already holds by checksum
/// (`materialize_owned_files`) — including a component whose archive isn't
/// present next to the .3pk at all. Mirrors `import_release`'s manifest
/// bookkeeping: only a fully-landed component is recorded under its new
/// checksum, so a partial one keeps reading as New/Changed — and therefore
/// selectable again — until the rest turns up.
pub fn recompile_release(
    conn: &Connection,
    package_path: &Path,
    library_dir: &Path,
    selection: Option<Vec<String>>,
) -> Result<RecompileOutcome, AppError> {
    let manifest = read_manifest(package_path)?;
    let package_dir = package_path
        .parent()
        .ok_or_else(|| AppError::InvalidInput("Package has no parent directory".into()))?;
    let dest = layout::release_dir(
        library_dir,
        &manifest.release.designer,
        &manifest.release.name,
        Some(&manifest.release.date),
    );
    let local = read_local_manifest(&dest);
    if dest.exists() && local.is_none() {
        return Err(AppError::InvalidInput(format!(
            "'{}' already exists — remove it first to re-import",
            dest.display()
        )));
    }
    let updating = local.is_some();
    let old_components: Vec<Component> = local.map(|m| m.components).unwrap_or_default();
    let old_by_name: HashMap<&str, &Component> = old_components
        .iter()
        .map(|c| (c.name.as_str(), c))
        .collect();
    std::fs::create_dir_all(&dest)
        .map_err(|e| AppError::IoError(format!("Failed to create release dir: {}", e)))?;
    extract_release_payload(package_path, &dest)?;

    let selected = |name: &str| {
        selection
            .as_ref()
            .is_none_or(|s| s.iter().any(|n| n == name))
    };
    let mut succeeded: HashSet<&str> = HashSet::new();
    let mut components_out: Vec<RecompiledComponent> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    for component in &manifest.components {
        if !selected(&component.name) {
            continue;
        }
        let component_dest = dest.join(layout::sanitize_segment(&component.name));
        let old = old_by_name.get(component.name.as_str()).copied();
        match import_component(package_dir, component, &component_dest, old, updating, &mut warnings) {
            Ok(count) => {
                succeeded.insert(component.name.as_str());
                components_out.push(RecompiledComponent {
                    name: component.name.clone(),
                    complete: true,
                    files_landed: count,
                    files_missing: 0,
                    missing_bytes: 0.0,
                });
            }
            Err(ImportFailure::Blocked(message)) => {
                errors.push(format!("{}: {}", component.name, message));
            }
            Err(ImportFailure::Retryable(_)) => {
                let manifest_files: Vec<ManifestFile> = component
                    .models
                    .iter()
                    .flat_map(|m| m.files.iter().cloned())
                    .collect();
                let (landed, missing, missing_bytes) =
                    materialize_owned_files(conn, &component_dest, &manifest_files)?;
                let complete = missing == 0;
                if complete {
                    succeeded.insert(component.name.as_str());
                } else {
                    warnings.push(format!(
                        "{}: {} of {} files recompiled from your library, {} still missing",
                        component.name,
                        landed,
                        manifest_files.len(),
                        missing
                    ));
                }
                components_out.push(RecompiledComponent {
                    name: component.name.clone(),
                    complete,
                    files_landed: landed,
                    files_missing: missing,
                    missing_bytes,
                });
            }
        }
    }

    // Same truthful-manifest bookkeeping as import_release: only a
    // component that landed COMPLETE gets its new checksum recorded.
    let new_names: HashSet<&str> = manifest
        .components
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    let mut merged: Vec<Component> = manifest
        .components
        .iter()
        .filter_map(|c| {
            if succeeded.contains(c.name.as_str()) {
                Some(c.clone())
            } else {
                old_by_name.get(c.name.as_str()).map(|old| (*old).clone())
            }
        })
        .collect();
    merged.extend(
        old_components
            .iter()
            .filter(|old| !new_names.contains(old.name.as_str()))
            .cloned(),
    );
    let mut final_manifest = manifest.clone();
    final_manifest.components = merged;
    std::fs::write(dest.join("manifest.json"), final_manifest.to_json()?)
        .map_err(|e| AppError::IoError(format!("Failed to write manifest: {}", e)))?;

    Ok(RecompileOutcome {
        release_name: manifest.release.name.clone(),
        designer: manifest.release.designer.clone(),
        dest_dir: dest.to_string_lossy().into_owned(),
        components: components_out,
        errors,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::compressors::compress_files;
    use crate::file::pack_manifest::{build_manifest, PackedComponent};
    use std::path::PathBuf;

    fn temp(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("stlpack_import_{}_{}", tag, std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        dir
    }

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        db::test_init(&conn);
        conn
    }

    fn write_release_json(staged: &Path) {
        std::fs::write(
            staged.join("release.json"),
            r#"{"name":"Knights","designer":"DTL","description":"","date":"5/2026",
                "version":"1","model_references":[],"groups":[],"release_dir":"x",
                "images":[],"other_files":[]}"#,
        )
        .unwrap();
    }

    #[test]
    fn safe_relative_rejects_traversal_absolute_and_drive_relative_names() {
        assert!(safe_relative("variant_b/base.stl").is_some(), "legit subdir kept");
        assert!(safe_relative("../escape.stl").is_none(), "parent-dir escape");
        assert!(safe_relative("a/../../b").is_none(), "buried parent-dir escape");
        assert!(safe_relative("/etc/passwd").is_none(), "posix-absolute");
        // Textual check: on a non-Windows build, Path's parser wouldn't
        // otherwise see this as anything but a plain relative component.
        assert!(safe_relative("C:evil.stl").is_none(), "drive-relative");
        assert!(safe_relative(r"C:\evil.stl").is_none(), "drive-absolute");
        assert!(safe_relative(r"\\server\share\x").is_none(), "unc-style");
    }

    fn write_model_json(component: &Path, name: &str, files: &[&str]) {
        let list = files
            .iter()
            .map(|f| format!("\"{}\"", f))
            .collect::<Vec<_>>()
            .join(",");
        std::fs::write(
            component.join("model.json"),
            format!(
                r#"{{"id":null,"name":"{}","description":null,"tags":[],"images":[],
                    "model_files":[{}],"group":null,"support_status":"unsupported"}}"#,
                name, list
            ),
        )
        .unwrap();
    }

    /// Pack `staged` (release.json + one dir per component) the way finalize
    /// does: component zips + a release.3pk holding manifest.json.
    fn pack(staged: &Path, component_names: &[&str], out: &Path) -> Manifest {
        std::fs::create_dir_all(out).unwrap();
        let mut packed = Vec::new();
        for name in component_names {
            let archive_path = out.join(format!("{}.zip", name));
            let entries = compress_files(
                &[staged.join(name)],
                std::fs::File::create(&archive_path).unwrap(),
                None::<fn(u32) -> bool>,
            )
            .unwrap();
            packed.push(PackedComponent {
                name: (*name).to_string(),
                archive_path,
                entries,
            });
        }
        let manifest = build_manifest(staged, &packed, "0.1.0").unwrap();
        std::fs::write(staged.join("manifest.json"), manifest.to_json().unwrap()).unwrap();
        compress_files(
            &[staged.join("manifest.json"), staged.join("release.json")],
            std::fs::File::create(out.join("release.3pk")).unwrap(),
            None::<fn(u32) -> bool>,
        )
        .unwrap();
        manifest
    }

    /// Pack a release with the real writer, then import it elsewhere and
    /// check the tree + curation sidecar arrived intact — the full loop.
    #[test]
    fn packed_release_imports_verified_and_complete() {
        let conn = test_conn();
        let dir = temp("roundtrip");
        let staged = dir.join("staged");
        let component = staged.join("knight");
        std::fs::create_dir_all(component.join("variant_b")).unwrap();
        std::fs::write(component.join("base.stl"), b"shared-base-bytes").unwrap();
        std::fs::write(component.join("variant_b/base.stl"), b"shared-base-bytes").unwrap();
        std::fs::write(component.join("body.stl"), b"knight-body-bytes").unwrap();
        write_model_json(
            &component,
            "knight",
            &["base.stl", "variant_b/base.stl", "body.stl"],
        );
        write_release_json(&staged);

        let out = dir.join("packed");
        pack(&staged, &["knight"], &out);

        // Import into a fresh library
        let library = dir.join("library");
        std::fs::create_dir_all(&library).unwrap();
        let outcome = import_release(&out.join("release.3pk"), &library, None).unwrap();
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        assert!(!outcome.updated);
        assert_eq!(outcome.components, 1);

        let release_dir = Path::new(&outcome.dest_dir);
        assert!(
            release_dir.ends_with("DTL/2026-05 Knights"),
            "canonical Designer/YYYY-MM Release landing spot, got {}",
            release_dir.display()
        );
        // Every manifest name exists — including the dedup-elided twin
        for name in [
            "knight/base.stl",
            "knight/variant_b/base.stl",
            "knight/body.stl",
            "knight/model.json",
            "release.json",
            "manifest.json",
        ] {
            assert!(release_dir.join(name).is_file(), "{} imported", name);
        }
        assert_eq!(
            std::fs::read(release_dir.join("knight/base.stl")).unwrap(),
            b"shared-base-bytes"
        );

        // A corrupted component archive is refused by checksum, not imported
        std::fs::write(out.join("knight.zip"), b"tampered").unwrap();
        std::fs::remove_dir_all(release_dir).ok();
        let outcome = import_release(&out.join("release.3pk"), &library, None).unwrap();
        assert_eq!(outcome.components, 0);
        assert!(outcome.errors[0].contains("checksum mismatch"));
        // …and the failed component is NOT recorded as present, so the next
        // inspect still offers it
        let inspection = inspect_package(&out.join("release.3pk"), &library, &conn).unwrap();
        assert_eq!(inspection.components[0].state, ComponentState::New);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn inspect_diffs_components_against_the_local_manifest() {
        let conn = test_conn();
        let dir = temp("inspect");
        let staged = dir.join("staged");
        for (name, bytes) in [("knight", &b"knight-v1"[..]), ("goblin", &b"goblin-v1"[..])] {
            let component = staged.join(name);
            std::fs::create_dir_all(&component).unwrap();
            std::fs::write(component.join("body.stl"), bytes).unwrap();
            write_model_json(&component, name, &["body.stl"]);
        }
        write_release_json(&staged);
        let out1 = dir.join("out1");
        pack(&staged, &["knight", "goblin"], &out1);
        let library = dir.join("library");
        std::fs::create_dir_all(&library).unwrap();

        // Not imported yet: everything is new
        let inspection = inspect_package(&out1.join("release.3pk"), &library, &conn).unwrap();
        assert!(!inspection.is_update);
        assert!(inspection.blocked.is_none());
        assert!(inspection
            .components
            .iter()
            .all(|c| c.state == ComponentState::New));

        import_release(&out1.join("release.3pk"), &library, None).unwrap();

        // Imported and untouched: everything is unchanged
        let inspection = inspect_package(&out1.join("release.3pk"), &library, &conn).unwrap();
        assert!(inspection.is_update);
        assert!(inspection
            .components
            .iter()
            .all(|c| c.state == ComponentState::Unchanged));

        // The creator ships v2 with a changed knight
        std::fs::write(staged.join("knight/body.stl"), b"knight-v2").unwrap();
        let out2 = dir.join("out2");
        pack(&staged, &["knight", "goblin"], &out2);
        let inspection = inspect_package(&out2.join("release.3pk"), &library, &conn).unwrap();
        let state = |name: &str| {
            inspection
                .components
                .iter()
                .find(|c| c.name == name)
                .unwrap()
                .state
        };
        assert_eq!(state("knight"), ComponentState::Changed);
        assert_eq!(state("goblin"), ComponentState::Unchanged);

        // A component archive missing next to the .3pk can't import
        std::fs::remove_file(out2.join("goblin.zip")).unwrap();
        let inspection = inspect_package(&out2.join("release.3pk"), &library, &conn).unwrap();
        let goblin = inspection
            .components
            .iter()
            .find(|c| c.name == "goblin")
            .unwrap();
        assert_eq!(goblin.state, ComponentState::MissingArchive);
        assert!(goblin.detail.is_some());

        // A locally packed-at-rest component refuses updates until unpacked
        let knight_dir = Path::new(&inspection.dest_dir).join("knight");
        std::fs::write(knight_dir.join(PACK_SIDECAR_NAME), b"{}").unwrap();
        let inspection = inspect_package(&out2.join("release.3pk"), &library, &conn).unwrap();
        let knight = inspection
            .components
            .iter()
            .find(|c| c.name == "knight")
            .unwrap();
        assert_eq!(knight.state, ComponentState::Packed);
        let outcome = import_release(
            &out2.join("release.3pk"),
            &library,
            Some(vec!["knight".into()]),
        )
        .unwrap();
        assert_eq!(outcome.components, 0);
        assert!(outcome.errors[0].contains("packed at rest"));

        std::fs::remove_dir_all(&dir).ok();
    }

    fn owned_file(path: &str, dir: &str, size_bytes: i64, hash: &str) -> crate::catalog::FileRow {
        crate::catalog::FileRow {
            path: path.into(),
            dir_path: dir.into(),
            file_name: path.rsplit('/').next().unwrap().into(),
            extension: "stl".into(),
            size_bytes,
            content_hash: Some(hash.into()),
            ..Default::default()
        }
    }

    /// The completeness diff is checksum-only and runs regardless of a
    /// component's archive state: knight is fully owned under names the
    /// manifest never uses, orc is partially owned including a
    /// packed-at-rest donor's sidecar hash, and goblin is owned nowhere.
    #[test]
    fn completeness_diffs_manifest_checksums_against_library_wide_ownership() {
        let mut conn = test_conn();
        let dir = temp("completeness");
        let staged = dir.join("staged");

        let knight = staged.join("knight");
        std::fs::create_dir_all(&knight).unwrap();
        std::fs::write(knight.join("body.stl"), b"knight-body-bytes").unwrap();
        std::fs::write(knight.join("base.stl"), b"knight-base-bytes").unwrap();
        write_model_json(&knight, "knight", &["body.stl", "base.stl"]);

        let orc = staged.join("orc");
        std::fs::create_dir_all(&orc).unwrap();
        std::fs::write(orc.join("orc_body.stl"), b"orc-body-bytes").unwrap();
        std::fs::write(orc.join("orc_head.stl"), b"orc-head-bytes").unwrap();
        write_model_json(&orc, "orc", &["orc_body.stl", "orc_head.stl"]);

        let goblin = staged.join("goblin");
        std::fs::create_dir_all(&goblin).unwrap();
        std::fs::write(goblin.join("gob.stl"), b"goblin-bytes").unwrap();
        write_model_json(&goblin, "goblin", &["gob.stl"]);

        write_release_json(&staged);
        let out = dir.join("packed");
        let manifest = pack(&staged, &["knight", "orc", "goblin"], &out);
        let checksum = |component: &str, file: &str| -> String {
            manifest
                .components
                .iter()
                .find(|c| c.name == component)
                .unwrap()
                .models[0]
                .files
                .iter()
                .find(|f| f.name == file)
                .unwrap()
                .checksum
                .clone()
        };
        let file_size = |component: &str, file: &str| -> f64 {
            manifest
                .components
                .iter()
                .find(|c| c.name == component)
                .unwrap()
                .models[0]
                .files
                .iter()
                .find(|f| f.name == file)
                .unwrap()
                .size_bytes as f64
        };

        // knight's two files sit somewhere else entirely, under different
        // names — checksum is identity, names aren't.
        let knight_body = pack::bare_hash(&checksum("knight", "body.stl")).to_string();
        let knight_base = pack::bare_hash(&checksum("knight", "base.stl")).to_string();
        // orc_body is owned only as a packed model's sidecar-hashed entry —
        // no unpack needed to match it; orc_head is owned nowhere.
        let orc_body = pack::bare_hash(&checksum("orc", "orc_body.stl")).to_string();
        let mut orc_donor = owned_file(
            "/library/vault/orc_body_twin.stl",
            "/library/vault",
            17,
            &orc_body,
        );
        orc_donor.archive_path = Some("/library/vault/model.plinthpack".into());

        let rows = vec![
            owned_file(
                "/library/scattered/renamed_body.stl",
                "/library/scattered",
                17,
                &knight_body,
            ),
            owned_file(
                "/library/other/an_old_download.stl",
                "/library/other",
                17,
                &knight_base,
            ),
            orc_donor,
        ];
        db::replace_catalog(&mut conn, "/library", &rows, &[], &[], &[], &[]).unwrap();

        let inspection = inspect_package(&out.join("release.3pk"), &dir.join("dest"), &conn).unwrap();
        let component = |name: &str| {
            inspection
                .components
                .iter()
                .find(|c| c.name == name)
                .unwrap()
        };

        let knight = component("knight");
        assert_eq!(knight.files_owned, 2);
        assert_eq!(knight.missing_bytes, 0.0);
        assert!(knight.missing.is_empty());

        let orc = component("orc");
        assert_eq!(orc.files_owned, 1);
        assert_eq!(orc.missing.len(), 1);
        assert_eq!(orc.missing[0].name, "orc_head.stl");
        assert_eq!(orc.missing_bytes, file_size("orc", "orc_head.stl"));

        let goblin = component("goblin");
        assert_eq!(goblin.files_owned, 0);
        assert_eq!(goblin.missing.len(), 1);
        assert_eq!(goblin.missing_bytes, file_size("goblin", "gob.stl"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn update_replaces_selected_components_and_preserves_local_edits() {
        let conn = test_conn();
        let dir = temp("update");
        let staged = dir.join("staged");
        let knight = staged.join("knight");
        std::fs::create_dir_all(&knight).unwrap();
        std::fs::write(knight.join("body.stl"), b"body-v1").unwrap();
        std::fs::write(knight.join("old_only.stl"), b"dropped-in-v2").unwrap();
        write_model_json(&knight, "knight", &["body.stl", "old_only.stl"]);
        let goblin = staged.join("goblin");
        std::fs::create_dir_all(&goblin).unwrap();
        std::fs::write(goblin.join("gob.stl"), b"gob-v1").unwrap();
        write_model_json(&goblin, "goblin", &["gob.stl"]);
        write_release_json(&staged);
        let out1 = dir.join("out1");
        pack(&staged, &["knight", "goblin"], &out1);
        let library = dir.join("library");
        std::fs::create_dir_all(&library).unwrap();
        let outcome = import_release(&out1.join("release.3pk"), &library, None).unwrap();
        let release_dir = PathBuf::from(&outcome.dest_dir);

        // The user saves supports over body.stl before v2 arrives
        std::fs::write(release_dir.join("knight/body.stl"), b"user-supported-body").unwrap();
        // …and drops in a file of their own the release never shipped
        std::fs::write(release_dir.join("knight/my-remix.stl"), b"mine").unwrap();

        // v2: body changed, old_only dropped, extra added; goblin untouched
        std::fs::write(knight.join("body.stl"), b"body-v2").unwrap();
        std::fs::remove_file(knight.join("old_only.stl")).unwrap();
        std::fs::write(knight.join("extra.stl"), b"extra-v2").unwrap();
        write_model_json(&knight, "knight", &["body.stl", "extra.stl"]);
        let out2 = dir.join("out2");
        pack(&staged, &["knight", "goblin"], &out2);

        let outcome = import_release(
            &out2.join("release.3pk"),
            &library,
            Some(vec!["knight".into()]),
        )
        .unwrap();
        assert!(outcome.updated);
        assert_eq!(outcome.components, 1, "{:?}", outcome.errors);

        let knight_dir = release_dir.join("knight");
        assert_eq!(
            std::fs::read(knight_dir.join("body.stl")).unwrap(),
            b"body-v2"
        );
        assert_eq!(
            std::fs::read(knight_dir.join("body (edited).stl")).unwrap(),
            b"user-supported-body",
            "locally edited file moved aside, never truncated"
        );
        assert_eq!(outcome.warnings.len(), 1, "{:?}", outcome.warnings);
        assert!(
            !knight_dir.join("old_only.stl").exists(),
            "stale file removed"
        );
        assert_eq!(
            std::fs::read(knight_dir.join("extra.stl")).unwrap(),
            b"extra-v2"
        );
        assert_eq!(
            std::fs::read(knight_dir.join("my-remix.stl")).unwrap(),
            b"mine"
        );
        assert_eq!(
            std::fs::read(release_dir.join("goblin/gob.stl")).unwrap(),
            b"gob-v1",
            "deselected component untouched"
        );

        // The written manifest reflects the disk: everything reads unchanged now
        let inspection = inspect_package(&out2.join("release.3pk"), &library, &conn).unwrap();
        assert!(inspection
            .components
            .iter()
            .all(|c| c.state == ComponentState::Unchanged));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_failed_component_update_stays_marked_changed() {
        let conn = test_conn();
        let dir = temp("failedupdate");
        let staged = dir.join("staged");
        let knight = staged.join("knight");
        std::fs::create_dir_all(&knight).unwrap();
        std::fs::write(knight.join("body.stl"), b"body-v1").unwrap();
        write_model_json(&knight, "knight", &["body.stl"]);
        write_release_json(&staged);
        let out1 = dir.join("out1");
        pack(&staged, &["knight"], &out1);
        let library = dir.join("library");
        std::fs::create_dir_all(&library).unwrap();
        let outcome = import_release(&out1.join("release.3pk"), &library, None).unwrap();
        let release_dir = PathBuf::from(&outcome.dest_dir);

        std::fs::write(knight.join("body.stl"), b"body-v2").unwrap();
        let out2 = dir.join("out2");
        pack(&staged, &["knight"], &out2);
        std::fs::write(out2.join("knight.zip"), b"tampered").unwrap();

        let outcome = import_release(&out2.join("release.3pk"), &library, None).unwrap();
        assert_eq!(outcome.components, 0);
        assert!(outcome.errors[0].contains("checksum mismatch"));
        // Disk untouched, and the local manifest still records v1 — the
        // component reads as changed again instead of silently "unchanged"
        assert_eq!(
            std::fs::read(release_dir.join("knight/body.stl")).unwrap(),
            b"body-v1"
        );
        let inspection = inspect_package(&out2.join("release.3pk"), &library, &conn).unwrap();
        assert_eq!(inspection.components[0].state, ComponentState::Changed);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// component.archive is attacker-authored manifest text; a value that
    /// escapes package_dir ("../secret.zip") must be refused before it's
    /// ever opened — not followed to hash/extract a file the import was
    /// never meant to touch.
    #[test]
    fn refuses_a_malicious_component_archive_path() {
        let conn = test_conn();
        let dir = temp("evilarchive");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();

        // Stands in for whatever a "../" archive path would actually reach.
        std::fs::write(dir.join("secret.zip"), b"top-secret-bytes").unwrap();

        let manifest = Manifest::new(
            manifest::ManifestRelease {
                name: "Evil Release".into(),
                designer: "Attacker".into(),
                date: "2026-01".into(),
                version: "1".into(),
                description: "".into(),
                tags: vec![],
                images: vec![],
            },
            vec![Component {
                name: "comp".into(),
                archive: "../secret.zip".into(),
                checksum: "blake3:deadbeef".into(),
                size_bytes: 0,
                dedup: false,
                models: vec![],
            }],
            "0.1.0",
        );
        std::fs::write(out.join("manifest.json"), manifest.to_json().unwrap()).unwrap();
        write_release_json(&out);
        compress_files(
            &[out.join("manifest.json"), out.join("release.json")],
            std::fs::File::create(out.join("release.3pk")).unwrap(),
            None::<fn(u32) -> bool>,
        )
        .unwrap();

        let library = dir.join("library");
        std::fs::create_dir_all(&library).unwrap();

        let inspection = inspect_package(&out.join("release.3pk"), &library, &conn).unwrap();
        assert_eq!(
            inspection.components[0].state,
            ComponentState::MissingArchive
        );
        assert!(inspection.components[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("not a safe"));

        let outcome = import_release(&out.join("release.3pk"), &library, None).unwrap();
        assert_eq!(outcome.components, 0, "the hostile component must not import");
        assert!(
            outcome.errors[0].contains("not a safe path"),
            "{:?}",
            outcome.errors
        );
        let release_dir = Path::new(&outcome.dest_dir);
        assert!(
            !release_dir.join("comp").exists(),
            "nothing extracted from the escaped archive"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The scenario the whole feature exists for: no sibling archive next
    /// to the .3pk at all. Recompile still materializes what the library
    /// owns — one file scattered under an unrelated name, one owned only
    /// as a packed-at-rest donor (a real pack.json + model.plinthpack on
    /// disk, ephemerally extracted) — leaves the unowned file untouched,
    /// and then completes for free once that file is acquired and indexed.
    #[test]
    fn recompiles_a_partial_component_when_its_archive_is_entirely_missing_and_resumes_for_free() {
        let mut conn = test_conn();
        let dir = temp("recompile_missing_archive");
        let staged = dir.join("staged");
        let relic = staged.join("relic");
        std::fs::create_dir_all(&relic).unwrap();
        std::fs::write(relic.join("relic_a.stl"), b"relic-a-bytes").unwrap();
        std::fs::write(relic.join("relic_b.stl"), b"relic-b-bytes").unwrap();
        std::fs::write(relic.join("relic_c.stl"), b"relic-c-bytes").unwrap();
        write_model_json(&relic, "relic", &["relic_a.stl", "relic_b.stl", "relic_c.stl"]);
        write_release_json(&staged);
        let out = dir.join("packed");
        let manifest = pack(&staged, &["relic"], &out);
        let checksum = |file: &str| -> String {
            manifest.components[0]
                .models[0]
                .files
                .iter()
                .find(|f| f.name == file)
                .unwrap()
                .checksum
                .clone()
        };
        let size = |file: &str| -> f64 {
            manifest.components[0]
                .models[0]
                .files
                .iter()
                .find(|f| f.name == file)
                .unwrap()
                .size_bytes as f64
        };

        // relic_a is owned loose, under a name/dir the manifest never uses —
        // a real file on disk, since materializing it means hard-linking or
        // copying real bytes, not just matching a DB row.
        let scattered_dir = dir.join("library/scattered");
        std::fs::create_dir_all(&scattered_dir).unwrap();
        std::fs::write(scattered_dir.join("some_other_name.stl"), b"relic-a-bytes").unwrap();
        let scattered = owned_file(
            &scattered_dir.join("some_other_name.stl").to_string_lossy(),
            &scattered_dir.to_string_lossy(),
            13,
            pack::bare_hash(&checksum("relic_a.stl")),
        );

        // relic_b is owned only inside a REAL packed model on disk — a
        // genuine pack.json + model.plinthpack, so the donor path exercises
        // extract_paths_ephemeral for real, not just a DB row.
        let donor_model = dir.join("library/vault/relic_b_donor");
        std::fs::create_dir_all(&donor_model).unwrap();
        std::fs::write(donor_model.join("twin.stl"), b"relic-b-bytes").unwrap();
        let cancel = AtomicBool::new(false);
        pack::pack_model("test", &donor_model, None, &cancel, |_, _| true).unwrap();
        let mut packed_donor = owned_file(
            &pack::entry_disk_path(&donor_model, "twin.stl").to_string_lossy(),
            &donor_model.to_string_lossy(),
            13,
            pack::bare_hash(&checksum("relic_b.stl")),
        );
        packed_donor.archive_path =
            Some(donor_model.join(pack::PACK_ARCHIVE_NAME).to_string_lossy().into_owned());

        let library_root = dir.join("library").to_string_lossy().into_owned();
        db::replace_catalog(
            &mut conn,
            &library_root,
            &[scattered, packed_donor],
            &[],
            &[],
            &[],
            &[],
        )
        .unwrap();

        // The sibling archive is gone: MissingArchive, exactly the
        // "a freebie imported into a different folder" scenario.
        std::fs::remove_file(out.join("relic.zip")).unwrap();

        let library = dir.join("dest_library");
        std::fs::create_dir_all(&library).unwrap();
        let outcome = recompile_release(&conn, &out.join("release.3pk"), &library, None).unwrap();
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        assert_eq!(outcome.components.len(), 1);
        let relic_out = &outcome.components[0];
        assert!(!relic_out.complete);
        assert_eq!(relic_out.files_landed, 2);
        assert_eq!(relic_out.files_missing, 1);
        assert_eq!(relic_out.missing_bytes, size("relic_c.stl"));

        let component_dir = Path::new(&outcome.dest_dir).join("relic");
        assert_eq!(
            std::fs::read(component_dir.join("relic_a.stl")).unwrap(),
            b"relic-a-bytes",
            "donor materialized under the manifest's own name"
        );
        assert_eq!(
            std::fs::read(component_dir.join("relic_b.stl")).unwrap(),
            b"relic-b-bytes",
            "packed donor extracted ephemerally, then copied into place"
        );
        assert!(
            !component_dir.join("relic_c.stl").exists(),
            "no donor anywhere — no placeholder, no trace on disk"
        );

        // The sibling archive is still absent, so state stays MissingArchive
        // — but completeness is checksum-only and reports the partial
        // landing regardless: never silently "unchanged" with a file gone.
        let inspection = inspect_package(&out.join("release.3pk"), &library, &conn).unwrap();
        assert_eq!(inspection.components[0].state, ComponentState::MissingArchive);
        assert_eq!(inspection.components[0].files_owned, 2);

        // The last file turns up somewhere else and gets indexed —
        // recompiling again completes the set for free, no re-fetch of
        // what already landed.
        let found_dir = dir.join("library/found");
        std::fs::create_dir_all(&found_dir).unwrap();
        std::fs::write(found_dir.join("relic_c_finally.stl"), b"relic-c-bytes").unwrap();
        let found = owned_file(
            &found_dir.join("relic_c_finally.stl").to_string_lossy(),
            &found_dir.to_string_lossy(),
            13,
            pack::bare_hash(&checksum("relic_c.stl")),
        );
        let found_root = found_dir.to_string_lossy().into_owned();
        db::replace_catalog(&mut conn, &found_root, &[found], &[], &[], &[], &[]).unwrap();

        let outcome = recompile_release(&conn, &out.join("release.3pk"), &library, None).unwrap();
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        let relic_out = &outcome.components[0];
        assert!(relic_out.complete);
        assert_eq!(relic_out.files_landed, 3);
        assert_eq!(relic_out.files_missing, 0);
        assert_eq!(
            std::fs::read(component_dir.join("relic_c.stl")).unwrap(),
            b"relic-c-bytes"
        );

        // The sibling archive itself never came back — state is still
        // MissingArchive, an orthogonal question from ownership — but the
        // checksum diff now shows the set complete, 0 bytes missing.
        let inspection = inspect_package(&out.join("release.3pk"), &library, &conn).unwrap();
        assert_eq!(inspection.components[0].state, ComponentState::MissingArchive);
        assert_eq!(inspection.components[0].files_owned, 3);
        assert!(inspection.components[0].missing.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A component already packed at rest is never a donor-fallback
    /// candidate: the directory already holds it, just compressed, so
    /// recompile must refuse rather than write loose donor files next to
    /// the archive.
    #[test]
    fn recompile_refuses_a_packed_at_rest_component_instead_of_writing_donors_into_it() {
        let conn = test_conn();
        let dir = temp("recompile_packed");
        let staged = dir.join("staged");
        let relic = staged.join("relic");
        std::fs::create_dir_all(&relic).unwrap();
        std::fs::write(relic.join("relic_a.stl"), b"relic-a-bytes").unwrap();
        write_model_json(&relic, "relic", &["relic_a.stl"]);
        write_release_json(&staged);
        let out = dir.join("packed");
        pack(&staged, &["relic"], &out);

        let library = dir.join("library");
        std::fs::create_dir_all(&library).unwrap();
        import_release(&out.join("release.3pk"), &library, None).unwrap();

        // The user packed the imported component at rest before a v2 shows
        // up (checksum changed, so the archive path would otherwise fail
        // and normally fall back to library donors).
        let component_dir = library.join("DTL/2026-05 Knights/relic");
        std::fs::write(component_dir.join(PACK_SIDECAR_NAME), b"{}").unwrap();
        std::fs::write(staged.join("relic/relic_a.stl"), b"relic-a-bytes-v2").unwrap();
        let out2 = dir.join("packed2");
        pack(&staged, &["relic"], &out2);

        let outcome = recompile_release(&conn, &out2.join("release.3pk"), &library, None).unwrap();
        assert!(outcome.components.is_empty());
        assert_eq!(outcome.errors.len(), 1);
        assert!(outcome.errors[0].contains("packed at rest"), "{:?}", outcome.errors);

        std::fs::remove_dir_all(&dir).ok();
    }
}
