//! The `.3pk` release manifest (format v1). See docs/3PK.md for the spec.

use crate::error::AppError;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::io::Read;
use std::path::Path;

pub const FORMAT: &str = "3pk";
/// Bump on a breaking change; readers reject unknown majors (see `is_readable`).
pub const VERSION: u32 = 1;

/// The whole manifest — serialized as `manifest.json` inside `release.3pk`.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct Manifest {
    pub format: String,
    pub version: u32,
    /// e.g. "plinth/0.1.0" — provenance, not load-bearing.
    pub generator: String,
    pub release: ManifestRelease,
    pub components: Vec<Component>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct ManifestRelease {
    pub name: String,
    pub designer: String,
    /// Canonical YYYY-MM.
    pub date: String,
    pub version: String,
    pub description: String,
    pub tags: Vec<String>,
    /// Image paths inside `release.3pk`.
    pub images: Vec<String>,
}

/// One group/model, mapping to a sibling component archive.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct Component {
    pub name: String,
    /// Archive filename, relative to the release dir.
    pub archive: String,
    /// `blake3:<hex>` of the archive bytes — drives update detection.
    pub checksum: String,
    pub size_bytes: u64,
    /// True when the archive stores duplicate contents once: some manifest
    /// file names are then absent from the archive and must be
    /// rematerialized from a sibling with the same checksum on extract
    /// (see extract_component_archive). Additive field — absent reads false.
    #[serde(default)]
    pub dedup: bool,
    pub models: Vec<ManifestModel>,
}

/// The wire form of a catalog model: scanner fields plus every user override,
/// tags, and per-file pose assignments. Optional fields are omitted-as-null
/// when unset.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct ManifestModel {
    pub id: Option<String>,
    pub name: String,
    pub custom_name: Option<String>,
    pub description: Option<String>,
    pub group: Option<String>,
    pub tags: Vec<String>,
    pub designer: Option<String>,
    pub sculptor: Option<String>,
    /// The facet between support and pose (weapon/mount). Additive in v1.
    #[serde(default)]
    pub variant: Option<String>,
    pub pose: Option<String>,
    pub scale: Option<String>,
    pub support_status: Option<String>,
    pub release_date: Option<String>,
    pub release_name: Option<String>,
    /// Base sizes in mm as canonical dimension strings ("25", or
    /// "60x35" for ovals/rectangles). Additive in v1.
    #[serde(default)]
    pub base_round_mm: Option<String>,
    #[serde(default)]
    pub base_square_mm: Option<String>,
    /// Preview path inside `release.3pk`.
    pub preview: Option<String>,
    pub files: Vec<ManifestFile>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct ManifestFile {
    /// Path relative to the component archive root.
    pub name: String,
    pub checksum: String,
    pub size_bytes: u64,
    /// The file's pose bucket, when the model was split by file (else null).
    pub pose: Option<String>,
    pub support_status: Option<String>,
}

impl Manifest {
    pub fn new(release: ManifestRelease, components: Vec<Component>, app_version: &str) -> Self {
        Self {
            format: FORMAT.to_string(),
            version: VERSION,
            generator: format!("plinth/{}", app_version),
            release,
            components,
        }
    }

    /// A reader accepts a manifest whose major matches this build's. Same-major
    /// additions are additive fields older readers ignore; a bumped major means
    /// the shape changed and we refuse to guess.
    pub fn is_readable(&self) -> bool {
        self.format == FORMAT && self.version == VERSION
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string_pretty(self)
            .map_err(|e| AppError::ConfigError(format!("Failed to encode manifest: {}", e)))
    }

    pub fn from_json(text: &str) -> Result<Self, AppError> {
        serde_json::from_str(text)
            .map_err(|e| AppError::InvalidInput(format!("Invalid 3pk manifest: {}", e)))
    }
}

