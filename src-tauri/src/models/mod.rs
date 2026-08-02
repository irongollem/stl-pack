pub(crate) mod events;

use crate::basecutter::cutters::{CutterKind, MagnetSpec};
use serde::{Deserialize, Serialize};
use specta::Type;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct Settings {
    pub scratch_dir: Option<String>,
    pub target_dir: Option<String>,
    pub compression_type: Option<CompressionType>,
    pub chunk_size: Option<u32>,
    pub max_compression_threads: Option<u32>,
    pub blender_path: Option<String>,
    /// Legacy single catalog folder. Read-only compatibility: it seeds
    /// catalog_roots on first load after the multi-root update and mirrors
    /// roots[0] so an older build opening the same store still works.
    pub catalog_root: Option<String>,
    /// The catalog folders. Scans and the roots UI operate on this list;
    /// entries must not nest inside each other (enforced on add).
    /// serde(default): a not-yet-updated frontend sends Settings without
    /// this key, and its saves must not start failing.
    #[serde(default)]
    pub catalog_roots: Option<Vec<String>>,
    /// Optional staging target: when set, Clean up builds every group's
    /// canonical layout HERE, draining the raw folders into it. Unset =
    /// each group cleans up inside its own folder. Must name one of
    /// catalog_roots; validated at use and cleared when its folder is
    /// removed.
    #[serde(default)]
    pub catalog_primary_root: Option<String>,
    /// Studios the scanner recognizes in folder names to infer a designer.
    /// Seeded from scanner::DEFAULT_DESIGNERS on first load; user-editable.
    pub known_designers: Option<Vec<String>>,
    /// What the catalog's print button does: "open-in-slicer" (default —
    /// hand the files to the OS-default slicer app) or "reveal-folder"
    /// (the drag-it-yourself flow for people juggling several slicers).
    pub print_action: Option<String>,
    /// Release-builder fields the user asked to keep across drafts (the
    /// "remember" checkboxes), keyed by field id — e.g. "designer" so
    /// creators don't retype their own name every release.
    pub release_field_defaults: Option<std::collections::HashMap<String, String>>,
    /// Zstd level for compressed-at-rest packing (None = 3, zstd's default:
    /// near-Deflate speed, much better ratio). Advanced knob; -7..=22.
    pub pack_level: Option<i32>,
    /// After a packed model's files were extracted for printing/preview,
    /// remove them again once the action is done (None = true).
    pub pack_cleanup_after: Option<bool>,
    /// The managed Blender version (e.g. "5.1.2") whose first-run setup the
    /// user last completed or dismissed. A version string, not a bool, so
    /// bumping the pin re-offers the dialog exactly once.
    #[serde(default)]
    pub blender_setup_acknowledged: Option<String>,
    /// The scale-reference figure ("banana for scale"): a user-supplied STL
    /// rendered in grey beside the model when the studio toggle is on. Not
    /// bundled with the app — the user picks any figure they like, so no
    /// third-party license rides in our binary.
    #[serde(default)]
    pub scale_reference_path: Option<String>,
    /// How tall the reference stands, in the model's own mm space
    /// (None = 28 — a classic tabletop human).
    #[serde(default)]
    pub scale_reference_height_mm: Option<f64>,
    /// The creator's licence file (PDF/txt/md), offered as a toggle in the
    /// release builder — set once, ride along in every release.3pk.
    #[serde(default)]
    pub licence_path: Option<String>,
    /// The user's magnet inventory (docs/BASECUTTER.md "Hollow, with magnet
    /// mounts"): what they actually own, e.g. 5x1, 6x2, 10x2. Base Cutter's
    /// per-placement magnet panel offers one chip per entry and suggests
    /// the largest whose boss fits a given base — never a hardcoded
    /// base->magnet table. Seeded with common hobby sizes on first load
    /// (see settings::default_magnet_inventory), same pattern as
    /// known_designers. serde(default): an older store has no such key.
    #[serde(default)]
    pub magnet_inventory: Option<Vec<MagnetSpec>>,
    /// The user's scatter asset library folder (docs/SCATTER.md "User
    /// library"): a flat folder of `*.stl` pieces `scan_scatter_library`
    /// reads non-recursively. None = no user library configured yet, the
    /// piece picker only offers generated + bundled sources.
    /// serde(default): an older store has no such key.
    #[serde(default)]
    pub scatter_library_dir: Option<String>,
    /// Triangle-count ceiling above which geometry mining skips the
    /// open-edge HashMap (see catalog::stl_facts::EDGE_STATS_MAX_TRIS and
    /// catalog::geometry::recommended_edge_cap). Seeded from the machine's
    /// RAM on first load; never below EDGE_STATS_MAX_TRIS — a stored value
    /// under that floor is clamped up on read. serde(default): an older
    /// store has no such key.
    #[serde(default)]
    pub edge_stats_max_tris: Option<u32>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct BlenderInfo {
    pub path: String,
    pub version: String,
}

/// How a detected Blender measures against the render gate
/// (provision::MIN_VERSION / RECOMMENDED_VERSION). Only Missing and TooOld
/// disable rendering; Outdated is a suggestion the user may dismiss.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub enum BlenderVerdict {
    Missing,
    TooOld,
    Outdated,
    Ok,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct BlenderCheck {
    pub verdict: BlenderVerdict,
    /// None only when Missing
    pub info: Option<BlenderInfo>,
    /// The pinned version the download pipeline would install — dialog copy
    /// and the first-run acknowledgement key
    pub managed_version: String,
    /// Whether the detected binary is one we downloaded ourselves
    pub is_managed: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct RenderOptions {
    /// Euler XYZ rotation in degrees, matching render_mini.py --rotate
    pub rotate: (f64, f64, f64),
    /// Linear RGB resin base color, matching --color (None = locked look default)
    pub color: Option<(f64, f64, f64)>,
    pub azimuth: Option<f64>,
    pub elevation: Option<f64>,
    pub zoom: Option<f64>,
    pub resolution: Option<u32>,
    pub samples: Option<u32>,
    /// Tonal look: "flat", "resin", "rich", or "marmoset" (Toolbag-style contrast)
    pub look: Option<String>,
    /// Output PNG path (None = next to the first STL part)
    pub output_path: Option<String>,
    /// Allow replacing an existing file; when false an existing output gets
    /// a unique -N suffix instead of being clobbered
    #[serde(default)]
    pub overwrite: bool,
    /// Re-seat parts exported around different origins by stacking them on
    /// the part named *base* (render_mini.py --align-parts)
    #[serde(default)]
    pub align_parts: bool,
    /// JSON overrides for the script's LOOK recipe, passed verbatim as
    /// --config. Knob paths and defaults mirror src/utils/renderLookSchema.ts
    #[serde(default)]
    pub look_config: Option<String>,
    /// Geometry-driven translucent resin: thickness-dependent SSS plus a
    /// warm rear-light boost. Intended for thin wings, cloth and foliage.
    #[serde(default)]
    pub translucent: bool,
    /// Render the configured scale-reference figure beside the model
    /// (settings supply the STL path + height; silently off when unset).
    #[serde(default)]
    pub scale_reference: bool,
    /// Stand the model on a standard tapered wargaming base — the hobby's
    /// own "banana for scale" (docs/BASECUTTER.md "Synergy: standard bases
    /// in the Render tool"). The NOMINAL (bottom-face) footprint the user
    /// picked from `basecutter::cutters::get_cutter_library`; None = no
    /// base. Rust derives the cut (top-face) footprint via `top_face_of`
    /// before this ever reaches the script — see
    /// `render::engine::build_render_command`.
    #[serde(default)]
    pub base: Option<CutterKind>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub enum CompressionType {
    SevenZip,
    Zip,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct Release {
    pub name: String,
    pub designer: String,
    pub description: String,
    pub date: String,
    pub version: String,
    pub model_references: Vec<ModelReference>,
    pub groups: Vec<String>,
    pub release_dir: String,
    pub images: Vec<String>,
    pub other_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub enum ModelLocation {
    Local(String),
    External(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ModelReference {
    #[specta(type = String)]
    pub id: Uuid,
    pub location: ModelLocation,
}

/// A WIP release sitting in the scratch dir, not yet packed — surfaced so
/// the builder can resume it without depending on the localStorage draft
/// snapshot surviving. Successful finalize deletes the scratch folder, so
/// anything found here is by definition unfinished.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ReleaseDraftSummary {
    pub release_dir: String,
    pub name: String,
    pub designer: String,
    pub model_count: u32,
}

/// A model as the release builder stages it and `model.json` records it.
/// The rich fields mirror the scanner's ModelJson reader — this is the WRITE
/// side of metadata portability (docs/3PK.md): whatever curation the catalog
/// holds rides into the sidecar, the manifest, and back out on another
/// user's scan. All optional with defaults so old sidecars still parse.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct StlModel {
    #[specta(type = Option<String>)]
    pub id: Option<Uuid>,
    pub name: String,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub images: Vec<String>, // the path of the temporary location of the image during archive creation
    pub model_files: Vec<String>, // the path of the temporary location of the model file during archive creation
    pub group: Option<String>,
    #[serde(default)]
    pub variant: Option<String>,
    #[serde(default)]
    pub pose: Option<String>,
    #[serde(default)]
    pub scale: Option<String>,
    #[serde(default)]
    pub support_status: Option<String>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub designer: Option<String>,
    #[serde(default)]
    pub sculptor: Option<String>,
    #[serde(default)]
    pub release_name: Option<String>,
    /// Base sizes in mm as canonical dimension strings: "25" for a
    /// regular base, "60x35" for an oval/rectangle (never a unit suffix).
    /// Both optional — plenty of models ship without a base at all.
    /// Additive to model.json/3pk.
    #[serde(default)]
    pub base_round_mm: Option<String>,
    #[serde(default)]
    pub base_square_mm: Option<String>,
    /// Per-file pose/variant assignments (a curated dump folder), restored
    /// into file_variants on scan. Names are file basenames.
    #[serde(default)]
    pub file_poses: Vec<FilePose>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct FilePose {
    pub name: String,
    #[serde(default)]
    pub variant: Option<String>,
    #[serde(default)]
    pub pose: Option<String>,
    #[serde(default)]
    pub support_status: Option<String>,
}
