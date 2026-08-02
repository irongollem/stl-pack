use crate::error::AppError;
use crate::models::Release;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;
use walkdir::WalkDir;

use super::{pack, FileRow, FileVariantRow, ModelRow, PackRow, IMAGE_EXTENSIONS, MODEL_EXTENSIONS};

/// A `model.json` sidecar as the scanner reads it: the classic StlModel
/// fields plus the rich 3pk metadata (all optional, so a plain sidecar still
/// parses — serde ignores fields we don't list, like model_files). This is
/// the read side of metadata portability: whatever a release was packed with
/// is restored on scan. See docs/3PK.md.
///
/// `pub(crate)`, fields included, so basecutter::commands' export sidecar
/// writer and its tests can parse a freshly written sidecar into the exact
/// type the scanner will later read it back as — one struct, no drift
/// between "what we write" and "what we read".
#[derive(Deserialize)]
pub(crate) struct ModelJson {
    #[serde(default)]
    pub(crate) id: Option<Uuid>,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) tags: Vec<String>,
    #[serde(default)]
    pub(crate) images: Vec<String>,
    #[serde(default)]
    pub(crate) variant: Option<String>,
    #[serde(default)]
    pub(crate) pose: Option<String>,
    #[serde(default)]
    pub(crate) scale: Option<String>,
    #[serde(default)]
    pub(crate) support_status: Option<String>,
    #[serde(default)]
    pub(crate) release_date: Option<String>,
    #[serde(default)]
    pub(crate) designer: Option<String>,
    #[serde(default)]
    pub(crate) sculptor: Option<String>,
    #[serde(default)]
    pub(crate) release_name: Option<String>,
    #[serde(default)]
    pub(crate) base_round_mm: Option<String>,
    #[serde(default)]
    pub(crate) base_square_mm: Option<String>,
    /// Chosen render orientation "x,y,z" (Blender euler degrees) — written
    /// by the render pipeline so re-renders skip repositioning. Additive.
    #[serde(default)]
    pub(crate) rotation: Option<String>,
    /// Measured printed size "WxDxH" in mm + part count, from the render
    /// pipeline's geometry pass. Additive.
    #[serde(default)]
    pub(crate) dims_mm: Option<String>,
    #[serde(default)]
    pub(crate) part_count: Option<u32>,
    /// Per-file variant/pose split, restored into file_variants on scan.
    #[serde(default)]
    pub(crate) file_poses: Vec<FilePoseJson>,
}

#[derive(Deserialize)]
pub(crate) struct FilePoseJson {
    /// File name relative to the model dir (matched by basename in its subtree).
    name: String,
    #[serde(default)]
    variant: Option<String>,
    #[serde(default)]
    pose: Option<String>,
    #[serde(default)]
    support_status: Option<String>,
}

pub struct ScanOutcome {
    pub files: Vec<FileRow>,
    pub models: Vec<ModelRow>,
    pub metadata_tags: Vec<(String, String)>,
    /// Per-file pose/variant assignments imported from model.json file_poses.
    pub metadata_file_variants: Vec<FileVariantRow>,
    /// Packed model dirs found via pack.json sidecars.
    pub packs: Vec<PackRow>,
}

/// Per-directory accumulator built during the walk.
#[derive(Default)]
struct DirInfo {
    model_files: u32,
    model_bytes: i64,
    first_image: Option<String>,
    metadata: Option<ModelJson>,
    /// A .lys/.chitu file was seen here — those formats only ship
    /// presupported, so the dir is "supported" even if nothing says so.
    has_presupported_format: bool,
    /// Support status read from a file NAME (the .stl case: the only
    /// ambiguous format, where creators often bake "supported" into the name).
    filename_support: Option<&'static str>,
}

/// Info from a release.json, applied to model dirs beneath it.
struct ReleaseInfo {
    name: String,
    designer: Option<String>,
}

