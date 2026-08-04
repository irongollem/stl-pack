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

use crate::catalog::layout;
use crate::catalog::pack::{edited_aside_path, PACK_SIDECAR_NAME};
use crate::error::AppError;
use crate::manifest::{self, Component, Manifest, ManifestFile};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;

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

/// Diff an incoming `release.3pk` against the library without touching disk.
pub fn inspect_package(
    package_path: &Path,
    library_dir: &Path,
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

    let components = manifest
        .components
        .iter()
        .map(|component| {
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
            ComponentStatus {
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
            }
        })
        .collect();

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
        let result = (|| -> Result<u32, String> {
            if updating && contains_pack_sidecar(&component_dest) {
                return Err("packed at rest — unpack it in the catalog first, then update".into());
            }
            // Same guard as inspect_package: an attacker-authored archive
            // name must not resolve outside package_dir before we open it.
            if safe_relative(&component.archive).is_none() {
                return Err(format!(
                    "archive '{}' is not a safe path — refusing to import",
                    component.archive
                ));
            }
            let archive_path = package_dir.join(&component.archive);
            if !archive_path.is_file() {
                return Err(format!("archive '{}' is missing", component.archive));
            }
            // The checksum is the integrity promise of the format — a truncated
            // download or bit-rot surfaces here, not as broken STLs later
            let actual = manifest::hash_file(&archive_path)?;
            if actual != component.checksum {
                return Err("checksum mismatch — the archive is corrupted or was modified".into());
            }
            let manifest_files: Vec<ManifestFile> = component
                .models
                .iter()
                .flat_map(|m| m.files.iter().cloned())
                .collect();
            if let Some(old) = old {
                warnings.extend(preserve_local_edits(&component_dest, old, &manifest_files)?);
            }
            // sanitize_segment (in component_dest): idempotent for our own
            // packages and stops a hostile component name ("../x") from
            // landing outside the release dir
            manifest::extract_component_archive(&archive_path, &component_dest, &manifest_files)?;
            if let Some(old) = old {
                remove_stale_files(&component_dest, old, &manifest_files);
            }
            Ok(manifest_files.len() as u32)
        })();
        match result {
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
        let inspection = inspect_package(&out.join("release.3pk"), &library).unwrap();
        assert_eq!(inspection.components[0].state, ComponentState::New);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn inspect_diffs_components_against_the_local_manifest() {
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
        let inspection = inspect_package(&out1.join("release.3pk"), &library).unwrap();
        assert!(!inspection.is_update);
        assert!(inspection.blocked.is_none());
        assert!(inspection
            .components
            .iter()
            .all(|c| c.state == ComponentState::New));

        import_release(&out1.join("release.3pk"), &library, None).unwrap();

        // Imported and untouched: everything is unchanged
        let inspection = inspect_package(&out1.join("release.3pk"), &library).unwrap();
        assert!(inspection.is_update);
        assert!(inspection
            .components
            .iter()
            .all(|c| c.state == ComponentState::Unchanged));

        // The creator ships v2 with a changed knight
        std::fs::write(staged.join("knight/body.stl"), b"knight-v2").unwrap();
        let out2 = dir.join("out2");
        pack(&staged, &["knight", "goblin"], &out2);
        let inspection = inspect_package(&out2.join("release.3pk"), &library).unwrap();
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
        let inspection = inspect_package(&out2.join("release.3pk"), &library).unwrap();
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
        let inspection = inspect_package(&out2.join("release.3pk"), &library).unwrap();
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

    #[test]
    fn update_replaces_selected_components_and_preserves_local_edits() {
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
        let inspection = inspect_package(&out2.join("release.3pk"), &library).unwrap();
        assert!(inspection
            .components
            .iter()
            .all(|c| c.state == ComponentState::Unchanged));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_failed_component_update_stays_marked_changed() {
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
        let inspection = inspect_package(&out2.join("release.3pk"), &library).unwrap();
        assert_eq!(inspection.components[0].state, ComponentState::Changed);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// component.archive is attacker-authored manifest text; a value that
    /// escapes package_dir ("../secret.zip") must be refused before it's
    /// ever opened — not followed to hash/extract a file the import was
    /// never meant to touch.
    #[test]
    fn refuses_a_malicious_component_archive_path() {
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

        let inspection = inspect_package(&out.join("release.3pk"), &library).unwrap();
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
}
