pub mod commands;
pub mod db;
pub mod dups;
pub mod layout;
pub mod normalize;
pub mod pack;
pub mod scanner;
pub mod sidecar;
pub mod stl_facts;

use serde::{Deserialize, Serialize};
use specta::Type;

/// Extensions treated as printable model files during scans. The second
/// row is machine-ready sliced output (g-code, vendor resin plates) —
/// not everyone keeps it, but it belongs with the model when they do.
pub const MODEL_EXTENSIONS: &[&str] = &[
    "stl", "obj", "3mf", "lys", "chitu", "chitubox", "blend", "gcode",
    "bgcode", "goo", "ctb", "cbddlp", "photon", "pwmx", "pwmo", "pwms", "pw0",
];
/// Extensions usable as a model preview image.
pub const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif"];

// ---- internal scan rows ----

#[derive(Debug, Clone, Default)]
pub struct FileRow {
    pub path: String,
    pub dir_path: String,
    pub file_name: String,
    pub extension: String,
    pub size_bytes: i64,
    pub modified_at: i64,
    /// Set when the file's bytes live inside a model.plinthpack instead of
    /// loose on disk — `path` is then the location the file WOULD occupy,
    /// which keeps every path-keyed table stable across pack/unpack.
    pub archive_path: Option<String>,
    /// `blake3:<hex>` known at scan time (pack sidecars carry one per file),
    /// letting packed files join duplicate detection without disk reads.
    pub content_hash: Option<String>,
}