/// Walk `root`, indexing every model file and assembling one model entry
/// per directory that directly contains model files. `on_progress` is
/// invoked periodically with (files_seen, current_dir).
pub fn scan(
    root: &Path,
    cancel: &AtomicBool,
    designers: &[String],
    mut on_progress: impl FnMut(u32, &str),
) -> Result<ScanOutcome, AppError> {
    if !root.is_dir() {
        return Err(AppError::NotFoundError(format!(
            "Catalog root '{}' is not a directory",
            root.display()
        )));
    }

    let mut files: Vec<FileRow> = Vec::new();
    // BTreeMap: deterministic order, and ancestor lookups for releases
    let mut dirs: BTreeMap<String, DirInfo> = BTreeMap::new();
    let mut releases: BTreeMap<String, ReleaseInfo> = BTreeMap::new();
    let mut packs: BTreeMap<String, pack::PackSidecar> = BTreeMap::new();
    let mut archive_dirs: Vec<String> = Vec::new();
    let mut seen: u32 = 0;

    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !is_hidden(e.path()))
        .filter_map(|e| e.ok())
    {
        if cancel.load(Ordering::SeqCst) {
            return Err(AppError::UserCancelled("Scan cancelled".to_string()));
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let Some(dir_path) = path.parent().map(|p| p.to_string_lossy().into_owned()) else {
            continue;
        };
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let extension = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        if MODEL_EXTENSIONS.contains(&extension.as_str()) {
            let metadata = entry.metadata().map_err(|e| {
                AppError::IoError(format!("Failed to stat {}: {}", path.display(), e))
            })?;
            let size_bytes = metadata.len() as i64;
            let modified_at = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);

            // Read support signals off the file before it's moved into the row
            let is_presupported_format = matches!(extension.as_str(), "lys" | "chitu" | "chitubox");
            let name_support = support_from_filename(&file_name);

            files.push(FileRow {
                path: path.to_string_lossy().into_owned(),
                dir_path: dir_path.clone(),
                file_name,
                extension,
                size_bytes,
                modified_at,
                archive_path: None,
                content_hash: None,
            });

            let info = dirs.entry(dir_path.clone()).or_default();
            info.model_files += 1;
            info.model_bytes += size_bytes;
            if is_presupported_format {
                info.has_presupported_format = true;
            }
            if let Some(status) = name_support {
                info.filename_support.get_or_insert(status);
            }

            seen += 1;
            if seen.is_multiple_of(250) {
                on_progress(seen, &dir_path);
            }
        } else if IMAGE_EXTENSIONS.contains(&extension.as_str()) {
            let info = dirs.entry(dir_path).or_default();
            if info.first_image.is_none() {
                info.first_image = Some(path.to_string_lossy().into_owned());
            }
        } else if file_name == "model.json" {
            if let Ok(parsed) = read_json::<ModelJson>(path) {
                dirs.entry(dir_path).or_default().metadata = Some(parsed);
            }
        } else if file_name == pack::PACK_SIDECAR_NAME {
            // A packed model: the sidecar stands in for the model files that
            // now live inside model.plinthpack (synthesized after the walk)
            if let Ok(parsed) = read_json::<pack::PackSidecar>(path) {
                if parsed.is_readable() {
                    packs.insert(dir_path, parsed);
                }
            }
        } else if file_name == pack::PACK_ARCHIVE_NAME {
            // Remembered so a lost/corrupt sidecar can be rebuilt after the
            // walk — without one the archive's model would silently vanish
            // from the catalog
            archive_dirs.push(dir_path);
        } else if file_name == "release.json" {
            if let Ok(parsed) = read_json::<Release>(path) {
                releases.insert(
                    dir_path,
                    ReleaseInfo {
                        name: parsed.name,
                        designer: Some(parsed.designer).filter(|d| !d.is_empty()),
                    },
                );
            }
        }
    }
    on_progress(seen, "");

    // Self-heal: an archive whose sidecar is missing or unreadable would
    // silently drop its model from the catalog. Rebuild the sidecar from the
    // archive itself (decompress + rehash — the slow path, but it only runs
    // for damaged dirs and permanently fixes them). ONLY when the dir has no
    // loose model files: with them present this is a crash-mid-pack orphan,
    // and pack_model's discard-and-repack path is the correct converger — a
    // rebuilt sidecar would freeze that dir in a half-packed limbo.
    for dir_path in archive_dirs {
        if cancel.load(Ordering::SeqCst) {
            return Err(AppError::UserCancelled("Scan cancelled".to_string()));
        }
        if packs.contains_key(&dir_path)
            || dirs.get(&dir_path).is_some_and(|info| info.model_files > 0)
        {
            continue;
        }
        on_progress(seen, &dir_path);
        if let Ok(sidecar) = pack::rebuild_sidecar(Path::new(&dir_path)) {
            packs.insert(dir_path, sidecar);
        }
    }

    // Materialize packed models into the same rows loose ones produce: each
    // pack.json entry becomes a FileRow at the path the file would occupy,
    // feeding the identical DirInfo accumulation so leaf models, support
    // inference and file_poses restoration work unchanged. Loose wins: a
    // path that was walked as a real file (crash mid-pack, ephemeral
    // extract) is already indexed and its pack entry is skipped.
    let mut pack_rows: Vec<PackRow> = Vec::new();
    if !packs.is_empty() {
        let walked: std::collections::HashSet<String> =
            files.iter().map(|f| f.path.clone()).collect();
        for (model_dir, sidecar) in &packs {
            if cancel.load(Ordering::SeqCst) {
                return Err(AppError::UserCancelled("Scan cancelled".to_string()));
            }
            let archive_path = Path::new(model_dir)
                .join(pack::PACK_ARCHIVE_NAME)
                .to_string_lossy()
                .into_owned();
            for entry in &sidecar.files {
                let path = pack::entry_disk_path(Path::new(model_dir), &entry.name);
                let path_str = path.to_string_lossy().into_owned();
                if walked.contains(path_str.as_str()) {
                    continue;
                }
                let Some(dir_path) = path.parent().map(|p| p.to_string_lossy().into_owned())
                else {
                    continue;
                };
                let file_name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let extension = path
                    .extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if !MODEL_EXTENSIONS.contains(&extension.as_str()) {
                    continue;
                }
                let is_presupported_format =
                    matches!(extension.as_str(), "lys" | "chitu" | "chitubox");
                let name_support = support_from_filename(&file_name);
                let size_bytes = entry.size_bytes as i64;

                files.push(FileRow {
                    path: path_str,
                    dir_path: dir_path.clone(),
                    file_name,
                    extension,
                    size_bytes,
                    modified_at: entry.modified_at,
                    archive_path: Some(archive_path.clone()),
                    // bare hex: the dup scanner's content_hash format —
                    // prefixed hashes would never group with loose twins
                    content_hash: Some(pack::bare_hash(&entry.checksum).to_string()),
                });

                let info = dirs.entry(dir_path).or_default();
                info.model_files += 1;
                info.model_bytes += size_bytes;
                if is_presupported_format {
                    info.has_presupported_format = true;
                }
                if let Some(status) = name_support {
                    info.filename_support.get_or_insert(status);
                }
            }
            pack_rows.push(PackRow {
                model_dir: model_dir.clone(),
                archive_path,
                archive_size_bytes: sidecar.archive_size_bytes as i64,
                archive_checksum: Some(sidecar.archive_checksum.clone()),
                packed_at: Some(sidecar.packed_at),
            });
        }
    }

    // Assemble one model per directory that directly holds model files
    let mut models = Vec::new();
    let mut metadata_tags = Vec::new();
    let mut metadata_file_variants = Vec::new();
    for (dir_path, info) in &dirs {
        if info.model_files == 0 {
            continue;
        }
        let release = nearest_release(&releases, dir_path);
        // An image beside the files, else one in a nested folder — creators
        // routinely ship renders in a "renders"/"images" subdir next to the
        // STLs, and those dirs hold no model files so they never become
        // models themselves; without this lookup their images were orphaned
        let own_image = info
            .first_image
            .clone()
            .or_else(|| descendant_image(&dirs, dir_path));
        let (name, description, uuid, source, preview, inferred) = match &info.metadata {
            Some(meta) => {
                // model.json image paths are relative to the model dir —
                // and often climb ("../render.png" from a build folder up
                // to the model root). The joined path must be NORMALIZED:
                // the asset protocol refuses raw `..` segments (traversal
                // guard), so a stored ".../Supported/../render.png" is a
                // preview that silently never loads even though is_file()
                // happily said yes.
                let preview = meta
                    .images
                    .first()
                    .map(|rel| normalize_join(Path::new(dir_path), rel))
                    .filter(|p| p.is_file())
                    .map(|p| p.to_string_lossy().into_owned())
                    .or(own_image);
                for tag in &meta.tags {
                    metadata_tags.push((dir_path.clone(), tag.clone()));
                }
                (
                    // sidecars are an interchange format — hand-edited or
                    // legacy ones ship stray whitespace ("Unsupported "
                    // taught us that lesson at the folder level)
                    meta.name.trim().to_string(),
                    meta.description
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from),
                    meta.id.map(|id| id.to_string()),
                    "metadata",
                    preview,
                    None,
                )
            }
            None => {
                let inferred = infer_model_identity(root, dir_path);
                // Files often sit in supported/unsupported subdirs with the
                // render one level up at the model root; borrow it when the
                // dir and its descendants have none.
                let preview = own_image
                    .or_else(|| ancestor_image(&dirs, dir_path, inferred.base_dir.as_deref()));
                (
                    inferred.name.clone(),
                    None,
                    None,
                    "heuristic",
                    preview,
                    Some(inferred),
                )
            }
        };

        // metadata models group under their own name (usually 1:1);
        // heuristic models under the inferred base
        let group_name = inferred
            .as_ref()
            .map(|i| i.group_name.clone())
            .unwrap_or_else(|| name.clone());

        // An enriched model.json is the authority — it carries the curation a
        // release was packed with — so its fields win over folder inference.
        // Values are trimmed on the way in; whitespace-only means unset.
        let meta = info.metadata.as_ref();
        let meta_field = |get: fn(&ModelJson) -> Option<String>| {
            meta.and_then(get)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };

        models.push(ModelRow {
            dir_path: dir_path.clone(),
            name,
            description,
            designer: meta_field(|m| m.designer.clone())
                .or_else(|| release.as_ref().and_then(|r| r.designer.clone()))
                .or_else(|| infer_designer(root, dir_path, designers)),
            release_name: meta_field(|m| m.release_name.clone())
                .or_else(|| release.map(|r| r.name.clone())),
            preview_path: preview,
            source: source.to_string(),
            uuid,
            file_count: info.model_files,
            total_size_bytes: info.model_bytes,
            variant: meta_field(|m| m.variant.clone())
                .or_else(|| inferred.as_ref().and_then(|i| i.variant.clone())),
            pose: meta_field(|m| m.pose.clone())
                .or_else(|| inferred.as_ref().and_then(|i| i.pose.clone())),
            scale: meta_field(|m| m.scale.clone()),
            // model.json wins; else the folder label; else a presupported-only
            // format (.lys/.chitu); else a hint baked into an .stl file name
            support_status: meta_field(|m| m.support_status.clone())
                .or_else(|| inferred.as_ref().and_then(|i| i.support_status.clone()))
                .or_else(|| {
                    info.has_presupported_format
                        .then(|| "supported".to_string())
                })
                .or_else(|| info.filename_support.map(String::from)),
            release_date: meta_field(|m| m.release_date.clone())
                .or_else(|| inferred.as_ref().and_then(|i| i.release_date.clone())),
            sculptor: meta_field(|m| m.sculptor.clone()),
            base_round_mm: meta_field(|m| m.base_round_mm.clone()),
            base_square_mm: meta_field(|m| m.base_square_mm.clone()),
            group_name: Some(group_name),
            rotation: meta_field(|m| m.rotation.clone()),
            dims_mm: meta_field(|m| m.dims_mm.clone()),
            part_count: meta.and_then(|m| m.part_count).map(|n| n.to_string()),
        });

        // Restore per-file poses: resolve each file_poses entry to a scanned
        // file (by basename within the model dir subtree) and seed it. The DB
        // import is INSERT OR IGNORE, so a user's own split always wins.
        if let Some(meta) = info.metadata.as_ref() {
            let subtree = format!("{}{}", dir_path, std::path::MAIN_SEPARATOR);
            for fp in &meta.file_poses {
                let has_facet = fp.variant.as_deref().is_some_and(|s| !s.is_empty())
                    || fp.pose.as_deref().is_some_and(|s| !s.is_empty());
                if !has_facet {
                    continue;
                }
                let base = fp.name.rsplit(['/', '\\']).next().unwrap_or(&fp.name);
                if let Some(file) = files
                    .iter()
                    .find(|f| f.file_name == base && f.path.starts_with(&subtree))
                {
                    metadata_file_variants.push(FileVariantRow {
                        path: file.path.clone(),
                        variant: fp.variant.clone(),
                        pose: fp.pose.clone(),
                        support_status: fp.support_status.clone(),
                    });
                }
            }
        }
    }

    Ok(ScanOutcome {
        files,
        models,
        metadata_tags,
        metadata_file_variants,
        packs: pack_rows,
    })
}

