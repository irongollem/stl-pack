//! Canonical staging for the release builder — the drift-prevention half of
//! the normalizer plan. The builder used to stage `[group/]model/` with
//! lowercase heuristic subfolders (`supported/`, `chitubox/`, `lychee/`),
//! so every release built in Plinth needed a cleanup pass the moment it was
//! imported into a library. Staging now emits the same shape the normalizer
//! converges on — `Model/Supported|Unsupported[/Variant]` with an
//! authoritative model.json in every leaf — so a packed release imports
//! already-normal: a scan + plan() after import finds nothing to move.
//!
//! The builder's unit of staging is one catalog MEMBER (one pose/build of a
//! model card). Members that share support and variant share a canonical
//! leaf — pose is metadata, not a folder — so staging must MERGE them:
//! their files land in one dir (name clashes resolved the way the
//! normalizer resolves them: identical twin dropped, else pose suffix, else
//! numbered) and the leaf gets ONE sidecar whose poses move down to file
//! level, exactly like normalize's write_leaf_json.

use crate::catalog::{layout, normalize, scanner};
use crate::error::AppError;
use crate::models::{FilePose, Release, StlModel};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Stage `members` (their model_files/images are absolute SOURCE paths)
/// into `release_path` in canonical layout. Returns one entry per LEAF
/// written: the merged sidecar model (paths now leaf-relative) and the
/// sidecar's release-relative path ('/'-separated, ready for release.json).
pub fn stage_models(
    release_path: &Path,
    release: &Release,
    members: &[StlModel],
) -> Result<Vec<(StlModel, String)>, AppError> {
    // The sidecars carry the release they are being packed INTO. Members
    // pulled from the catalog still remember their source release; leaving
    // that in would make the receiving library's normalizer file the model
    // under the OLD release the moment it was imported.
    let release_date = super::pack_manifest::canonical_date(&release.date);
    let release_designer = Some(release.designer.trim())
        .filter(|d| !d.is_empty())
        .map(String::from);

    struct Leaf {
        dir: PathBuf,
        /// Raw trimmed card name — becomes the sidecar's `name`, which is
        /// what the scanner groups members by.
        tier: String,
        grouped: bool,
        support: Option<String>,
        variant: Option<String>,
        members: Vec<usize>,
    }
    let mut leaves: Vec<Leaf> = Vec::new();

    for (index, member) in members.iter().enumerate() {
        let tier = member
            .group
            .as_deref()
            .map(str::trim)
            .filter(|g| !g.is_empty())
            .unwrap_or_else(|| member.name.trim())
            .to_string();
        let model_dir = release_path.join(layout::sanitize_segment(&tier));
        // Metadata decides the build folder; only a member that carries NO
        // support status falls back to the same signals the scanner reads
        // (pre-sliced formats, name tokens), so what we stamp into the
        // sidecar is what a rescan would have concluded anyway.
        let support = match member
            .support_status
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(raw) => canonical_support(raw),
            None => infer_support(&member.model_files),
        };
        let variant = member
            .variant
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(layout::title_case);
        let dir = layout::member_dir(&model_dir, support.as_deref(), variant.as_deref());
        match leaves.iter_mut().find(|leaf| leaf.dir == dir) {
            Some(leaf) => {
                leaf.grouped |= member.group.is_some();
                leaf.members.push(index);
            }
            None => leaves.push(Leaf {
                dir,
                tier,
                grouped: member.group.is_some(),
                support,
                variant,
                members: vec![index],
            }),
        }
    }

    let mut results = Vec::with_capacity(leaves.len());
    for leaf in &leaves {
        fs::create_dir_all(&leaf.dir)
            .map_err(|e| AppError::IoError(format!("failed to create model folder; {}", e)))?;
        let leaf_members: Vec<&StlModel> = leaf.members.iter().map(|&i| &members[i]).collect();
        let multi = leaf_members.len() > 1;

        let mut files_rel: Vec<String> = Vec::new();
        let mut images_rel: Vec<String> = Vec::new();
        let mut tags: Vec<String> = Vec::new();
        let mut file_poses: Vec<FilePose> = Vec::new();
        let push_pose = |pose: FilePose, out: &mut Vec<FilePose>| {
            if !out.iter().any(|existing| existing.name == pose.name) {
                out.push(pose);
            }
        };

        for member in &leaf_members {
            let pose = member
                .pose
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty());
            // original basename -> final name in the leaf, in file order
            let mut placed: Vec<(String, String)> = Vec::new();
            for source in &member.model_files {
                let source = Path::new(source);
                match place_into_leaf(&leaf.dir, source, pose)? {
                    Placement::Copied(name) => {
                        files_rel.push(name.clone());
                        placed.push((basename(source), name));
                    }
                    // byte-identical twin already landed (repeated bases
                    // across poses) — reference the shared copy
                    Placement::Twin(name) => placed.push((basename(source), name)),
                }
            }

            for (index, image) in member.images.iter().enumerate() {
                let source = Path::new(image);
                let named = super::storage::rename_image(
                    &layout::sanitize_segment(member.name.trim()),
                    source,
                    index,
                );
                match place_named_into_leaf(&leaf.dir, source, &named, pose)? {
                    Placement::Copied(name) => images_rel.push(name),
                    Placement::Twin(_) => {} // the earlier member's copy serves
                }
            }

            for tag in &member.tags {
                if !tags.contains(tag) {
                    tags.push(tag.clone());
                }
            }

            // Explicit per-file assignments travel (renamed files keep
            // theirs under the new name). When poses MERGE into one leaf,
            // each member's dir-level pose moves down to its own files —
            // write_leaf_json's rule: file poses beat a dir pose.
            let explicit: HashMap<&str, &FilePose> = member
                .file_poses
                .iter()
                .map(|fp| (fp.name.as_str(), fp))
                .collect();
            for (original, final_name) in &placed {
                if let Some(fp) = explicit.get(original.as_str()) {
                    push_pose(
                        FilePose {
                            name: final_name.clone(),
                            variant: fp.variant.clone(),
                            pose: fp.pose.clone(),
                            support_status: fp.support_status.clone(),
                        },
                        &mut file_poses,
                    );
                } else if multi {
                    if let Some(pose) = pose {
                        push_pose(
                            FilePose {
                                name: final_name.clone(),
                                variant: None,
                                pose: Some(pose.to_string()),
                                support_status: None,
                            },
                            &mut file_poses,
                        );
                    }
                }
            }
        }
        tags.sort();

        let pick = |get: fn(&StlModel) -> Option<&String>| -> Option<String> {
            leaf_members
                .iter()
                .filter_map(|m| get(m))
                .map(|s| s.trim().to_string())
                .find(|s| !s.is_empty())
        };
        // dir-level pose only describes a single-member leaf with no
        // file-level splits — otherwise the two mechanisms fight on rescan
        let dir_pose = if !multi && file_poses.is_empty() {
            pick(|m| m.pose.as_ref())
        } else {
            None
        };

        let sidecar_model = StlModel {
            id: Some(Uuid::new_v4()),
            name: leaf.tier.clone(),
            description: pick(|m| m.description.as_ref()),
            tags,
            images: images_rel,
            model_files: files_rel,
            group: leaf.grouped.then(|| leaf.tier.clone()),
            variant: leaf
                .variant
                .clone()
                .or_else(|| pick(|m| m.variant.as_ref()).map(|v| layout::title_case(&v))),
            pose: dir_pose,
            scale: pick(|m| m.scale.as_ref()),
            support_status: leaf
                .support
                .clone()
                .or_else(|| pick(|m| m.support_status.as_ref())),
            release_date: Some(release_date.clone()),
            designer: release_designer.clone().or_else(|| pick(|m| m.designer.as_ref())),
            sculptor: pick(|m| m.sculptor.as_ref()),
            release_name: Some(release.name.trim().to_string()),
            base_round_mm: pick(|m| m.base_round_mm.as_ref()),
            base_square_mm: pick(|m| m.base_square_mm.as_ref()),
            file_poses,
        };

        let sidecar_path = leaf.dir.join("model.json");
        let json = serde_json::to_string_pretty(&sidecar_model)?;
        super::writer::write_json(json, sidecar_path.clone())?;

        results.push((sidecar_model, release_relative(release_path, &sidecar_path)?));
    }

    Ok(results)
}