/// One packed model dir, from its pack.json sidecar.
#[derive(Debug, Clone)]
pub struct PackRow {
    pub model_dir: String,
    pub archive_path: String,
    pub archive_size_bytes: i64,
    pub archive_checksum: Option<String>,
    pub packed_at: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct ModelRow {
    // Default-derived so tests can spell only the fields they exercise
    pub dir_path: String,
    pub name: String,
    pub description: Option<String>,
    pub designer: Option<String>,
    pub release_name: Option<String>,
    pub preview_path: Option<String>,
    pub source: String,
    pub uuid: Option<String>,
    pub file_count: u32,
    pub total_size_bytes: i64,
    pub variant: Option<String>,
    pub pose: Option<String>,
    pub scale: Option<String>,
    pub support_status: Option<String>,
    pub release_date: Option<String>,
    pub sculptor: Option<String>,
    pub base_round_mm: Option<String>,
    pub base_square_mm: Option<String>,
    /// The logical model this row is a variant of; rows sharing it collapse
    /// into one catalog group (see db::search_groups).
    pub group_name: Option<String>,
    /// Chosen render orientation "x,y,z" (Blender euler degrees), from
    /// model.json — the studio-saved value overlays via model_user_meta.
    pub rotation: Option<String>,
    /// True printed bounding box "60.2x35.1x88.7" (mm) + part count,
    /// measured by the render pipeline; round-trips through model.json.
    pub dims_mm: Option<String>,
    pub part_count: Option<String>,
}

/// A per-file pose/variant assignment imported from a model.json's
/// `file_poses` — the scanner resolves each entry to a scanned file path.
#[derive(Debug, Clone)]
pub struct FileVariantRow {
    pub path: String,
    pub variant: Option<String>,
    pub pose: Option<String>,
    pub support_status: Option<String>,
}

// ---- frontend-facing types ----

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct CatalogEntry {
    pub dir_path: String,
    /// Effective display name: the user's custom_name when set, else the
    /// scanner's. custom_name below carries the raw override so the UI can
    /// distinguish "renamed" from "inferred" (and clear it to revert).
    pub name: String,
    pub description: Option<String>,
    pub designer: Option<String>,
    pub release_name: Option<String>,
    pub preview_path: Option<String>,
    pub tags: Vec<String>,
    pub file_count: u32,
    pub total_size_bytes: f64,
    /// The facet between support and pose — a weapon/sculpt option
    /// (sword, mounted, …). Free-text, user-supplied (no scanner inference).
    pub variant: Option<String>,
    pub pose: Option<String>,
    pub scale: Option<String>,
    pub support_status: Option<String>,
    pub release_date: Option<String>,
    pub custom_name: Option<String>,
    /// The studio/brand. Scanned from the release for release models, and
    /// user-overridable per model. sculptor (the individual artist) has no
    /// folder signal, so it comes only from the user or an imported manifest.
    pub sculptor: Option<String>,
    /// Base sizes in mm: "25", or "60x35" for ovals/rectangles.
    pub base_round_mm: Option<String>,
    pub base_square_mm: Option<String>,
    /// Set only on members synthesized from file→pose assignments: a stable
    /// `{dir_path}\u{1f}{pose}` handle ("...\u{1f}" for the residual
    /// unassigned member). None means a whole-folder member — its dir_path
    /// alone identifies it. The UI keys members on `variant_key ?? dir_path`
    /// and passes it back to fetch only that pose's files.
    pub variant_key: Option<String>,
    /// The scanner-level group this member belongs to — the unit a combine
    /// maps and a detach removes. Differs from the card's group_name when
    /// the member is only in the card via a rename/combine.
    pub source_group: String,
    /// True when every model file in this member's folder lives inside a
    /// pack archive (compressed at rest) — byte-needing actions must unpack
    /// or extract first.
    #[serde(default)]
    pub packed: bool,
    /// Chosen render orientation "x,y,z" (Blender euler degrees) — saved
    /// when the studio renders this model; batch re-renders reuse it.
    #[serde(default)]
    pub rotation: Option<String>,
    /// Measured printed size "60.2x35.1x88.7" in mm, and how many part
    /// files make up the model — captured by the render pipeline.
    #[serde(default)]
    pub dims_mm: Option<String>,
    #[serde(default)]
    pub part_count: Option<String>,
    /// This member's effective 18+ flag (explicit override, or the
    /// designer-wide rule when unset) — a display filter, not encryption;
    /// scans still index everything. See db::NSFW_EFFECTIVE_SQL.
    #[serde(default)]
    pub nsfw: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct CatalogSearchResult {
    pub entries: Vec<CatalogEntry>,
    pub total: u32,
}

/// One logical model: every variant dir (supported/unsupported builds,
/// poses) sharing a group_name, aggregated for the card view. Drill in
/// with get_catalog_group_members.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct CatalogGroup {
    pub group_name: String,
    pub designer: Option<String>,
    /// Aggregated like designer: any variant's value stands in for the
    /// group, so grouping by release works even when only the release
    /// models (not later additions) carry the metadata.
    pub release_name: Option<String>,
    pub release_date: Option<String>,
    pub variant_count: u32,
    pub pose_count: u32,
    pub support_statuses: Vec<String>,
    pub file_count: u32,
    pub total_size_bytes: f64,
    pub preview_path: Option<String>,
    /// True when every member of the group is compressed at rest.
    #[serde(default)]
    pub packed: bool,
    /// True when ANY member is effectively 18+ — the card-level badge and
    /// what the browse filter hides when Settings' "Show 18+" is off.
    #[serde(default)]
    pub nsfw: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct CatalogGroupResult {
    pub groups: Vec<CatalogGroup>,
    pub total: u32,
}

/// One configured catalog folder and its indexed footprint — a row in the
/// roots management UI. Zero counts with no last_scan mean "added but never
/// scanned"; a stale last_scan is the cue to offer a rescan.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct CatalogRootSummary {
    pub root: String,
    pub model_count: u32,
    pub file_count: u32,
    pub total_size_bytes: f64,
    pub last_scan_epoch: Option<f64>,
    /// The optional staging folder: Clean up drains the other folders'
    /// models into this one. At most one row carries it.
    pub primary: bool,
}

/// One designer and how many logical models (groups) carry that name —
/// feeds the catalog's designer filter dropdown.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct DesignerCount {
    pub designer: String,
    pub model_count: u32,
}

/// One planned normalizer action. `kind`: "dir" renames a whole folder
/// (hardlink-safe), "file" moves/renames one file, "pose" only records
/// file-level pose metadata (no filesystem side). `pose` rides along on
/// file ops when a pose-dir merge bakes the pose into the file name.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct NormalizeOp {
    pub from: String,
    pub to: String,
    pub kind: String,
    pub pose: Option<String>,
}