/// Join `rel` onto `base` and lexically resolve `.`/`..` segments so the
/// stored path is clean. Purely lexical on purpose: fs::canonicalize would
/// also resolve symlinks, silently rewriting NAS mount paths into whatever
/// the server exports.
fn normalize_join(base: &Path, rel: &str) -> std::path::PathBuf {
    let mut out = std::path::PathBuf::new();
    for comp in base.join(rel).components() {
        match comp {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// First image found in any subdirectory of `dir_path`. BTreeMap keys are
/// sorted, so all descendants sit in one contiguous range after the
/// prefix — no full-map scan.
fn descendant_image(dirs: &BTreeMap<String, DirInfo>, dir_path: &str) -> Option<String> {
    let prefix = format!("{}{}", dir_path, std::path::MAIN_SEPARATOR);
    dirs.range(prefix.clone()..)
        .take_while(|(key, _)| key.starts_with(&prefix))
        .find_map(|(_, info)| info.first_image.clone())
}

/// First image on the path from `dir_path`'s parent up to (and including)
/// `base_dir`. This is the common "model / {supported, unsupported} / files"
/// layout where the render lives at the model root beside the build folders,
/// not inside them. Bounded by base_dir — the model's identity root — so we
/// never borrow a release-level cover shared by unrelated models.
fn ancestor_image(
    dirs: &BTreeMap<String, DirInfo>,
    dir_path: &str,
    base_dir: Option<&str>,
) -> Option<String> {
    let base = base_dir?;
    // base == dir_path means the files sit at the identity root itself, so
    // its own image was already considered — nothing to borrow upward.
    if base == dir_path {
        return None;
    }
    let mut current = Path::new(dir_path).parent();
    while let Some(dir) = current {
        let key = dir.to_string_lossy();
        if let Some(image) = dirs
            .get(key.as_ref())
            .and_then(|info| info.first_image.clone())
        {
            return Some(image);
        }
        if key.as_ref() == base {
            break;
        }
        current = dir.parent();
    }
    None
}

// ---- stacked-folder identity inference (heuristic models only) ----

struct InferredModel {
    name: String,
    /// The base name without the pose suffix — variants of one model share
    /// it, which is what groups "galeb duhr A/B/C" onto one catalog card.
    group_name: String,
    /// The facet between support and pose (weapon/mount), read from a
    /// `Model/Support/Variant/Pose` tree — see the demotion rule below.
    variant: Option<String>,
    pose: Option<String>,
    support_status: Option<String>,
    release_date: Option<String>,
    /// The ancestor dir that gave the base name — the model's identity root.
    /// Images live there when the files sit in supported/unsupported subdirs,
    /// so it bounds how far up we borrow a preview from.
    base_dir: Option<String>,
}

/// Read identity out of "stacked" library structures like
/// `<release-05-2026>/<model>/<unsupported>/<A>`. The dirs that directly
/// hold the files are often packaging variants, and naming models after
/// them yields a catalog full of "A"s and "supported"s. Climb toward the
/// root until a segment says something, and keep what the packaging
/// segments told us as metadata instead of throwing it away.
fn infer_model_identity(root: &Path, dir_path: &str) -> InferredModel {
    let mut pose: Option<String> = None;
    let mut variant: Option<String> = None;
    let mut support_status: Option<String> = None;
    let mut release_date: Option<String> = None;
    let mut base_name: Option<String> = None;
    let mut base_dir: Option<String> = None;
    let mut variant_dir: Option<String> = None;
    // True when the name candidate sat ABOVE an already-found pose — the
    // `…/Spear/A` shape. That's the precondition for the demotion below.
    let mut name_above_pose = false;

    let mut current = Some(Path::new(dir_path));
    while let Some(dir) = current {
        if dir == root {
            break;
        }
        let Some(segment) = dir.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            break;
        };

        if release_date.is_none() {
            release_date = date_from_segment(&segment);
        }
        if let Some(status) = support_from_segment(&segment) {
            support_status.get_or_insert_with(|| status.to_string());
            // Demotion: in `Butterfly Cavalry/Supported/Spear/A` the climb
            // reads A (pose) then Spear (name candidate) then this support
            // segment — but a "name" wedged between a pose below and support
            // above is a weapon/mount facet, not the model. Demote it to
            // variant and keep climbing for the real name. The knights' flat
            // `Supported/A/Spear` shape is untouched: there the name is the
            // leaf itself, found before any pose, so name_above_pose stays
            // false and "Spear" remains the group (combine handles those).
            if name_above_pose && variant.is_none() {
                variant = base_name.take().inspect(|_| {
                    variant_dir = base_dir.take();
                });
            }
        } else if base_name.is_none() {
            // Pose markers only count BELOW the name: once a real name is
            // found, a short ancestor dir is a collection, not a variant
            if let Some(p) = pose_from_segment(&segment) {
                pose.get_or_insert(p);
            } else if !is_generic_segment(&segment) {
                // The pose is often baked into the name segment itself
                // ("galeb duhr A") rather than a nested folder. Peel a
                // trailing short marker off so the A/B/C variants collapse
                // into one model instead of three lookalike cards.
                let (base, trailing) = split_trailing_pose(&segment);
                if let Some(p) = trailing {
                    pose.get_or_insert(p);
                }
                name_above_pose = pose.is_some();
                base_name = Some(base);
                base_dir = Some(dir.to_string_lossy().into_owned());
            }
        }
        current = dir.parent();
    }

    // A demotion that never found a real name above the support tier (hit
    // the scan root first) is undone — better the old leaf-derived name
    // than an empty one.
    if base_name.is_none() && variant.is_some() {
        base_name = variant.take();
        base_dir = variant_dir.take();
    }

    // nothing but packaging all the way up: keep the old leaf-dir name
    let group_name = base_name.clone().unwrap_or_else(|| {
        prettify_segment(
            &Path::new(dir_path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| dir_path.to_string()),
        )
    });
    // Member display name carries the distinguishing facets: variant first
    // ("Butterfly Cavalry Spear A"), pose last — matching the tier order.
    let name = if base_name.is_some() {
        let mut parts = vec![group_name.clone()];
        parts.extend(variant.clone());
        parts.extend(pose.clone());
        parts.join(" ")
    } else {
        group_name.clone()
    };

    InferredModel {
        name,
        group_name,
        variant,
        pose,
        support_status,
        release_date,
        base_dir,
    }
}

/// The starter lexicon of common STL-mini studios, seeded into settings on
/// first run. Trees rarely spell the designer as a field, but very often
/// name a folder after the studio. Matching is on alphanumerics only, so
/// "dragon_trappers_lodge", "Dragon Trapper's Lodge" and
/// "DragonTrappersLodge" all hit. The user's saved list (settings) is what
/// the scanner actually uses — this is only the default the UI starts from.
pub const DEFAULT_DESIGNERS: &[&str] = &[
    "Dragon Trapper's Lodge",
    "Artisan Guild",
    "Titan Forge",
    "Lost Kingdom Miniatures",
    "Cast n Play",
    "Epic Miniatures",
    "Great Grimoire",
    "Archvillain Games",
    "Loot Studios",
    "DM Stash",
    "Bite the Bullet",
    "Clay Cyanide",
    "Ghamak",
    "Punga Miniatures",
    "Rescale Miniatures",
    "Papsikels",
    "Printed Obsession",
    "Twin Goddess Miniatures",
    "Fantasy Cult",
];

/// Lowercased alphanumerics only, so punctuation/spacing/underscores don't
/// block a lexicon match.
pub(crate) fn alnum_key(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// The first `designers`-listed studio named by any ancestor segment of
/// `dir_path` — a fallback designer for trees with no release.json to state
/// it outright. The list comes from settings (seeded with DEFAULT_DESIGNERS).
fn infer_designer(root: &Path, dir_path: &str, designers: &[String]) -> Option<String> {
    let mut current = Some(Path::new(dir_path));
    while let Some(dir) = current {
        if let Some(segment) = dir.file_name().map(|n| n.to_string_lossy().into_owned()) {
            let key = alnum_key(&segment);
            if let Some(hit) = designers.iter().find(|d| key.contains(&alnum_key(d))) {
                return Some(hit.clone());
            }
        }
        if dir == root {
            break;
        }
        current = dir.parent();
    }
    None
}

/// "galeb_duhr" reads like a filename; "galeb duhr" reads like a name.
/// Underscores are transfer-armor, not identity — swap them for spaces and
/// collapse the leftovers. Hyphens stay: they're often part of the name.
fn prettify_segment(segment: &str) -> String {
    segment
        .replace('_', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Slicer file formats a presupported model ships in. The format is the file
/// type, NOT a variant, so a "stl supported" folder is the same supported
/// model as "lys supported" — these are dropped before reading support.
const SLICER_FORMATS: &[&str] = &["stl", "stls", "lys", "chitu", "chitubox", "obj", "3mf"];

pub(crate) fn support_from_segment(segment: &str) -> Option<&'static str> {
    // Drop any slicer-format words so "stl supported", "lys presupported"
    // and "unsupported chitu" all read as their support status. presupported
    // means supports are present — same answer as supported.
    let core: String = segment
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !SLICER_FORMATS.iter().any(|f| f == w))
        .collect::<Vec<_>>()
        .join("");
    match core.as_str() {
        "supported" | "presupported" => Some("supported"),
        "unsupported" => Some("unsupported"),
        _ => None,
    }
}

/// Support status hinted by a file NAME (not a whole segment): creators
/// often tag an .stl "..._Supported.stl". A substring check, so it fires
/// mid-name — "unsupported" is tested first since it contains "supported".
pub(crate) fn support_from_filename(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    if lower.contains("unsupported") {
        Some("unsupported")
    } else if lower.contains("presupported") || lower.contains("supported") {
        Some("supported")
    } else {
        None
    }
}

/// Descriptive variant dirs; a lexicon rather than a length rule because
/// "minotaur" must stay a NAME while "standing" is a pose. Deliberately
/// conservative — anything it misses is one click away in the pose field.
const POSE_WORDS: &[&str] = &[
    "sitting",
    "standing",
    "kneeling",
    "crouching",
    "lying",
    "mounted",
    "dismounted",
    "riding",
    "walking",
    "running",
    "flying",
    "jumping",
    "attacking",
    "charging",
    "casting",
    "shooting",
    "idle",
    "resting",
];

/// Variant markers, not names: "A", "B2", "01", "pose 3", "sitting",
/// "on a horse".
fn pose_from_segment(segment: &str) -> Option<String> {
    let lower = segment.trim().to_lowercase();
    // explicit "pose <x>" prefix takes whatever follows it
    if let Some(rest) = lower.strip_prefix("pose") {
        let rest = rest.trim_start_matches([' ', '-', '_']);
        if !rest.is_empty() {
            return Some(rest.to_uppercase());
        }
    }
    // short codes: A, B2, 01
    if !lower.is_empty() && lower.len() <= 2 && lower.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Some(lower.to_uppercase());
    }
    // descriptive poses keep their wording ("on a horse", not "ON A HORSE")
    let normalized = prettify_segment(&lower.replace('-', " "));
    if POSE_WORDS.contains(&normalized.as_str()) || normalized.starts_with("on ") {
        return Some(normalized);
    }
    None
}

/// Peels a trailing pose marker off a name segment when the pose is baked
/// into the name rather than a nested folder: "galeb duhr A" ->
/// ("galeb duhr", Some("A")). Conservative: only a single trailing short
/// marker (a letter, a 1-2 char code, or "pose X"), and only when a
/// meaningful base name survives in front of it — so "warhammer 40k" and
/// "st b" stay whole, but "galeb duhr A/B/C" collapse into one model.
/// Folder inference is only a default here; user metadata still overrides it.
fn split_trailing_pose(segment: &str) -> (String, Option<String>) {
    let pretty = prettify_segment(segment);
    let Some(split_at) = pretty.rfind(' ') else {
        return (pretty, None);
    };
    let base = pretty[..split_at].trim();
    let marker = pretty[split_at + 1..].trim();
    // The base must still read like a name, not a stray initial or another
    // packaging word left behind once the marker is gone.
    if base.chars().count() >= 3 && !is_generic_segment(base) {
        if let Some(pose) = pose_from_segment(marker) {
            return (base.to_string(), Some(pose));
        }
    }
    (pretty, None)
}

/// Container words that describe packaging, not the model.
fn is_generic_segment(segment: &str) -> bool {
    let normalized = segment.trim().to_lowercase();
    matches!(
        normalized.as_str(),
        "stl" | "stls" | "obj" | "files" | "parts" | "lys" | "chitubox"
    ) || support_from_segment(&normalized).is_some()
        || pose_from_segment(&normalized).is_some()
}

/// A MM-YYYY or YYYY-MM digit pair anywhere in the segment, e.g.
/// "dungeon_classics-05-2026" -> "2026-05".
fn date_from_segment(segment: &str) -> Option<String> {
    let tokens: Vec<&str> = segment
        .split(|c: char| !c.is_ascii_digit())
        .filter(|t| !t.is_empty())
        .collect();
    for pair in tokens.windows(2).rev() {
        let (month_token, year_token) = match (pair[0].len(), pair[1].len()) {
            (2, 4) => (pair[0], pair[1]),
            (4, 2) => (pair[1], pair[0]),
            _ => continue,
        };
        if let (Ok(month), Ok(year)) = (month_token.parse::<u32>(), year_token.parse::<u32>()) {
            if (1..=12).contains(&month) && (1900..=2200).contains(&year) {
                return Some(format!("{:04}-{:02}", year, month));
            }
        }
    }
    None
}

pub(crate) fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .map(|n| n.to_string_lossy().starts_with('.'))
        .unwrap_or(false)
}

pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, AppError> {
    let text = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

/// The release.json in the closest ancestor directory, if any.
fn nearest_release<'a>(
    releases: &'a BTreeMap<String, ReleaseInfo>,
    dir_path: &str,
) -> Option<&'a ReleaseInfo> {
    let mut current = Some(Path::new(dir_path));
    while let Some(dir) = current {
        if let Some(info) = releases.get(&dir.to_string_lossy().into_owned()) {
            return Some(info);
        }
        current = dir.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn scans_metadata_and_heuristic_models() {
        let root = std::env::temp_dir().join(format!("stlpack_scan_test_{}", std::process::id()));
        fs::create_dir_all(root.join("release/newt")).unwrap();
        fs::create_dir_all(root.join("loose")).unwrap();

        fs::write(root.join("release/newt/newt.stl"), b"solid fake").unwrap();
        fs::write(root.join("release/newt/newt-main.png"), b"png").unwrap();
        fs::write(
            root.join("release/newt/model.json"),
            r#"{"id":null,"name":"Giant Newt","description":"big","tags":["amphibian"],"images":["newt-main.png"],"model_files":["newt.stl"],"group":null}"#,
        )
        .unwrap();
        fs::write(
            root.join("release/release.json"),
            r#"{"name":"Critterfolk","designer":"DTL","description":"","date":"06/2026","version":"1","model_references":[],"groups":[],"release_dir":"","images":[],"other_files":[]}"#,
        )
        .unwrap();
        fs::write(root.join("loose/dragon.stl"), b"solid fake dragon").unwrap();
        fs::write(root.join("loose/notes.txt"), b"ignore me").unwrap();

        let cancel = AtomicBool::new(false);
        let outcome = scan(&root, &cancel, &[], |_, _| {}).unwrap();

        assert_eq!(outcome.files.len(), 2, "only model files are indexed");

        let newt = outcome
            .models
            .iter()
            .find(|m| m.name == "Giant Newt")
            .expect("metadata model");
        assert_eq!(newt.source, "metadata");
        assert_eq!(newt.release_name.as_deref(), Some("Critterfolk"));
        assert_eq!(newt.designer.as_deref(), Some("DTL"));
        assert!(newt
            .preview_path
            .as_deref()
            .unwrap()
            .ends_with("newt-main.png"));

        let dragon = outcome
            .models
            .iter()
            .find(|m| m.name == "loose")
            .expect("heuristic model named after its dir");
        assert_eq!(dragon.source, "heuristic");

        assert_eq!(
            outcome.metadata_tags,
            vec![(
                root.join("release/newt").to_string_lossy().into_owned(),
                "amphibian".to_string()
            )]
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn imports_rich_metadata_from_model_json() {
        // the 3pk read side: an enriched model.json restores the curation a
        // release was packed with, winning over folder inference
        let root = std::env::temp_dir().join(format!("stlpack_richmeta_{}", std::process::id()));
        let dir = root.join("knight");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("knight.stl"), b"solid").unwrap();
        // values deliberately padded: sidecars are hand-editable and the
        // import must trim — the exact-match asserts below prove it
        fs::write(
            dir.join("model.json"),
            r#"{"id":null,"name":" Knight ","description":"a knight","tags":["hero"],
                "images":[],"model_files":["knight.stl"],"group":"Knights",
                "variant":"sword","pose":"charging ","scale":" 32mm",
                "support_status":"unsupported","release_date":" 2026-05 ",
                "designer":" Dragon Trapper's Lodge ","sculptor":"A. Artist",
                "release_name":"Order of the Unicorn "}"#,
        )
        .unwrap();

        let cancel = AtomicBool::new(false);
        let outcome = scan(&root, &cancel, &[], |_, _| {}).unwrap();
        let m = outcome
            .models
            .iter()
            .find(|m| m.name == "Knight")
            .expect("metadata model");
        assert_eq!(m.source, "metadata");
        assert_eq!(m.variant.as_deref(), Some("sword"));
        assert_eq!(m.pose.as_deref(), Some("charging"));
        assert_eq!(m.scale.as_deref(), Some("32mm"));
        assert_eq!(m.support_status.as_deref(), Some("unsupported"));
        assert_eq!(m.designer.as_deref(), Some("Dragon Trapper's Lodge"));
        assert_eq!(m.sculptor.as_deref(), Some("A. Artist"));
        assert_eq!(m.release_name.as_deref(), Some("Order of the Unicorn"));
        assert_eq!(m.release_date.as_deref(), Some("2026-05"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn relative_image_refs_normalize_to_clean_paths() {
        // The normalizer's sidecars reference the model-root render from a
        // build folder as "../render.png". Storing the joined path RAW kept
        // the ".." segment — and the asset protocol refuses traversal
        // segments, so every such preview silently failed to load in the
        // UI (broken card image after each cleanup) while is_file() and
        // the DB row both looked perfectly healthy.
        let root = std::env::temp_dir().join(format!("stlpack_relimg_{}", std::process::id()));
        let sup = root.join("ashtok/Supported");
        fs::create_dir_all(&sup).unwrap();
        fs::write(sup.join("ashtok.stl"), b"solid").unwrap();
        fs::write(root.join("ashtok/render.png"), b"png").unwrap();
        fs::write(
            sup.join("model.json"),
            r#"{"name":"Ashtok","images":["../render.png"]}"#,
        )
        .unwrap();

        let cancel = AtomicBool::new(false);
        let outcome = scan(&root, &cancel, &[], |_, _| {}).unwrap();
        let m = outcome
            .models
            .iter()
            .find(|m| m.name == "Ashtok")
            .expect("metadata model");
        let preview = m.preview_path.as_deref().expect("preview resolved");
        assert!(
            !preview.contains(".."),
            "preview path must be normalized: {}",
            preview
        );
        assert_eq!(
            preview,
            root.join("ashtok/render.png").to_string_lossy(),
            "must point at the real file"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn imports_file_poses_from_model_json() {
        // a dump folder's split, carried in model.json, resolves each entry to
        // a scanned file path (by basename) so it can seed file_variants
        let root = std::env::temp_dir().join(format!("stlpack_fileposes_{}", std::process::id()));
        let dir = root.join("knights");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("sword_a.stl"), b"s").unwrap();
        fs::write(dir.join("bow_b.stl"), b"b").unwrap();
        fs::write(dir.join("base.stl"), b"x").unwrap();
        fs::write(
            dir.join("model.json"),
            r#"{"name":"Knights","file_poses":[
                {"name":"sword_a.stl","variant":"sword","pose":"1"},
                {"name":"bow_b.stl","variant":"bow","pose":"2","support_status":"unsupported"},
                {"name":"base.stl"}
            ]}"#,
        )
        .unwrap();

        let cancel = AtomicBool::new(false);
        let outcome = scan(&root, &cancel, &[], |_, _| {}).unwrap();
        // base.stl carries no facet, so it's skipped; the other two resolve
        assert_eq!(outcome.metadata_file_variants.len(), 2);
        let sword = outcome
            .metadata_file_variants
            .iter()
            .find(|f| f.variant.as_deref() == Some("sword"))
            .unwrap();
        assert!(sword.path.ends_with("sword_a.stl"));
        assert_eq!(sword.pose.as_deref(), Some("1"));
        let bow = outcome
            .metadata_file_variants
            .iter()
            .find(|f| f.variant.as_deref() == Some("bow"))
            .unwrap();
        assert_eq!(bow.support_status.as_deref(), Some("unsupported"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn infers_identity_from_stacked_folders() {
        let root = Path::new("/lib");

        // the minihoard shape that motivated this:
        // release-with-date / model / support / pose
        let inferred = infer_model_identity(
            root,
            "/lib/dungeon_classics-05-2026/galeb_duhr/unsupported/A",
        );
        assert_eq!(inferred.name, "galeb duhr A", "underscores prettified");
        assert_eq!(inferred.group_name, "galeb duhr");
        assert_eq!(inferred.pose.as_deref(), Some("A"));
        assert_eq!(inferred.support_status.as_deref(), Some("unsupported"));
        assert_eq!(inferred.release_date.as_deref(), Some("2026-05"));

        // presupported counts as supported; "pose 2" is a variant marker
        let inferred = infer_model_identity(root, "/lib/rats/pre-supported/pose 2");
        assert_eq!(inferred.name, "rats 2");
        assert_eq!(inferred.group_name, "rats", "poses share the group");
        assert_eq!(inferred.pose.as_deref(), Some("2"));
        assert_eq!(inferred.support_status.as_deref(), Some("supported"));

        // a plain named dir stays exactly what it was
        let inferred = infer_model_identity(root, "/lib/loose");
        assert_eq!(inferred.name, "loose");
        assert!(inferred.pose.is_none());
        assert!(inferred.support_status.is_none());

        // packaging all the way to the root: fall back to the leaf name
        let inferred = infer_model_identity(root, "/lib/supported");
        assert_eq!(inferred.name, "supported");

        // a short ancestor above the name is a collection, not a pose
        let inferred = infer_model_identity(root, "/lib/xx/minotaur/stls");
        assert_eq!(inferred.name, "minotaur");
        assert!(inferred.pose.is_none());

        // descriptive poses: lexicon words and "on ..." phrases are
        // variants, but an unknown word stays a NAME (a "sitting" dir must
        // never become the model — or every mini's sitting pose would
        // merge into one giant "sitting" group)
        let inferred = infer_model_identity(root, "/lib/knight/supported/on_a_horse");
        assert_eq!(inferred.name, "knight on a horse");
        assert_eq!(inferred.pose.as_deref(), Some("on a horse"));
        let inferred = infer_model_identity(root, "/lib/knight/unsupported/standing");
        assert_eq!(inferred.name, "knight standing");
        assert_eq!(inferred.pose.as_deref(), Some("standing"));
        assert_eq!(inferred.group_name, "knight");
        let inferred = infer_model_identity(root, "/lib/creatures/minotaur");
        assert_eq!(inferred.name, "minotaur", "unknown words are names");

        // pose baked into the name segment (no nested pose folder): the
        // trailing marker peels off so A/B/C collapse into one "galeb duhr"
        let inferred = infer_model_identity(root, "/lib/dungeon_classics/galeb duhr A");
        assert_eq!(inferred.name, "galeb duhr A");
        assert_eq!(inferred.group_name, "galeb duhr", "poses share the group");
        assert_eq!(inferred.pose.as_deref(), Some("A"));
        let inferred = infer_model_identity(root, "/lib/dungeon_classics/galeb_duhr_B2");
        assert_eq!(inferred.pose.as_deref(), Some("B2"));
        assert_eq!(inferred.group_name, "galeb duhr");

        // but a trailing token that isn't a short marker stays part of the
        // name, and a base too thin to be a name is left whole
        let inferred = infer_model_identity(root, "/lib/tanks/warhammer 40k");
        assert_eq!(inferred.name, "warhammer 40k");
        assert!(inferred.pose.is_none());
        let inferred = infer_model_identity(root, "/lib/misc/st b");
        assert_eq!(inferred.name, "st b", "base too short to split");
        assert!(inferred.pose.is_none());

        // a slicer-format prefix on a support folder is still that support
        // status, and the model name comes from the parent, so "stl
        // supported" and "lys supported" collapse onto one "dryad dragon"
        let inferred = infer_model_identity(root, "/lib/dryad dragon/stl supported");
        assert_eq!(inferred.name, "dryad dragon");
        assert_eq!(inferred.support_status.as_deref(), Some("supported"));
        let inferred = infer_model_identity(root, "/lib/dryad dragon/lys presupported");
        assert_eq!(inferred.name, "dryad dragon");
        assert_eq!(inferred.support_status.as_deref(), Some("supported"));
        let inferred = infer_model_identity(root, "/lib/dryad dragon/unsupported chitu");
        assert_eq!(inferred.support_status.as_deref(), Some("unsupported"));
    }

    #[test]
    fn a_name_between_support_and_pose_is_a_variant_not_a_model() {
        let root = Path::new("/lib");

        // Model / Support / Variant / Pose (DTL's cavalry shape): "Spear" is
        // a weapon facet of Butterfly Cavalry, not a model called Spear —
        // grouping it as one merged every spear-armed model in the library
        let inferred =
            infer_model_identity(root, "/lib/unicorn_pt.1/Butterfly Cavalry/Supported/Spear/A");
        assert_eq!(inferred.group_name, "Butterfly Cavalry");
        assert_eq!(inferred.variant.as_deref(), Some("Spear"));
        assert_eq!(inferred.pose.as_deref(), Some("A"));
        assert_eq!(inferred.support_status.as_deref(), Some("supported"));
        assert_eq!(inferred.name, "Butterfly Cavalry Spear A");

        // The flat release-root shape (Support / Pose / Weapon) does NOT
        // demote: the name is the leaf, found before any pose, so combines
        // keyed on these weapon groups keep working
        let inferred = infer_model_identity(root, "/lib/unicorn_pt.1/Supported/A/Spear");
        assert_eq!(inferred.group_name, "Spear");
        assert!(inferred.variant.is_none());

        // A demotion with nothing nameable above the support tier is undone
        // (scan root reached): better the leaf-derived name than none
        let inferred = infer_model_identity(root, "/lib/Supported/Spear/A");
        assert_eq!(inferred.group_name, "Spear");
        assert!(inferred.variant.is_none());
        assert_eq!(inferred.pose.as_deref(), Some("A"));
    }

    #[test]
    fn support_reads_through_slicer_format_labels() {
        assert_eq!(support_from_segment("supported"), Some("supported"));
        assert_eq!(support_from_segment("pre-supported"), Some("supported"));
        assert_eq!(support_from_segment("stl supported"), Some("supported"));
        assert_eq!(support_from_segment("lys_presupported"), Some("supported"));
        assert_eq!(support_from_segment("unsupported stl"), Some("unsupported"));
        // a bare format or a real name is not a support folder
        assert_eq!(support_from_segment("stl"), None);
        assert_eq!(support_from_segment("supported dragon"), None);
    }

    #[test]
    fn support_read_from_stl_file_names() {
        assert_eq!(
            support_from_filename("CopperDragon_Body_Supported.stl"),
            Some("supported")
        );
        assert_eq!(
            support_from_filename("gob_a_UNSUPPORTED.stl"),
            Some("unsupported"),
            "unsupported wins even though it contains 'supported'"
        );
        assert_eq!(
            support_from_filename("presupported-torso.stl"),
            Some("supported")
        );
        assert_eq!(support_from_filename("dryad_dragon_head.stl"), None);
    }

    #[test]
    fn heuristic_variants_borrow_the_render_at_their_model_root() {
        // model / {supported, unsupported} / files, with the render sitting
        // at the model root beside the build folders — the layout whose
        // images the scanner used to drop on the floor.
        let root =
            std::env::temp_dir().join(format!("stlpack_ancestor_img_{}", std::process::id()));
        let model_root = root.join("goblin");
        fs::create_dir_all(model_root.join("supported")).unwrap();
        fs::create_dir_all(model_root.join("unsupported")).unwrap();
        fs::write(model_root.join("supported/gob_a.stl"), b"solid").unwrap();
        fs::write(model_root.join("unsupported/gob_a.stl"), b"solid").unwrap();
        fs::write(model_root.join("goblin-render.png"), b"png").unwrap();

        let cancel = AtomicBool::new(false);
        let outcome = scan(&root, &cancel, &[], |_, _| {}).unwrap();

        assert!(!outcome.models.is_empty());
        for model in &outcome.models {
            assert!(
                model
                    .preview_path
                    .as_deref()
                    .unwrap_or_default()
                    .ends_with("goblin-render.png"),
                "variant {} should borrow the root render, got {:?}",
                model.dir_path,
                model.preview_path
            );
        }

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn infers_designer_from_a_studio_folder() {
        let root = std::env::temp_dir().join(format!("stlpack_designer_{}", std::process::id()));
        // a studio folder, spelled with underscores and no apostrophe
        let dir = root.join("dragon_trappers_lodge").join("goblin");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("gob.stl"), b"solid").unwrap();

        let designers: Vec<String> = DEFAULT_DESIGNERS.iter().map(|s| s.to_string()).collect();
        let cancel = AtomicBool::new(false);
        let outcome = scan(&root, &cancel, &designers, |_, _| {}).unwrap();

        let goblin = outcome
            .models
            .iter()
            .find(|m| m.name.to_lowercase().contains("goblin"))
            .expect("goblin model");
        assert_eq!(goblin.designer.as_deref(), Some("Dragon Trapper's Lodge"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn root_folder_itself_can_name_the_designer() {
        // The multi-root workflow adds a designer's folder AS a root, so the
        // designer signal is the root's own basename — the upward walk must
        // check it before stopping, and the model name must not absorb it.
        let base = std::env::temp_dir().join(format!("stlpack_rootdes_{}", std::process::id()));
        let root = base.join("dm_stash");
        let dir = root.join("2026-01 dungeon").join("mimic");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("mimic.stl"), b"solid").unwrap();

        let designers: Vec<String> = DEFAULT_DESIGNERS.iter().map(|s| s.to_string()).collect();
        let cancel = AtomicBool::new(false);
        let outcome = scan(&root, &cancel, &designers, |_, _| {}).unwrap();

        let mimic = outcome
            .models
            .iter()
            .find(|m| m.name.to_lowercase().contains("mimic"))
            .expect("mimic model");
        assert_eq!(mimic.designer.as_deref(), Some("DM Stash"));

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn plinth_bases_export_layout_scans_as_six_separate_models() {
        // Mirrors basecutter::commands::export_cuts's on-disk layout for a
        // six-base Base Cutter "Add to catalog" export, one cut per folder:
        // {root}/Plinth Bases/{YYYY-MM group}/{cut stem}/{cut stem}.stl
        // with a minimal {name, designer: "Plinth Bases"} model.json per
        // per-cut folder. Confirms the general rule this bug turned on: a
        // sidecar'd folder is its own model — a name-suffix heuristic like
        // "foo-1, foo-2, ..." never gets a chance to merge them, because
        // metadata models skip folder inference (`inferred` stays None)
        // entirely and take group_name straight from the sidecar's name.
        // (The actual "one model, six parts" bug the user hit was a
        // DIFFERENT bug one layer up, in export_cuts itself — see
        // basecutter::commands::tests::
        // distinct_bases_sharing_a_default_stem_scan_as_separate_models.)
        let root =
            std::env::temp_dir().join(format!("stlpack_plinth_layout_{}", std::process::id()));
        let release_dir = root.join("Plinth Bases").join("2026-07 Sunday Batch");
        for n in 1..=6 {
            let stem = format!("round285-{n}");
            let dir = release_dir.join(&stem);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(format!("{stem}.stl")), b"solid fake").unwrap();
            fs::write(
                dir.join("model.json"),
                format!(r#"{{"name":"{stem}","designer":"Plinth Bases"}}"#),
            )
            .unwrap();
        }

        let cancel = AtomicBool::new(false);
        let outcome = scan(&root, &cancel, &[], |_, _| {}).unwrap();

        assert_eq!(outcome.models.len(), 6, "each per-cut folder is its own model");
        for model in &outcome.models {
            assert_eq!(model.file_count, 1);
            assert_eq!(model.source, "metadata");
            // group_name must stay per-model, not collapse "round285-1" ..
            // "round285-6" onto a shared "round285" group
            assert_eq!(model.group_name.as_deref(), Some(model.name.as_str()));
        }

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn date_from_segment_handles_both_orders_and_junk() {
        assert_eq!(
            date_from_segment("dungeon_classics-05-2026").as_deref(),
            Some("2026-05")
        );
        assert_eq!(
            date_from_segment("2025-11 heroes").as_deref(),
            Some("2025-11")
        );
        assert_eq!(date_from_segment("warhammer 40k"), None);
        assert_eq!(date_from_segment("release-13-2026"), None, "month 13");
        assert_eq!(date_from_segment("plain name"), None);
    }
}