/// "supported"/"presupported"/"unsupported" (any case) -> the canonical
/// lowercase value the catalog stores. Unknown wording -> None: the files
/// stay at the model root and the raw string survives as pure metadata.
fn canonical_support(raw: &str) -> Option<String> {
    match layout::support_segment(Some(raw))? {
        "Supported" => Some("supported".to_string()),
        _ => Some("unsupported".to_string()),
    }
}

/// The support status a scan would infer for these files: a pre-sliced
/// format (.lys/.chitu) means presupported outright; otherwise the first
/// file NAME that carries a support token decides.
fn infer_support(file_paths: &[String]) -> Option<String> {
    let mut from_name: Option<&'static str> = None;
    for path in file_paths {
        let path = Path::new(path);
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if matches!(ext.as_str(), "lys" | "chitu" | "chitubox") {
            return Some("supported".to_string());
        }
        if from_name.is_none() {
            from_name = path
                .file_name()
                .and_then(|n| scanner::support_from_filename(&n.to_string_lossy()));
        }
    }
    from_name.map(String::from)
}

enum Placement {
    Copied(String),
    Twin(String),
}

fn basename(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn place_into_leaf(
    leaf: &Path,
    source: &Path,
    pose: Option<&str>,
) -> Result<Placement, AppError> {
    let name = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .ok_or_else(|| {
            AppError::InvalidInput(format!("Invalid file path: {}", source.display()))
        })?;
    place_named_into_leaf(leaf, source, &name, pose)
}

/// Land `source` in `leaf` under `name`, resolving clashes the normalizer's
/// way: a byte-identical twin is simply shared (no second copy), else a
/// pose suffix frees the name, else a numbered name — never silently
/// overwrite, never lose a file.
fn place_named_into_leaf(
    leaf: &Path,
    source: &Path,
    name: &str,
    pose: Option<&str>,
) -> Result<Placement, AppError> {
    let mut candidates: Vec<String> = vec![name.to_string()];
    if let Some(pose) = pose {
        let suffixed = layout::pose_suffixed_name(name, pose);
        if suffixed != name {
            candidates.push(suffixed);
        }
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem.to_string(), Some(ext.to_string())),
        _ => (name.to_string(), None),
    };
    candidates.extend((2..100).map(|n| match &ext {
        Some(ext) => format!("{} {}.{}", stem, n, ext),
        None => format!("{} {}", stem, n),
    }));

    for candidate in candidates {
        let dest = leaf.join(&candidate);
        if !dest.exists() {
            fs::copy(source, &dest).map_err(|e| {
                AppError::IoError(format!("failed to copy {}; {}", source.display(), e))
            })?;
            return Ok(Placement::Copied(candidate));
        }
        if normalize::same_content(source, &dest) {
            return Ok(Placement::Twin(candidate));
        }
    }
    Err(AppError::InvalidInput(format!(
        "Could not place '{}' in '{}': too many name clashes",
        name,
        leaf.display()
    )))
}