/// Everything the normalizer wants to do to ONE model group — shown to the
/// user as a reviewable diff before anything moves.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct NormalizeGroupPlan {
    pub group_name: String,
    pub designer: String,
    pub target_dir: String,
    pub ops: Vec<NormalizeOp>,
    /// Source dirs to sweep (if emptied) after the moves.
    pub old_dirs: Vec<String>,
    /// Human-readable caveats (name clashes, folders left in place).
    pub notes: Vec<String>,
    pub clean: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct NormalizeSkip {
    pub group_name: String,
    pub reason: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct NormalizePlan {
    /// Groups with work to do (already-clean groups are only counted).
    pub groups: Vec<NormalizeGroupPlan>,
    pub skipped: Vec<NormalizeSkip>,
    pub total_ops: u32,
    pub clean_groups: u32,
    /// Names of the already-clean groups — finalize can be re-run on these
    /// to refresh their model.json sidecars without moving anything (the
    /// repair path when sidecar-writing logic improves after a cleanup).
    pub clean_names: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct CatalogFile {
    pub path: String,
    pub file_name: String,
    pub extension: String,
    pub size_bytes: f64,
    /// True when the bytes live inside the model's pack archive — `path` is
    /// then where the file lands when extracted, not a file on disk.
    #[serde(default)]
    pub packed: bool,
}

/// The user-editable metadata for one model, saved together from the drawer.
/// A struct rather than positional args because specta caps command arity.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct ModelMetaUpdate {
    pub custom_name: Option<String>,
    pub variant: Option<String>,
    pub pose: Option<String>,
    pub scale: Option<String>,
    pub support_status: Option<String>,
    pub release_date: Option<String>,
    pub designer: Option<String>,
    pub sculptor: Option<String>,
    pub release_name: Option<String>,
    #[serde(default)]
    pub base_round_mm: Option<String>,
    #[serde(default)]
    pub base_square_mm: Option<String>,
}

/// A user's per-file pose assignment for a "dump everything in one folder"
/// model. Purely metadata (keyed by path, rescan-safe): the file stays put
/// on disk, but the catalog fans the folder out into one member per pose.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct FileVariant {
    pub path: String,
    pub dir_path: String,
    pub variant: Option<String>,
    pub pose: Option<String>,
    pub support_status: Option<String>,
}

/// What ensure_model_files did: the paths it materialized from archives
/// (ephemeral working copies, candidates for cleanup_ephemeral_files) vs
/// how many of the requested paths were already loose on disk.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct EnsureOutcome {
    pub extracted: Vec<String>,
    pub already_loose: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct ExtensionStat {
    pub extension: String,
    pub file_count: u32,
    pub total_size_bytes: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct CatalogStats {
    pub total_models: u32,
    pub total_files: u32,
    pub total_size_bytes: f64,
    pub extensions: Vec<ExtensionStat>,
    pub last_scan_epoch: Option<f64>,
    /// Compressed-at-rest accounting: how many models are packed, what their
    /// files would occupy loose, and what the archives actually take.
    /// total_size_bytes reports logical sizes, so the UI derives real disk
    /// usage as total − packed_logical + packed_archive.
    #[serde(default)]
    pub packed_models: u32,
    #[serde(default)]
    pub packed_logical_bytes: f64,
    #[serde(default)]
    pub packed_archive_bytes: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct DuplicateGroup {
    pub hash: String,
    pub size_bytes: f64,
    pub paths: Vec<String>,
    /// How many physical copies the paths actually occupy on disk. Hardlinked
    /// paths count once, so a fully merged group reports 1 ("shared", nothing
    /// to reclaim) and reclaimable space is size_bytes × (distinct_copies − 1).
    pub distinct_copies: u32,
    /// The subset of `paths` whose bytes live inside a pack archive — they
    /// join detection via their stored checksums but can't be merged or
    /// deleted until the model is unpacked; the UI greys them with a hint.
    #[serde(default)]
    pub packed_paths: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct TagCount {
    pub tag: String,
    pub count: u32,
}

/// One (designer, release_name) origin among the models a group rename would
/// touch. group_renames is keyed purely on the scanner-derived group name —
/// no root/designer scoping — so a generic name ("Spear") reused by an
/// unrelated designer collides with it silently. More than one distinct
/// origin here means the rename reaches further than whatever single card
/// the user is looking at.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct GroupOrigin {
    pub designer: Option<String>,
    pub release_name: Option<String>,
    pub model_count: u32,
}

/// One requested directory move: rename `from` to `to` on disk and repoint
/// the catalog index to match.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct MoveOperation {
    pub from: String,
    pub to: String,
}

/// Outcome of a batch that may partially succeed — the counts and the
/// per-item errors travel together so the UI can report both.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct BatchOutcome {
    pub succeeded: u32,
    pub errors: Vec<String>,
}

/// One distinct release_name found across scanned models — a read-only
/// aggregation over already-indexed data, NOT a persisted publish log (the
/// catalog doesn't track a "finalized" event, so this just reflects what
/// scanning found on disk).
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct ReleaseSummary {
    pub release_name: String,
    pub designer: Option<String>,
    pub model_count: u32,
    pub total_size_bytes: f64,
}