/// Extract a component archive and rematerialize any manifest-listed file
/// a deduplicated archive elided, from an extracted sibling with the same
/// checksum — hardlinked where the destination volume supports it, copied
/// otherwise. A no-op step on non-dedup archives.
pub fn extract_component_archive(
    archive_path: &Path,
    dest_dir: &Path,
    files: &[ManifestFile],
) -> Result<(), AppError> {
    let file = std::fs::File::open(archive_path)
        .map_err(|e| AppError::IoError(format!("Failed to open {}: {}", archive_path.display(), e)))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| AppError::InvalidInput(format!("Not a readable archive: {}", e)))?;
    archive
        .extract(dest_dir)
        .map_err(|e| AppError::IoError(format!("Extraction failed: {}", e)))?;

    // First extracted path per checksum = the donor for elided twins.
    // `entry.name` is untrusted, so it goes through the same relative-path
    // guard as below; a name that fails it is just skipped as a donor,
    // not an error — a real gap still surfaces via the loop below.
    let mut by_checksum: std::collections::HashMap<&str, std::path::PathBuf> =
        std::collections::HashMap::new();
    for entry in files {
        let Some(rel) = crate::file::import::safe_relative(&entry.name) else {
            continue;
        };
        let path = dest_dir.join(rel);
        if path.is_file() {
            by_checksum.entry(entry.checksum.as_str()).or_insert(path);
        }
    }
    for entry in files {
        let Some(rel) = crate::file::import::safe_relative(&entry.name) else {
            return Err(AppError::InvalidInput(format!(
                "Manifest entry '{}' is not a safe relative path — refusing to write outside the destination",
                entry.name
            )));
        };
        let path = dest_dir.join(rel);
        // Belt-and-suspenders: this should be unreachable given the guard
        // above, but never hardlink/copy to a path that escaped dest_dir.
        if !path.starts_with(dest_dir) {
            return Err(AppError::InvalidInput(format!(
                "Manifest entry '{}' resolves outside the destination directory",
                entry.name
            )));
        }
        if path.exists() {
            continue;
        }
        let Some(donor) = by_checksum.get(entry.checksum.as_str()) else {
            return Err(AppError::InvalidInput(format!(
                "Archive is missing '{}' and no file with its checksum exists to restore it",
                entry.name
            )));
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AppError::IoError(format!("Failed to create dirs: {}", e)))?;
        }
        if std::fs::hard_link(donor, &path).is_err() {
            std::fs::copy(donor, &path)
                .map_err(|e| AppError::IoError(format!("Failed to restore {}: {}", entry.name, e)))?;
        }
    }
    Ok(())
}