/// Release-relative sidecar path with '/' separators — the portable form
/// release.json references travel in (and zip entry names use).
fn release_relative(release_path: &Path, path: &Path) -> Result<String, AppError> {
    let rel = path.strip_prefix(release_path).map_err(|_| {
        AppError::InvalidInput(format!(
            "Path '{}' is not within release directory '{}'",
            path.display(),
            release_path.display()
        ))
    })?;
    Ok(rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_release() -> Release {
        Release {
            name: "Dread Swamp".to_string(),
            designer: "Bestiarum".to_string(),
            description: String::new(),
            date: "7/2026".to_string(),
            version: "1.0.0".to_string(),
            model_references: vec![],
            groups: vec![],
            release_dir: String::new(),
            images: vec![],
            other_files: vec![],
        }
    }

    fn member(name: &str, files: Vec<String>) -> StlModel {
        StlModel {
            id: None,
            name: name.to_string(),
            description: None,
            tags: vec![],
            images: vec![],
            model_files: files,
            group: None,
            variant: None,
            pose: None,
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

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "plinth_stage_{}_{}_{}",
            tag,
            std::process::id(),
            Uuid::new_v4()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn read_sidecar(path: &Path) -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    /// Two poses of one grouped model, both supported: ONE canonical leaf,
    /// one merged sidecar, poses demoted to file level, and the repeated
    /// base file shared instead of duplicated.
    #[test]
    fn merges_poses_into_one_supported_leaf() {
        let dir = scratch("poses");
        let sources = dir.join("src");
        fs::create_dir_all(sources.join("a")).unwrap();
        fs::create_dir_all(sources.join("b")).unwrap();
        fs::write(sources.join("a/galeb.stl"), b"pose a body").unwrap();
        fs::write(sources.join("a/base.stl"), b"shared base").unwrap();
        fs::write(sources.join("b/galeb.stl"), b"pose b body").unwrap();
        fs::write(sources.join("b/base.stl"), b"shared base").unwrap();

        let release_dir = dir.join("release");
        fs::create_dir_all(&release_dir).unwrap();
        let mut pose_a = member(
            "Galeb Duhr",
            vec![
                sources.join("a/galeb.stl").to_string_lossy().into_owned(),
                sources.join("a/base.stl").to_string_lossy().into_owned(),
            ],
        );
        pose_a.group = Some("Galeb Duhr".to_string());
        pose_a.pose = Some("A".to_string());
        pose_a.support_status = Some("presupported".to_string());
        let mut pose_b = member(
            "Galeb Duhr",
            vec![
                sources.join("b/galeb.stl").to_string_lossy().into_owned(),
                sources.join("b/base.stl").to_string_lossy().into_owned(),
            ],
        );
        pose_b.group = Some("Galeb Duhr".to_string());
        pose_b.pose = Some("B".to_string());
        pose_b.support_status = Some("supported".to_string());

        let staged =
            stage_models(&release_dir, &test_release(), &[pose_a, pose_b]).unwrap();

        assert_eq!(staged.len(), 1, "one leaf, not one dir per pose");
        let (model, sidecar_rel) = &staged[0];
        assert_eq!(sidecar_rel, "Galeb Duhr/Supported/model.json");

        let leaf = release_dir.join("Galeb Duhr/Supported");
        // pose A's files keep their names; pose B's clashing body got the
        // pose suffix; the byte-identical base landed once
        assert!(leaf.join("galeb.stl").is_file());
        assert!(leaf.join("galeb B.stl").is_file());
        assert!(leaf.join("base.stl").is_file());
        assert!(!leaf.join("base B.stl").exists(), "identical twin shared");

        assert_eq!(model.name, "Galeb Duhr");
        assert_eq!(model.pose, None, "merged leaf holds no dir-level pose");
        assert_eq!(model.support_status.as_deref(), Some("supported"));
        let sidecar = read_sidecar(&leaf.join("model.json"));
        let poses: Vec<(&str, &str)> = sidecar["file_poses"]
            .as_array()
            .unwrap()
            .iter()
            .map(|fp| {
                (
                    fp["name"].as_str().unwrap(),
                    fp["pose"].as_str().unwrap(),
                )
            })
            .collect();
        assert!(poses.contains(&("galeb.stl", "A")));
        assert!(poses.contains(&("galeb B.stl", "B")));
        assert!(
            poses.contains(&("base.stl", "A")),
            "the shared base keeps the first claimant's pose"
        );

        // the release being built overrides the members' source release
        assert_eq!(model.release_name.as_deref(), Some("Dread Swamp"));
        assert_eq!(model.release_date.as_deref(), Some("2026-07"));
        assert_eq!(model.designer.as_deref(), Some("Bestiarum"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn variant_and_unknown_support_place_canonically() {
        let dir = scratch("variants");
        let sources = dir.join("src");
        fs::create_dir_all(&sources).unwrap();
        fs::write(sources.join("knight.stl"), b"knight").unwrap();
        fs::write(sources.join("mystery.stl"), b"mystery").unwrap();

        let release_dir = dir.join("release");
        fs::create_dir_all(&release_dir).unwrap();
        let mut with_variant = member(
            "Knight",
            vec![sources.join("knight.stl").to_string_lossy().into_owned()],
        );
        with_variant.support_status = Some("unsupported".to_string());
        with_variant.variant = Some("sword + shield".to_string());
        with_variant.pose = Some("A".to_string());
        // no support status and no filename/format hints -> model root
        let unknown = member(
            "Mystery",
            vec![sources.join("mystery.stl").to_string_lossy().into_owned()],
        );

        let staged =
            stage_models(&release_dir, &test_release(), &[with_variant, unknown]).unwrap();

        let (knight, knight_rel) = &staged[0];
        assert_eq!(
            knight_rel,
            "Knight/Unsupported/Sword + Shield/model.json",
            "variant folder carries the Title Case convention"
        );
        assert_eq!(knight.variant.as_deref(), Some("Sword + Shield"));
        assert_eq!(
            knight.pose.as_deref(),
            Some("A"),
            "single-member leaf keeps its dir-level pose"
        );
        assert!(release_dir
            .join("Knight/Unsupported/Sword + Shield/knight.stl")
            .is_file());

        let (mystery, mystery_rel) = &staged[1];
        assert_eq!(mystery_rel, "Mystery/model.json");
        assert_eq!(mystery.support_status, None);
        assert!(release_dir.join("Mystery/mystery.stl").is_file());

        fs::remove_dir_all(&dir).ok();
    }

    /// THE guarantee this module exists for: a release staged canonically,
    /// packed, and imported into a library scans into a catalog the
    /// normalizer has ZERO work for. If this test fails, releases built in
    /// Plinth have started drifting from the cleaner's canon again.
    #[test]
    fn packed_release_imports_normal_form() {
        use crate::file::compressors::compress_files;
        use crate::file::pack_manifest::{build_manifest, PackedComponent};

        let dir = scratch("roundtrip");
        let sources = dir.join("src");
        fs::create_dir_all(&sources).unwrap();
        fs::write(sources.join("galeb_a.stl"), b"pose a").unwrap();
        fs::write(sources.join("galeb_b.stl"), b"pose b").unwrap();
        fs::write(sources.join("knight.stl"), b"knight").unwrap();

        // a two-pose supported group plus an unsupported variant model —
        // one of every canonical tier in a single release
        let staged_dir = dir.join("staged");
        fs::create_dir_all(&staged_dir).unwrap();
        let release = test_release();
        let mut pose_a = member(
            "Galeb Duhr",
            vec![sources.join("galeb_a.stl").to_string_lossy().into_owned()],
        );
        pose_a.group = Some("Galeb Duhr".to_string());
        pose_a.pose = Some("A".to_string());
        pose_a.support_status = Some("supported".to_string());
        let mut pose_b = member(
            "Galeb Duhr",
            vec![sources.join("galeb_b.stl").to_string_lossy().into_owned()],
        );
        pose_b.group = Some("Galeb Duhr".to_string());
        pose_b.pose = Some("B".to_string());
        pose_b.support_status = Some("supported".to_string());
        let mut knight = member(
            "Knight",
            vec![sources.join("knight.stl").to_string_lossy().into_owned()],
        );
        knight.support_status = Some("unsupported".to_string());
        knight.variant = Some("sword".to_string());
        stage_models(&staged_dir, &release, &[pose_a, pose_b, knight]).unwrap();
        fs::write(
            staged_dir.join("release.json"),
            serde_json::to_string(&release).unwrap(),
        )
        .unwrap();

        // pack exactly like compression_jobs: one archive per top-level
        // dir, then manifest.json + release.json into release.3pk
        let out = dir.join("packed");
        fs::create_dir_all(&out).unwrap();
        let mut components = Vec::new();
        for entry in fs::read_dir(&staged_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_dir())
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            let archive_path = out.join(format!("{}.zip", name));
            let entries = compress_files(
                &[entry.path()],
                fs::File::create(&archive_path).unwrap(),
                None::<fn(u32) -> bool>,
            )
            .unwrap();
            components.push(PackedComponent {
                name,
                archive_path,
                entries,
            });
        }
        let manifest = build_manifest(&staged_dir, &components, "0.1.0").unwrap();
        fs::write(staged_dir.join("manifest.json"), manifest.to_json().unwrap()).unwrap();
        compress_files(
            &[
                staged_dir.join("manifest.json"),
                staged_dir.join("release.json"),
            ],
            fs::File::create(out.join("release.3pk")).unwrap(),
            None::<fn(u32) -> bool>,
        )
        .unwrap();

        let library = dir.join("library");
        fs::create_dir_all(&library).unwrap();
        let outcome =
            crate::file::import::import_release(&out.join("release.3pk"), &library, None).unwrap();
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        assert!(
            Path::new(&outcome.dest_dir).ends_with("Bestiarum/2026-07 Dread Swamp"),
            "{}",
            outcome.dest_dir
        );

        // scan the library and ask the normalizer for work — there is none
        let scanned = crate::catalog::scanner::scan(
            &library,
            &std::sync::atomic::AtomicBool::new(false),
            &[],
            |_, _| {},
        )
        .unwrap();
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::catalog::db::test_init(&conn);
        crate::catalog::db::replace_catalog(
            &mut conn,
            &library.to_string_lossy(),
            &scanned.files,
            &scanned.models,
            &scanned.metadata_tags,
            &scanned.metadata_file_variants,
            &scanned.packs,
        )
        .unwrap();
        let plan =
            crate::catalog::normalize::plan(&conn, std::slice::from_ref(&library), None, None, None)
                .unwrap();
        assert!(plan.skipped.is_empty(), "{:?}", plan.skipped);
        assert_eq!(
            plan.total_ops,
            0,
            "imported release should be normal-form; planned: {:?}",
            plan.groups
                .iter()
                .map(|g| (&g.group_name, &g.ops))
                .collect::<Vec<_>>()
        );
        assert_eq!(plan.clean_groups, 2, "{:?}", plan.clean_names);

        fs::remove_dir_all(&dir).ok();
    }

    /// Support falls back to the scanner's own signals: a .lys is
    /// presupported by definition, a "_supported" name token counts, and
    /// the inferred value is written INTO the sidecar so a rescan agrees.
    #[test]
    fn infers_support_like_the_scanner_when_metadata_is_silent() {
        let dir = scratch("infer");
        let sources = dir.join("src");
        fs::create_dir_all(&sources).unwrap();
        fs::write(sources.join("bog.lys"), b"sliced").unwrap();
        fs::write(sources.join("hag_supported.stl"), b"tokened").unwrap();

        let release_dir = dir.join("release");
        fs::create_dir_all(&release_dir).unwrap();
        let sliced = member(
            "Bog",
            vec![sources.join("bog.lys").to_string_lossy().into_owned()],
        );
        let tokened = member(
            "Hag",
            vec![sources
                .join("hag_supported.stl")
                .to_string_lossy()
                .into_owned()],
        );

        let staged = stage_models(&release_dir, &test_release(), &[sliced, tokened]).unwrap();

        assert!(
            release_dir.join("Bog/Supported/bog.lys").is_file(),
            "pre-sliced format lands in Supported, no lychee/ subfolder"
        );
        assert_eq!(staged[0].0.support_status.as_deref(), Some("supported"));
        assert!(release_dir
            .join("Hag/Supported/hag_supported.stl")
            .is_file());
        assert_eq!(staged[1].0.support_status.as_deref(), Some("supported"));

        fs::remove_dir_all(&dir).ok();
    }
}