/// BLAKE3 of a file's bytes, encoded `blake3:<hex>`. Streams so a multi-GB
/// component archive never lands fully in memory.
pub fn hash_file(path: &Path) -> Result<String, AppError> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| AppError::IoError(format!("Failed to open {}: {}", path.display(), e)))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|e| AppError::IoError(format!("Failed to read {}: {}", path.display(), e)))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        Manifest::new(
            ManifestRelease {
                name: "Dungeon Classics".into(),
                designer: "Dragon Trapper's Lodge".into(),
                date: "2026-05".into(),
                version: "1.0.0".into(),
                description: "".into(),
                tags: vec!["dungeon".into()],
                images: vec!["images/cover.png".into()],
            },
            vec![Component {
                name: "galeb duhr".into(),
                archive: "galeb duhr.zip".into(),
                checksum: "blake3:abc".into(),
                size_bytes: 123,
                dedup: false,
                models: vec![ManifestModel {
                    id: Some("uuid".into()),
                    name: "galeb duhr".into(),
                    custom_name: None,
                    description: None,
                    group: Some("galeb duhr".into()),
                    tags: vec!["earth".into()],
                    designer: Some("Dragon Trapper's Lodge".into()),
                    sculptor: None,
                    variant: None,
                    pose: Some("A".into()),
                    scale: Some("32mm".into()),
                    support_status: Some("unsupported".into()),
                    release_date: Some("2026-05".into()),
                    release_name: Some("Dungeon Classics".into()),
                    base_round_mm: None,
                    base_square_mm: None,
                    preview: Some("images/galeb duhr A.png".into()),
                    files: vec![ManifestFile {
                        name: "A/body.stl".into(),
                        checksum: "blake3:def".into(),
                        size_bytes: 42,
                        pose: Some("A".into()),
                        support_status: Some("unsupported".into()),
                    }],
                }],
            }],
            "0.1.0",
        )
    }

    #[test]
    fn round_trips_through_json() {
        let manifest = sample();
        let json = manifest.to_json().unwrap();
        let parsed = Manifest::from_json(&json).unwrap();
        assert!(parsed.is_readable());
        assert_eq!(parsed.format, "3pk");
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.generator, "plinth/0.1.0");
        assert_eq!(parsed.components.len(), 1);
        let model = &parsed.components[0].models[0];
        assert_eq!(model.pose.as_deref(), Some("A"));
        assert_eq!(model.designer.as_deref(), Some("Dragon Trapper's Lodge"));
        assert_eq!(model.files[0].pose.as_deref(), Some("A"));
    }

    #[test]
    fn rejects_an_unknown_major_version() {
        let mut manifest = sample();
        manifest.version = 999;
        assert!(!manifest.is_readable());
    }

    #[test]
    fn deduplicated_archive_round_trips_every_manifest_name() {
        let dir = std::env::temp_dir().join(format!("stlpack_3pk_rt_{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("src/knight/variant_b")).unwrap();
        std::fs::write(dir.join("src/knight/base.stl"), b"shared-base-bytes!").unwrap();
        std::fs::write(
            dir.join("src/knight/variant_b/base.stl"),
            b"shared-base-bytes!",
        )
        .unwrap();
        std::fs::write(dir.join("src/knight/body.stl"), b"unique-body-bytes!").unwrap();

        let archive_path = dir.join("knight.zip");
        let archive = std::fs::File::create(&archive_path).unwrap();
        let entries = crate::file::compressors::compress_files(
            &[dir.join("src/knight")],
            archive,
            None::<fn(u32) -> bool>,
        )
        .unwrap();
        assert!(entries.iter().any(|e| !e.stored), "dedup actually happened");

        let files: Vec<ManifestFile> = entries
            .iter()
            .map(|e| ManifestFile {
                name: e.name.clone(),
                checksum: e.checksum.clone(),
                size_bytes: e.size_bytes,
                pose: None,
                support_status: None,
            })
            .collect();

        let out = dir.join("out");
        extract_component_archive(&archive_path, &out, &files).unwrap();
        for name in ["base.stl", "variant_b/base.stl", "body.stl"] {
            assert!(out.join(name).is_file(), "{} restored", name);
        }
        assert_eq!(
            std::fs::read(out.join("base.stl")).unwrap(),
            std::fs::read(out.join("variant_b/base.stl")).unwrap()
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A single-file archive plus a `dest` root, for the traversal tests
    /// below — the archive contents are irrelevant, only the ManifestFile
    /// list under test matters.
    fn archive_and_dest(dir: &Path, tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let src = dir.join(format!("src_{}", tag));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("dummy.stl"), b"dummy").unwrap();
        let archive_path = dir.join(format!("{}.zip", tag));
        crate::file::compressors::compress_files(
            &[src],
            std::fs::File::create(&archive_path).unwrap(),
            None::<fn(u32) -> bool>,
        )
        .unwrap();
        (archive_path, dir.join(format!("dest_{}", tag)))
    }

    fn traversal_file(name: &str) -> ManifestFile {
        ManifestFile {
            name: name.into(),
            checksum: "blake3:whatever".into(),
            size_bytes: 0,
            pose: None,
            support_status: None,
        }
    }

    #[test]
    fn rejects_a_parent_dir_escape_in_a_manifest_name() {
        let dir = std::env::temp_dir().join(format!("stlpack_3pk_trav1_{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let (archive_path, dest) = archive_and_dest(&dir, "parent");
        let files = vec![traversal_file("../escape.stl")];
        let result = extract_component_archive(&archive_path, &dest, &files);
        assert!(matches!(result, Err(AppError::InvalidInput(_))));
        assert!(
            !dir.join("escape.stl").exists(),
            "must not write outside dest_dir"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An absolute manifest name must not be joined verbatim onto dest_dir
    /// (PathBuf::join replaces the base entirely for an absolute arg).
    #[test]
    fn rejects_an_absolute_path_in_a_manifest_name() {
        let dir = std::env::temp_dir().join(format!("stlpack_3pk_trav2_{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let (archive_path, dest) = archive_and_dest(&dir, "abs");
        let outside = dir.join("outside.stl");
        let evil_name = outside.to_string_lossy().into_owned();
        let files = vec![traversal_file(&evil_name)];
        let result = extract_component_archive(&archive_path, &dest, &files);
        assert!(matches!(result, Err(AppError::InvalidInput(_))));
        assert!(!outside.exists(), "must not write outside dest_dir");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A Windows drive-relative name ("C:evil.stl") is not `is_absolute()`
    /// in Rust's model, but carries a `Component::Prefix` that makes
    /// `PathBuf::join` replace the base — the guard must reject it too.
    #[test]
    fn rejects_a_windows_drive_relative_name() {
        let dir = std::env::temp_dir().join(format!("stlpack_3pk_trav3_{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let (archive_path, dest) = archive_and_dest(&dir, "drive");
        let files = vec![traversal_file("C:evil.stl")];
        let result = extract_component_archive(&archive_path, &dest, &files);
        assert!(matches!(result, Err(AppError::InvalidInput(_))));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hashes_a_file_with_the_blake3_prefix() {
        let dir = std::env::temp_dir().join(format!("stlpack_manifest_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.bin");
        std::fs::write(&path, b"hello").unwrap();
        let hash = hash_file(&path).unwrap();
        assert!(hash.starts_with("blake3:"));
        assert_eq!(hash, hash_file(&path).unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }
}
