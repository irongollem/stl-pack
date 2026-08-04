//! Scatter job pipeline: embed scatter_landscape.py, build the job JSON,
//! spawn headless Blender, parse its stdout token protocol into
//! `ScatterStatus` events. See docs/SCATTER.md "Pinned interfaces" for
//! the job JSON shape and stdout protocol this file implements.
//!
//! One script invocation, one job — not an N-item batch with a mid-run
//! validation-abort gate, so this stays a single file rather than
//! job.rs/commands.rs's split.

use crate::error::AppError;
use crate::models::BlenderInfo;
use crate::models::events::{
    ScatterCancelledStatus, ScatterFailedStatus, ScatterFinishedStatus, ScatterProgressStatus,
    ScatterStartedStatus, ScatterStatus,
};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::AppHandle;
use tauri_specta::Event;
use tokio::sync::Notify;
use uuid::Uuid;

/// The Blender script ships INSIDE the binary (see
/// engine::materialize_embedded_script for the always-overwrite rationale).
const SCATTER_SCRIPT: &str = include_str!("../../resources/scatter_landscape.py");

pub fn materialize_scatter_script(app_handle: &AppHandle) -> Result<PathBuf, AppError> {
    crate::render::engine::materialize_embedded_script(app_handle, "scatter_landscape.py", SCATTER_SCRIPT)
}

/// A generated piece kind — the only source scatter can actually place
/// today (bundled/user assets are a later phase). Serializes lowercase
/// ("pebble"/"rock"/"twig"/"grass"/"mushroom") to match
/// scatter_landscape.py's generated-kind set exactly. `Mushroom` is the
/// one kind that deliberately stands upright (stem down) rather than
/// lying flat like every other non-round generated kind.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Type)]
#[serde(rename_all = "lowercase")]
pub enum GeneratedPieceKind {
    Pebble,
    Rock,
    Twig,
    Grass,
    Mushroom,
}

/// One piece's source — externally tagged with NO `#[serde(tag = ...)]`
/// (Rust's default enum-with-struct-variants shape): `{"Generated":
/// {"kind": "pebble"|"rock"}}` or `{"Asset": {"id": "..."}}`. `Asset` is
/// a recognized, well-formed part of the shape today even though the
/// script fails it gracefully (not implemented yet).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Type)]
pub enum ScatterPieceSource {
    Generated { kind: GeneratedPieceKind },
    Asset { id: String },
}

fn default_weight() -> f64 {
    1.0
}

/// One entry in `ScatterParams.pieces`: a piece source plus its relative
/// pick weight (`scatter_landscape.py::pick_piece_kind` draws by weight from
/// the accepted, non-zero-weight entries). `weight` defaults to 1.0 when
/// omitted, mirroring the script's own `entry.get("weight", 1.0)`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Type)]
pub struct PieceChoice {
    pub piece: ScatterPieceSource,
    #[serde(default = "default_weight")]
    pub weight: f64,
}

/// Where a bundled/user-library scatter asset lives, at scan time
/// (docs/SCATTER.md "Bundled assets" / "Scale anchor"). `footprint_mm` and
/// `height_mm` are measured once at curation/scan, not user input.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Type)]
#[serde(rename_all = "lowercase")]
pub enum ScatterAssetSource {
    Bundled,
    User,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Type)]
pub struct ScatterAsset {
    pub id: String,
    pub label: String,
    pub source: ScatterAssetSource,
    pub path: String,
    pub footprint_mm: f64,
    pub height_mm: f64,
    /// The sRGB hex a placed instance of this asset paints its "Col"
    /// corner attribute with — a curated, muted tabletop tone per bundled
    /// id, or `"#9a9a9a"` for every user-library asset (no curation pass
    /// has looked at it, so neutral grey is the honest default).
    pub color: String,
    /// `None` for every bundled asset (curated and normalized, never
    /// warns) and for a user-library piece under the heuristic;
    /// `Some(message)` is advisory only — the piece is still usable,
    /// never dropped from the returned list on this account alone. See
    /// `scatter_assets::MINI_FOOTPRINT_WARNING_MM` for the exact threshold.
    pub warning: Option<String>,
}

/// Bundled scatter asset set — see `scatter_assets::BUNDLED_ASSETS` for
/// the pinned id/label/footprint/height/license table this reads. Each
/// asset is materialized lazily on every call.
#[tauri::command]
#[specta::specta]
pub fn get_scatter_assets(app_handle: AppHandle) -> Result<Vec<ScatterAsset>, AppError> {
    crate::basecutter::scatter_assets::get_bundled_assets(&app_handle)
}

fn default_scale() -> (f64, f64) {
    (0.85, 1.15)
}
fn default_scale_factor() -> f64 {
    1.0
}
fn default_sink_mm() -> (f64, f64) {
    (0.0, 0.6)
}
fn default_align_to_surface() -> bool {
    true
}
fn default_max_slope_deg() -> f64 {
    55.0
}
fn default_edge_margin_mm() -> f64 {
    2.0
}
fn default_clump() -> f64 {
    0.0
}

/// Scatter placement parameters. Defaults mirror scatter_landscape.py's
/// `scatter()`'s own `params.get(key, default)` fallbacks exactly, so a
/// partial JSON (from a preset or an older UI build) behaves identically
/// whether the default is applied here or in the script. `seed`,
/// `density_per_dm2`, and `pieces` have no script-side default (missing
/// keys raise `KeyError`/`ValueError` there), so they stay required here
/// too.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Type)]
pub struct ScatterParams {
    pub seed: u32,
    pub density_per_dm2: f64,
    #[serde(default = "default_scale")]
    pub scale: (f64, f64),
    #[serde(default = "default_scale_factor")]
    pub scale_factor: f64,
    #[serde(default = "default_sink_mm")]
    pub sink_mm: (f64, f64),
    #[serde(default = "default_align_to_surface")]
    pub align_to_surface: bool,
    #[serde(default = "default_max_slope_deg")]
    pub max_slope_deg: f64,
    #[serde(default = "default_edge_margin_mm")]
    pub edge_margin_mm: f64,
    /// Clustering bias for candidate placement, `0.0..=1.0`. `0.0` (the
    /// default) is the original even jittered-grid behavior EXACTLY — no
    /// warp step runs at all, so a job that omits this key places
    /// identically to before it existed. Toward `1.0`, candidates are
    /// pulled toward a handful of seeded cluster centers instead of
    /// staying evenly spread, so pieces read as tufts/patches rather than
    /// a uniform scatter.
    #[serde(default = "default_clump")]
    pub clump: f64,
    pub pieces: Vec<PieceChoice>,
}

/// A scatter job, as sent from the frontend and forwarded to
/// scatter_landscape.py verbatim — unlike `BaseCutJob`, no field is
/// renamed: the script reads `job["landscape_path"]`, `job["out_path"]`,
/// `job["layers"]` directly.
///
/// `layers` is a STACK, not a single pass: each entry is a full
/// `ScatterParams`, and each places independently onto the TERRAIN from
/// its own seed — adding or removing a layer never moves another layer's
/// pieces. Must be non-empty; `start_scatter`/`validate_scatter_job`
/// reject an empty stack before any Blender work. One layer is the
/// common case.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Type)]
pub struct ScatterJob {
    pub landscape_path: String,
    pub out_path: String,
    pub layers: Vec<ScatterParams>,
}

/// One parsed line of scatter_landscape.py's stdout protocol (see its
/// docstring). Pure/process-free, so the grammar is unit-testable without
/// spawning Blender. `SCATTER_PIECE` (the `--debug`-only per-piece line)
/// is deliberately not modeled here — normal runs never pass `--debug`,
/// so the line never appears on this path.
#[derive(Debug, Clone, PartialEq)]
pub enum ScatterToken {
    Started,
    Progress { placed: u32, total: u32 },
    Done {
        out: String,
        placed: u32,
        manifold: bool,
        /// Re-measured loose-shell count on the round-tripped export
        /// (terrain + one per placed piece, by construction).
        /// `#[serde(default)]`-equivalent here via `Option`: a payload
        /// that omits the field (an older script build) still parses, it
        /// just carries no shell count.
        shells: Option<u32>,
        /// The number of layers in the stack that just ran. Same
        /// `Option`-for-forward-compat treatment as `shells`.
        layers: Option<u32>,
        /// The scattered output's GLB twin path — `Option` for the same
        /// forward/backward-compat reason as `shells`/`layers`: the
        /// current script always sends it, but the parser shouldn't
        /// hard-fail on a stale/hand-edited payload that omits it.
        glb: Option<String>,
    },
    Failed { reason: String },
}

pub fn parse_scatter_token(line: &str) -> Option<ScatterToken> {
    #[derive(Deserialize)]
    struct ProgressPayload {
        placed: u32,
        total: u32,
    }
    #[derive(Deserialize)]
    struct DonePayload {
        out: String,
        placed: u32,
        manifold: bool,
        #[serde(default)]
        shells: Option<u32>,
        #[serde(default)]
        layers: Option<u32>,
        #[serde(default)]
        glb: Option<String>,
    }
    #[derive(Deserialize)]
    struct FailedPayload {
        reason: String,
    }

    let line = line.trim();
    if line == "SCATTER_START" {
        return Some(ScatterToken::Started);
    }
    if let Some(json) = line.strip_prefix("SCATTER_PROGRESS ") {
        let p: ProgressPayload = serde_json::from_str(json).ok()?;
        return Some(ScatterToken::Progress {
            placed: p.placed,
            total: p.total,
        });
    }
    if let Some(json) = line.strip_prefix("SCATTER_DONE ") {
        let p: DonePayload = serde_json::from_str(json).ok()?;
        return Some(ScatterToken::Done {
            out: p.out,
            placed: p.placed,
            manifold: p.manifold,
            shells: p.shells,
            layers: p.layers,
            glb: p.glb,
        });
    }
    if let Some(json) = line.strip_prefix("SCATTER_FAILED ") {
        let p: FailedPayload = serde_json::from_str(json).ok()?;
        return Some(ScatterToken::Failed { reason: p.reason });
    }
    None
}

/// Every unique asset id referenced by `pieces`' `Asset { id }` entries, in
/// first-seen order (a `HashSet` would work too, but a stable order keeps
/// the injected `asset_paths` JSON and any log output deterministic).
fn asset_ids(pieces: &[PieceChoice]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut ids = Vec::new();
    for choice in pieces {
        if let ScatterPieceSource::Asset { id } = &choice.piece {
            if seen.insert(id.clone()) {
                ids.push(id.clone());
            }
        }
    }
    ids
}

/// Every unique asset id referenced across ALL layers' `pieces`, in
/// first-seen order (layer order, then per-layer piece order). Reuses
/// `asset_ids`' own within-layer dedup, then dedups again across layers
/// so an id shared by two layers is only resolved once.
fn layer_asset_ids(layers: &[ScatterParams]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut ids = Vec::new();
    for layer in layers {
        for id in asset_ids(&layer.pieces) {
            if seen.insert(id.clone()) {
                ids.push(id);
            }
        }
    }
    ids
}

/// Resolve every `Asset` piece referenced by ANY of `job.layers`' `pieces`
/// to an absolute path (bundled set first, then the user library — see
/// `scatter_assets::resolve_asset_path`), returning the `id -> path` map
/// `write_job_file` injects into the wire JSON. Fails BEFORE any Blender
/// work if an id can't be resolved — scatter_landscape.py re-checks
/// `asset_paths` itself as defense in depth against a hand-edited or
/// stale job file.
pub fn resolve_asset_paths(
    app_handle: &AppHandle,
    layers: &[ScatterParams],
) -> Result<std::collections::HashMap<String, String>, AppError> {
    layer_asset_ids(layers)
        .into_iter()
        .map(|id| {
            let path = crate::basecutter::scatter_assets::resolve_asset_path(app_handle, &id)?;
            Ok((id, path.to_string_lossy().into_owned()))
        })
        .collect()
}

/// The `asset_colors` counterpart to `resolve_asset_paths` — same id set
/// (every `Asset` piece referenced by ANY layer), but pure: unlike a
/// path, a color never needs an `AppHandle` or touches disk, so this
/// can't fail the way `resolve_asset_paths` can on an unresolvable id.
pub fn resolve_asset_colors(layers: &[ScatterParams]) -> std::collections::HashMap<String, String> {
    layer_asset_ids(layers)
        .into_iter()
        .map(|id| {
            let color = crate::basecutter::scatter_assets::resolve_asset_color(&id);
            (id, color)
        })
        .collect()
}

/// Serialize `job` and inject the resolved `asset_paths`/`asset_colors`
/// maps under their own top-level keys — same "Rust stays the single
/// owner of derived data" shape as `job::write_job_file`'s "cut"
/// footprint injection, but at the top level (one map for the whole job)
/// rather than per-placement, since asset identity isn't a
/// per-placement-derived value, just a lookup shared across every
/// placement that references the same id.
fn job_json_with_asset_extras(
    job: &ScatterJob,
    asset_paths: &std::collections::HashMap<String, String>,
    asset_colors: &std::collections::HashMap<String, String>,
) -> Result<serde_json::Value, AppError> {
    let mut value = serde_json::to_value(job)
        .map_err(|e| AppError::JsonError(format!("Failed to encode scatter job: {}", e)))?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "asset_paths".to_string(),
            serde_json::to_value(asset_paths)
                .map_err(|e| AppError::JsonError(format!("Failed to encode asset_paths: {}", e)))?,
        );
        obj.insert(
            "asset_colors".to_string(),
            serde_json::to_value(asset_colors)
                .map_err(|e| AppError::JsonError(format!("Failed to encode asset_colors: {}", e)))?,
        );
    }
    Ok(value)
}

/// Write the job JSON into `dir` (the materialized script's directory in
/// production; a scratch dir in tests) so Blender can read it via `--job`.
/// `asset_paths` is the already-resolved `id -> path` map — pass an empty
/// map for a job with no `Asset` pieces. `asset_colors` is
/// `resolve_asset_colors`' equally-derived `id -> hex` map; an
/// empty/missing map behaves identically to every piece using the
/// neutral default.
pub fn write_job_file(
    dir: &Path,
    job: &ScatterJob,
    job_id: &str,
    asset_paths: &std::collections::HashMap<String, String>,
    asset_colors: &std::collections::HashMap<String, String>,
) -> Result<PathBuf, AppError> {
    let path = dir.join(format!("scatter_job_{job_id}.json"));
    let value = job_json_with_asset_extras(job, asset_paths, asset_colors)?;
    let json = serde_json::to_string_pretty(&value)
        .map_err(|e| AppError::JsonError(format!("Failed to encode scatter job: {}", e)))?;
    std::fs::write(&path, json)
        .map_err(|e| AppError::IoError(format!("Failed to write scatter job file: {}", e)))?;
    Ok(path)
}

/// Assemble the headless scatter invocation: `--background
/// --factory-startup --python-exit-code 1 --python <script> -- --job <json>`
/// — identical shape to `job::build_base_cut_command`, one job file rather
/// than per-run flags.
pub fn build_scatter_command(
    blender: &BlenderInfo,
    script: &Path,
    job_path: &Path,
) -> tokio::process::Command {
    let mut cmd = crate::render::engine::new_command(Path::new(&blender.path));
    cmd.arg("--background")
        .arg("--factory-startup")
        .arg("--python-exit-code")
        .arg("1")
        .arg("--python")
        .arg(script)
        .arg("--")
        .arg("--job")
        .arg(job_path);
    cmd
}

/// Spawn Blender against `job_path` and parse its stdout into
/// `ScatterToken`s, invoking `on_token` for each as it arrives. Returns the
/// `SCATTER_DONE` payload's fields on success, or `(error, stdout_tail)`.
///
/// One script invocation makes exactly one decorated STL, so every token
/// is handled the same way regardless of content — there's nothing to
/// abort mid-run. Failure is entirely a non-zero exit (the script's own
/// `sys.exit(1)` after `SCATTER_FAILED`, or an uncaught exception via
/// `--python-exit-code 1`). The `Failed` token's `reason` is captured so
/// the error message can quote it instead of just "Blender exited with
/// exit status 1".
pub async fn spawn_and_parse<F>(
    blender: &BlenderInfo,
    script: &Path,
    job_path: &Path,
    cancel_token: &Notify,
    mut on_token: F,
) -> Result<(String, u32, bool, Option<String>), (AppError, String)>
where
    F: FnMut(&ScatterToken),
{
    let cmd = build_scatter_command(blender, script, job_path);
    let mut done: Option<(String, u32, bool, Option<String>)> = None;
    let mut failure_reason: Option<String> = None;

    let merge_tail = |out: String, err: String| {
        if err.is_empty() {
            out
        } else {
            format!("{}\n{}", out, err)
        }
    };

    let run_result = crate::render::engine::run_blender_lines(cmd, Some(cancel_token), |line| {
        if let Some(token) = parse_scatter_token(line) {
            match &token {
                ScatterToken::Done { out, placed, manifold, glb, .. } => {
                    done = Some((out.clone(), *placed, *manifold, glb.clone()))
                }
                ScatterToken::Failed { reason } => failure_reason = Some(reason.clone()),
                ScatterToken::Started | ScatterToken::Progress { .. } => {}
            }
            on_token(&token);
        }
        ControlFlow::Continue(())
    })
    .await;

    use crate::render::engine::BlenderRunError::*;
    let run = match run_result {
        Ok(run) => run,
        Err(SpawnFailed(e)) => {
            return Err((
                AppError::IoError(format!("Failed to launch Blender: {}", e)),
                String::new(),
            ))
        }
        Err(StdoutCaptureFailed) => {
            return Err((
                AppError::IoError("Failed to capture Blender stdout".to_string()),
                String::new(),
            ))
        }
        Err(ReadFailed { source, stdout_tail, stderr_tail }) => {
            return Err((
                AppError::IoError(format!("Failed reading Blender output: {}", source)),
                merge_tail(stdout_tail, stderr_tail),
            ))
        }
        Err(WaitFailed { source, stdout_tail, stderr_tail }) => {
            return Err((
                AppError::IoError(format!("Failed waiting for Blender: {}", source)),
                merge_tail(stdout_tail, stderr_tail),
            ))
        }
        Err(Cancelled { stdout_tail, stderr_tail }) => {
            return Err((
                AppError::UserCancelled("Scatter job cancelled".to_string()),
                merge_tail(stdout_tail, stderr_tail),
            ))
        }
        Err(AbortedByCaller { stdout_tail, stderr_tail }) => {
            return Err((
                AppError::FileProcessingError("Scatter job aborted".to_string()),
                merge_tail(stdout_tail, stderr_tail),
            ))
        }
    };

    if !run.status.success() {
        let message = failure_reason
            .map(|reason| format!("Scatter failed: {}", reason))
            .unwrap_or_else(|| format!("Blender exited with {}", run.status));
        return Err((
            AppError::FileProcessingError(message),
            merge_tail(run.stdout_tail, run.stderr_tail),
        ));
    }

    done.ok_or_else(|| {
        (
            AppError::FileProcessingError(
                "Blender exited cleanly but never reported SCATTER_DONE".to_string(),
            ),
            merge_tail(run.stdout_tail, run.stderr_tail),
        )
    })
}

/// Input guards for `start_scatter`, split out as a plain function (no
/// `AppHandle`/Blender detection) so it's unit-testable without spawning a
/// job. Deliberately checks the same conditions scatter_landscape.py itself
/// would reject (bad density, bad scale range, no usable pieces) so the
/// error surfaces before a Blender launch instead of as an opaque
/// `SCATTER_FAILED`/non-zero-exit round trip. Runs the same per-layer sanity
/// check (`validate_layer`) against EVERY entry in `job.layers` — one bad
/// layer anywhere in the stack fails the whole job before Blender launches.
pub fn validate_scatter_job(job: &ScatterJob) -> Result<(), AppError> {
    if !Path::new(&job.landscape_path).is_file() {
        return Err(AppError::NotFoundError(format!(
            "Landscape not found: {}",
            job.landscape_path
        )));
    }
    if let Some(parent) = Path::new(&job.out_path).parent() {
        if !parent.as_os_str().is_empty() && !parent.is_dir() {
            return Err(AppError::InvalidInput(format!(
                "Output directory does not exist: {}",
                parent.display()
            )));
        }
    }
    if job.layers.is_empty() {
        return Err(AppError::InvalidInput(
            "layers is empty — nothing to scatter".to_string(),
        ));
    }
    for (index, layer) in job.layers.iter().enumerate() {
        validate_layer(index, layer)?;
    }
    Ok(())
}

/// The per-layer half of `validate_scatter_job` — same three checks the
/// single-layer `ScatterParams` shape always ran, now applied to one entry
/// of the stack at a time. `index` is folded into the error message so a
/// bad layer 2-of-3 doesn't read as "the job" failed with no clue which
/// layer.
fn validate_layer(index: usize, params: &ScatterParams) -> Result<(), AppError> {
    if params.density_per_dm2 <= 0.0 {
        return Err(AppError::InvalidInput(format!(
            "layer {index}: density_per_dm2 must be > 0"
        )));
    }
    let (scale_lo, scale_hi) = params.scale;
    if scale_lo <= 0.0 || scale_hi <= 0.0 || scale_lo > scale_hi {
        return Err(AppError::InvalidInput(format!(
            "layer {index}: scale range must be positive and ordered (min <= max), got ({}, {})",
            scale_lo, scale_hi
        )));
    }
    // Rejected, not clamped — every other range in this function (density,
    // scale) is a hard reject on out-of-range input rather than a silent
    // clamp, so a typo'd clump value surfaces immediately instead of
    // quietly running at a different value than what was asked for.
    if !(0.0..=1.0).contains(&params.clump) {
        return Err(AppError::InvalidInput(format!(
            "layer {index}: clump must be in 0.0..=1.0, got {}",
            params.clump
        )));
    }
    if params.pieces.is_empty() {
        return Err(AppError::InvalidInput(format!(
            "layer {index}: pieces is empty — nothing to scatter"
        )));
    }
    if !params.pieces.iter().any(|p| p.weight > 0.0) {
        return Err(AppError::InvalidInput(format!(
            "layer {index}: every piece has weight <= 0 — nothing to scatter"
        )));
    }
    Ok(())
}

/// (job id, its cancel token) — the shape held by the active-job registry,
/// named so clippy's complex-type lint doesn't fire on the static below.
type ActiveJob = (String, Arc<Notify>);

/// The single running scatter job, if any — only one at a time, its own
/// guard (scatter is a distinct activity that merely shares the one
/// Blender process slot).
static ACTIVE_SCATTER: Lazy<Mutex<Option<ActiveJob>>> = Lazy::new(|| Mutex::new(None));

/// Atomically claim the single scatter slot under ONE lock — a separate
/// is-active check before claiming would let two concurrent calls both
/// pass it and spawn two Blender jobs. Extracted as its own function
/// (taking the registry rather than reaching for the static directly) so
/// the race-free claim/release behavior is unit-testable without a Tauri
/// `AppHandle`.
fn claim_active_job(
    registry: &Mutex<Option<ActiveJob>>,
    job_id: &str,
    cancel_token: &Arc<Notify>,
) -> Result<(), AppError> {
    let mut active = registry
        .lock()
        .map_err(|e| AppError::ConfigError(format!("Failed to access scatter registry: {}", e)))?;
    if active.is_some() {
        return Err(AppError::InvalidInput(
            "A scatter job is already running".to_string(),
        ));
    }
    *active = Some((job_id.to_string(), Arc::clone(cancel_token)));
    Ok(())
}

/// Release the slot, but only if it's still holding THIS job — a job that
/// already lost its slot (e.g. to a bug elsewhere) must not clobber a
/// different job's claim.
fn release_active_job(registry: &Mutex<Option<ActiveJob>>, job_id: &str) {
    if let Ok(mut active) = registry.lock() {
        if active.as_ref().is_some_and(|(id, _)| id == job_id) {
            *active = None;
        }
    }
}

#[tauri::command]
#[specta::specta]
pub async fn start_scatter(app_handle: AppHandle, job: ScatterJob) -> Result<String, AppError> {
    validate_scatter_job(&job)?;

    let blender = crate::render::engine::detect_blender_cached().await?;
    let script = materialize_scatter_script(&app_handle)?;

    let job_id = Uuid::new_v4().to_string();
    let cancel_token = Arc::new(Notify::new());
    claim_active_job(&ACTIVE_SCATTER, &job_id, &cancel_token)?;

    ScatterStatus::Started(ScatterStartedStatus {
        job_id: job_id.clone(),
    })
    .emit(&app_handle)
    .ok();

    let job_id_clone = job_id.clone();
    tokio::spawn(async move {
        run_scatter_job(app_handle, job_id_clone, blender, script, job, cancel_token).await;
    });

    Ok(job_id)
}

#[tauri::command]
#[specta::specta]
pub async fn cancel_scatter(job_id: String) -> Result<(), AppError> {
    let active = ACTIVE_SCATTER
        .lock()
        .map_err(|e| AppError::ConfigError(format!("Failed to access scatter registry: {}", e)))?;
    match active.as_ref() {
        Some((active_id, token)) if *active_id == job_id => {
            // notify_one(), not notify_waiters() — same reasoning as
            // basecutter::commands::cancel_base_cut: a cancel landing before
            // the spawned task reaches spawn_and_parse's select loop must
            // not be dropped on the floor. notify_one() stores a permit for
            // exactly this case.
            token.notify_one();
            Ok(())
        }
        _ => Err(AppError::NotFoundError(format!(
            "No active scatter job with ID: {}",
            job_id
        ))),
    }
}

/// Translate one ScatterToken into its ScatterStatus event and emit it.
/// `Started` is already emitted synchronously by `start_scatter` before the
/// child even spawns (so the frontend sees it immediately); `Done`/`Failed`
/// flow into the terminal Finished/Failed event `run_scatter_job` emits
/// after `spawn_and_parse` returns (it also needs to know whether the run
/// errored) — nothing to do here for either.
fn handle_token(app_handle: &AppHandle, job_id: &str, token: &ScatterToken) {
    if let ScatterToken::Progress { placed, total } = token {
        ScatterStatus::Progress(ScatterProgressStatus {
            job_id: job_id.to_string(),
            placed: *placed,
            total: *total,
        })
        .emit(app_handle)
        .ok();
    }
}

async fn run_scatter_job(
    app_handle: AppHandle,
    job_id: String,
    blender: BlenderInfo,
    script: PathBuf,
    job: ScatterJob,
    cancel_token: Arc<Notify>,
) {
    let script_dir = script
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    // Resolved once, up front — a failure here (unknown id, missing user-
    // library file) is reported the same way any other pre-flight failure
    // is, before write_job_file or Blender ever runs.
    let asset_paths = match resolve_asset_paths(&app_handle, &job.layers) {
        Ok(paths) => paths,
        Err(e) => {
            release_active_job(&ACTIVE_SCATTER, &job_id);
            ScatterStatus::Failed(ScatterFailedStatus {
                job_id,
                message: e.to_string(),
                stdout_tail: String::new(),
            })
            .emit(&app_handle)
            .ok();
            return;
        }
    };

    // Pure and infallible (see resolve_asset_colors' doc comment) — no
    // pre-flight failure branch needed here the way asset_paths above has.
    let asset_colors = resolve_asset_colors(&job.layers);

    let result = match write_job_file(&script_dir, &job, &job_id, &asset_paths, &asset_colors) {
        Ok(job_path) => {
            let app_handle_ref = &app_handle;
            let job_id_ref = &job_id;
            let outcome = spawn_and_parse(&blender, &script, &job_path, &cancel_token, |token| {
                handle_token(app_handle_ref, job_id_ref, token)
            })
            .await;
            std::fs::remove_file(&job_path).ok();
            outcome
        }
        Err(e) => Err((e, String::new())),
    };

    release_active_job(&ACTIVE_SCATTER, &job_id);

    match result {
        Ok((out_path, placed, manifold, glb_path)) => {
            ScatterStatus::Finished(ScatterFinishedStatus {
                job_id,
                out_path,
                placed,
                manifold,
                glb_path,
            })
            .emit(&app_handle)
            .ok();
        }
        Err((AppError::UserCancelled(_), _stdout_tail)) => {
            ScatterStatus::Cancelled(ScatterCancelledStatus { job_id })
                .emit(&app_handle)
                .ok();
        }
        Err((e, stdout_tail)) => {
            ScatterStatus::Failed(ScatterFailedStatus {
                job_id,
                message: e.to_string(),
                stdout_tail,
            })
            .emit(&app_handle)
            .ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pebble(weight: f64) -> PieceChoice {
        PieceChoice {
            piece: ScatterPieceSource::Generated {
                kind: GeneratedPieceKind::Pebble,
            },
            weight,
        }
    }

    fn rock(weight: f64) -> PieceChoice {
        PieceChoice {
            piece: ScatterPieceSource::Generated {
                kind: GeneratedPieceKind::Rock,
            },
            weight,
        }
    }

    fn valid_params() -> ScatterParams {
        ScatterParams {
            seed: 7,
            density_per_dm2: 25.0,
            scale: (0.85, 1.15),
            scale_factor: 1.0,
            sink_mm: (0.0, 0.6),
            align_to_surface: true,
            max_slope_deg: 55.0,
            edge_margin_mm: 3.0,
            clump: 0.0,
            pieces: vec![
                pebble(0.6),
                PieceChoice {
                    piece: ScatterPieceSource::Generated {
                        kind: GeneratedPieceKind::Rock,
                    },
                    weight: 0.4,
                },
            ],
        }
    }

    /// Pins the full job JSON against every key scatter_landscape.py's
    /// module docstring documents — the ground-truth shape check the task
    /// calls for.
    #[test]
    fn job_serializes_to_the_pinned_script_shape() {
        let job = ScatterJob {
            landscape_path: "/path/to/landscape.stl".to_string(),
            out_path: "/path/to/landscape-scattered.stl".to_string(),
            layers: vec![valid_params()],
        };
        let json = serde_json::to_value(&job).unwrap();

        assert_eq!(json["landscape_path"], "/path/to/landscape.stl");
        assert_eq!(json["out_path"], "/path/to/landscape-scattered.stl");
        assert!(json.get("landscape").is_none(), "must not use base_cut.py's renamed key");
        // The old single-params shape is gone outright, not kept alongside
        // layers (house rule: old === redundant, no compat branch).
        assert!(json.get("params").is_none(), "the single-params shape must not survive");
        assert!(json["layers"].is_array());
        assert_eq!(json["layers"].as_array().unwrap().len(), 1);

        for key in [
            "seed",
            "density_per_dm2",
            "scale",
            "scale_factor",
            "sink_mm",
            "align_to_surface",
            "max_slope_deg",
            "edge_margin_mm",
            "clump",
            "pieces",
        ] {
            assert!(
                json["layers"][0].get(key).is_some(),
                "layers[0].{key} missing from wire JSON — scatter_landscape.py's docstring pins this key"
            );
        }
        assert_eq!(json["layers"][0]["seed"], 7);
        assert_eq!(json["layers"][0]["density_per_dm2"], 25.0);
        assert_eq!(json["layers"][0]["scale"][0], 0.85);
        assert_eq!(json["layers"][0]["scale"][1], 1.15);
        assert_eq!(json["layers"][0]["sink_mm"][0], 0.0);
        assert_eq!(json["layers"][0]["sink_mm"][1], 0.6);
        assert_eq!(json["layers"][0]["align_to_surface"], true);
        assert_eq!(json["layers"][0]["max_slope_deg"], 55.0);
        assert_eq!(json["layers"][0]["edge_margin_mm"], 3.0);

        assert_eq!(json["layers"][0]["pieces"][0]["piece"]["Generated"]["kind"], "pebble");
        assert_eq!(json["layers"][0]["pieces"][0]["weight"], 0.6);
        assert_eq!(json["layers"][0]["pieces"][1]["piece"]["Generated"]["kind"], "rock");
        assert_eq!(json["layers"][0]["pieces"][1]["weight"], 0.4);

        let back: ScatterJob = serde_json::from_value(json).unwrap();
        assert_eq!(back.landscape_path, "/path/to/landscape.stl");
        assert_eq!(back.layers.len(), 1);
    }

    /// The stack shape the whole task is about: multiple layers, in order,
    /// each carrying its own full ScatterParams — not just a one-layer
    /// convenience wrapper.
    #[test]
    fn job_serializes_multiple_layers_in_order() {
        let mut layer_two = valid_params();
        layer_two.seed = 99;
        layer_two.density_per_dm2 = 5.0;
        let job = ScatterJob {
            landscape_path: "/l.stl".to_string(),
            out_path: "/out.stl".to_string(),
            layers: vec![valid_params(), layer_two],
        };
        let json = serde_json::to_value(&job).unwrap();
        assert_eq!(json["layers"].as_array().unwrap().len(), 2);
        assert_eq!(json["layers"][0]["seed"], 7);
        assert_eq!(json["layers"][1]["seed"], 99);
        assert_eq!(json["layers"][1]["density_per_dm2"], 5.0);
    }

    /// The `Asset` variant is a recognized, well-formed part of the pinned
    /// shape even though scatter_landscape.py fails it gracefully (S4 not
    /// implemented) — the wire shape itself must still round-trip cleanly.
    #[test]
    fn asset_piece_source_serializes_to_the_pinned_shape() {
        let choice = PieceChoice {
            piece: ScatterPieceSource::Asset {
                id: "skull-01".to_string(),
            },
            weight: 1.0,
        };
        let json = serde_json::to_value(&choice).unwrap();
        assert_eq!(json["piece"]["Asset"]["id"], "skull-01");
        assert!(json["piece"].get("Generated").is_none());

        let back: PieceChoice = serde_json::from_value(json).unwrap();
        assert_eq!(back, choice);
    }

    /// The non-round Generated kinds (twig/grass/mushroom) must
    /// round-trip through the SAME externally-tagged, lowercase shape as
    /// pebble/rock — scatter_landscape.py's generated-kind dispatch matches
    /// on these exact lowercase strings (see its GENERATED_KINDS set).
    #[test]
    fn twig_grass_mushroom_piece_kinds_serialize_to_the_pinned_lowercase_shape() {
        for (kind, expected) in [
            (GeneratedPieceKind::Twig, "twig"),
            (GeneratedPieceKind::Grass, "grass"),
            (GeneratedPieceKind::Mushroom, "mushroom"),
        ] {
            let choice = PieceChoice {
                piece: ScatterPieceSource::Generated { kind: kind.clone() },
                weight: 1.0,
            };
            let json = serde_json::to_value(&choice).unwrap();
            assert_eq!(json["piece"]["Generated"]["kind"], expected);

            let back: PieceChoice = serde_json::from_value(json).unwrap();
            assert_eq!(back, choice);
        }
    }

    /// Every default here must match scatter_landscape.py's own
    /// `params.get(key, default)` fallbacks exactly — a JSON that omits an
    /// optional key must behave identically whether Rust or the script
    /// applies the default.
    #[test]
    fn params_defaults_match_the_scripts_own_fallbacks_when_omitted() {
        let json = serde_json::json!({
            "seed": 1,
            "density_per_dm2": 10.0,
            "pieces": [{"piece": {"Generated": {"kind": "pebble"}}}]
        });
        let params: ScatterParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.scale, (0.85, 1.15));
        assert_eq!(params.scale_factor, 1.0);
        assert_eq!(params.sink_mm, (0.0, 0.6));
        assert!(params.align_to_surface);
        assert_eq!(params.max_slope_deg, 55.0);
        assert_eq!(params.edge_margin_mm, 2.0);
        // layer_json.get("clump", 0.0) — omitting clump must reproduce the
        // pre-clump even jittered-grid behavior exactly (see
        // scatter_landscape.py's build_candidates: clump <= 0.0 skips the
        // warp step entirely).
        assert_eq!(params.clump, 0.0);
        // entry.get("weight", 1.0)
        assert_eq!(params.pieces[0].weight, 1.0);
    }

    #[test]
    fn parse_scatter_token_handles_every_token_type() {
        assert_eq!(parse_scatter_token("SCATTER_START"), Some(ScatterToken::Started));
        assert_eq!(
            parse_scatter_token(r#"SCATTER_PROGRESS {"placed": 3, "total": 10}"#),
            Some(ScatterToken::Progress { placed: 3, total: 10 })
        );
        assert_eq!(
            // No "shells"/"layers"/"glb" key — an older-script-shaped payload
            // must still parse.
            parse_scatter_token(
                r#"SCATTER_DONE {"out": "/l-scattered.stl", "placed": 10, "manifold": true}"#
            ),
            Some(ScatterToken::Done {
                out: "/l-scattered.stl".to_string(),
                placed: 10,
                manifold: true,
                shells: None,
                layers: None,
                glb: None,
            })
        );
        assert_eq!(
            parse_scatter_token(
                r#"SCATTER_DONE {"out": "/l.stl", "glb": "/l-scattered.glb", "placed": 5,
                   "manifold": true, "shells": 6, "non_manifold_edges": 0,
                   "total_edges": 900, "layers": 2}"#
            ),
            Some(ScatterToken::Done {
                out: "/l.stl".to_string(),
                placed: 5,
                manifold: true,
                shells: Some(6),
                layers: Some(2),
                glb: Some("/l-scattered.glb".to_string()),
            })
        );
        assert_eq!(
            parse_scatter_token(r#"SCATTER_FAILED {"reason": "assets not supported yet (S4)"}"#),
            Some(ScatterToken::Failed {
                reason: "assets not supported yet (S4)".to_string(),
            })
        );
    }

    #[test]
    fn parse_scatter_token_rejects_garbage_and_blender_noise() {
        assert_eq!(parse_scatter_token(""), None);
        assert_eq!(parse_scatter_token("   "), None);
        assert_eq!(parse_scatter_token("SCATTER_START extra-garbage"), None);
        assert_eq!(parse_scatter_token("SCATTER_PROGRESS not-json"), None);
        assert_eq!(parse_scatter_token("random log line from Blender"), None);
        assert_eq!(
            parse_scatter_token("Blender 5.1.2 (hash abcdef1234 built 2025-01-01)"),
            None
        );
        // Missing a required field is not a token.
        assert_eq!(parse_scatter_token(r#"SCATTER_DONE {"out": "/l.stl"}"#), None);
        // The --debug-only per-piece line is not modeled — must not parse.
        assert_eq!(
            parse_scatter_token(r#"SCATTER_PIECE {"index": 0, "kind": "pebble"}"#),
            None
        );
    }

    #[test]
    fn scatter_command_has_expected_shape() {
        let blender = BlenderInfo {
            path: "/usr/bin/blender".to_string(),
            version: "Blender 5.1.2".to_string(),
        };
        let cmd = build_scatter_command(
            &blender,
            Path::new("/tmp/scatter_landscape.py"),
            Path::new("/tmp/job.json"),
        );
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            vec![
                "--background",
                "--factory-startup",
                "--python-exit-code",
                "1",
                "--python",
                "/tmp/scatter_landscape.py",
                "--",
                "--job",
                "/tmp/job.json",
            ]
        );
    }

    #[test]
    fn write_job_file_writes_readable_json() {
        let dir = std::env::temp_dir().join(format!("stlpack_scatter_unit_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let job = ScatterJob {
            landscape_path: "/l.stl".to_string(),
            out_path: "/out/l-scattered.stl".to_string(),
            layers: vec![valid_params()],
        };
        let path = write_job_file(
            &dir,
            &job,
            "abc123",
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
        )
        .unwrap();
        assert!(path.is_file());
        let contents = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(value["landscape_path"], "/l.stl");
        assert_eq!(value["out_path"], "/out/l-scattered.stl");
        assert_eq!(value["asset_paths"], serde_json::json!({}));
        assert_eq!(value["asset_colors"], serde_json::json!({}));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Pins the `asset_paths` wire shape the task calls for: a top-level
    /// `{"id": "path", ...}` map injected alongside `landscape_path` /
    /// `out_path` / `params`, exactly what scatter_landscape.py's
    /// `job.get("asset_paths", {})` reads. Mirrors
    /// `job::wire_json_carries_the_derived_cut_footprint`'s pinning style.
    #[test]
    fn write_job_file_injects_asset_paths_at_the_top_level() {
        let dir = std::env::temp_dir().join(format!("stlpack_scatter_assetpaths_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut params = valid_params();
        params.pieces.push(PieceChoice {
            piece: ScatterPieceSource::Asset {
                id: "skull-hesperocyon".to_string(),
            },
            weight: 0.3,
        });
        let job = ScatterJob {
            landscape_path: "/l.stl".to_string(),
            out_path: "/out/l-scattered.stl".to_string(),
            layers: vec![params],
        };
        let mut asset_paths = std::collections::HashMap::new();
        asset_paths.insert(
            "skull-hesperocyon".to_string(),
            "/materialized/scatter/skull-hesperocyon.stl".to_string(),
        );

        let path = write_job_file(
            &dir,
            &job,
            "assetpaths123",
            &asset_paths,
            &std::collections::HashMap::new(),
        )
        .unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(
            value["asset_paths"]["skull-hesperocyon"],
            "/materialized/scatter/skull-hesperocyon.stl"
        );
        // The frontend-facing ScatterJob type itself must not gain an
        // "asset_paths" field — same non-pollution rule job.rs's own test
        // pins for "cut".
        let plain = serde_json::to_value(&job).unwrap();
        assert!(plain.get("asset_paths").is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The `asset_colors` counterpart to
    /// `write_job_file_injects_asset_paths_at_the_top_level` — VTT GLB
    /// export design doc "Scatter": Rust builds this map from the asset
    /// registry (`resolve_asset_colors`) and injects it at job-write time,
    /// the script never guesses a color from an id.
    #[test]
    fn write_job_file_injects_asset_colors_at_the_top_level() {
        let dir = std::env::temp_dir().join(format!("stlpack_scatter_assetcolors_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut params = valid_params();
        params.pieces.push(PieceChoice {
            piece: ScatterPieceSource::Asset {
                id: "skull-hesperocyon".to_string(),
            },
            weight: 0.3,
        });
        let job = ScatterJob {
            landscape_path: "/l.stl".to_string(),
            out_path: "/out/l-scattered.stl".to_string(),
            layers: vec![params],
        };
        let asset_colors = resolve_asset_colors(&job.layers);
        assert_eq!(asset_colors.get("skull-hesperocyon"), Some(&"#cfc6b0".to_string()));

        let path = write_job_file(&dir, &job, "assetcolors123", &std::collections::HashMap::new(), &asset_colors)
            .unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(value["asset_colors"]["skull-hesperocyon"], "#cfc6b0");
        // Same non-pollution rule as asset_paths: the frontend-facing
        // ScatterJob type never gains an "asset_colors" field of its own.
        let plain = serde_json::to_value(&job).unwrap();
        assert!(plain.get("asset_colors").is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// An id with no curated color (a user-library asset, or any id not in
    /// `scatter_assets::BUNDLED_ASSETS`) resolves to the neutral default —
    /// `resolve_asset_colors` never fails the way `resolve_asset_paths` can.
    #[test]
    fn resolve_asset_colors_defaults_unknown_ids_to_neutral_grey() {
        let mut params = valid_params();
        params.pieces.push(PieceChoice {
            piece: ScatterPieceSource::Asset {
                id: "some-user-library-rock".to_string(),
            },
            weight: 0.2,
        });
        let colors = resolve_asset_colors(&[params]);
        assert_eq!(colors.get("some-user-library-rock"), Some(&"#9a9a9a".to_string()));
    }

    #[test]
    fn asset_ids_collects_unique_ids_in_first_seen_order() {
        let pieces = vec![
            pebble(1.0),
            PieceChoice {
                piece: ScatterPieceSource::Asset { id: "b".to_string() },
                weight: 1.0,
            },
            PieceChoice {
                piece: ScatterPieceSource::Asset { id: "a".to_string() },
                weight: 1.0,
            },
            PieceChoice {
                piece: ScatterPieceSource::Asset { id: "b".to_string() },
                weight: 1.0,
            },
        ];
        assert_eq!(asset_ids(&pieces), vec!["b".to_string(), "a".to_string()]);
    }

    /// The asset_paths union the ScatterJob doc comment pins: an id
    /// referenced by more than one layer appears once, in first-seen order
    /// across the WHOLE stack (layer order, then within-layer piece order).
    #[test]
    fn layer_asset_ids_unions_across_layers() {
        let mut layer_one = valid_params();
        layer_one.pieces = vec![
            pebble(1.0),
            PieceChoice { piece: ScatterPieceSource::Asset { id: "skull-a".to_string() }, weight: 1.0 },
        ];
        let mut layer_two = valid_params();
        layer_two.pieces = vec![
            PieceChoice { piece: ScatterPieceSource::Asset { id: "skull-b".to_string() }, weight: 1.0 },
            // Same id as layer one — must not be resolved/listed twice.
            PieceChoice { piece: ScatterPieceSource::Asset { id: "skull-a".to_string() }, weight: 1.0 },
        ];
        assert_eq!(
            layer_asset_ids(&[layer_one, layer_two]),
            vec!["skull-a".to_string(), "skull-b".to_string()]
        );
    }

    #[test]
    fn validate_scatter_job_rejects_a_missing_landscape() {
        let job = ScatterJob {
            landscape_path: "/definitely/not/a/real/path.stl".to_string(),
            out_path: std::env::temp_dir().to_string_lossy().into_owned() + "/out.stl",
            layers: vec![valid_params()],
        };
        assert!(matches!(
            validate_scatter_job(&job),
            Err(AppError::NotFoundError(_))
        ));
    }

    #[test]
    fn validate_scatter_job_rejects_a_nonexistent_output_directory() {
        let dir = std::env::temp_dir().join(format!("stlpack_scatter_validate_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let landscape = dir.join("landscape.stl");
        std::fs::write(&landscape, b"not a real stl, just needs to exist").unwrap();

        let job = ScatterJob {
            landscape_path: landscape.to_string_lossy().into_owned(),
            out_path: dir
                .join("nonexistent-subdir")
                .join("out.stl")
                .to_string_lossy()
                .into_owned(),
            layers: vec![valid_params()],
        };
        assert!(matches!(
            validate_scatter_job(&job),
            Err(AppError::InvalidInput(_))
        ));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_scatter_job_rejects_bad_density_and_scale_and_pieces() {
        let dir = std::env::temp_dir().join(format!("stlpack_scatter_validate2_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let landscape = dir.join("landscape.stl");
        std::fs::write(&landscape, b"not a real stl, just needs to exist").unwrap();
        let landscape_path = landscape.to_string_lossy().into_owned();
        let out_path = dir.join("out.stl").to_string_lossy().into_owned();

        let mut params = valid_params();
        params.density_per_dm2 = 0.0;
        let job = ScatterJob { landscape_path: landscape_path.clone(), out_path: out_path.clone(), layers: vec![params] };
        assert!(matches!(validate_scatter_job(&job), Err(AppError::InvalidInput(_))));

        let mut params = valid_params();
        params.scale = (1.2, 0.8); // min > max
        let job = ScatterJob { landscape_path: landscape_path.clone(), out_path: out_path.clone(), layers: vec![params] };
        assert!(matches!(validate_scatter_job(&job), Err(AppError::InvalidInput(_))));

        let mut params = valid_params();
        params.scale = (0.0, 1.15); // not > 0
        let job = ScatterJob { landscape_path: landscape_path.clone(), out_path: out_path.clone(), layers: vec![params] };
        assert!(matches!(validate_scatter_job(&job), Err(AppError::InvalidInput(_))));

        let mut params = valid_params();
        params.pieces = vec![];
        let job = ScatterJob { landscape_path: landscape_path.clone(), out_path: out_path.clone(), layers: vec![params] };
        assert!(matches!(validate_scatter_job(&job), Err(AppError::InvalidInput(_))));

        let mut params = valid_params();
        params.pieces = vec![pebble(0.0)];
        let job = ScatterJob { landscape_path: landscape_path.clone(), out_path: out_path.clone(), layers: vec![params] };
        assert!(matches!(validate_scatter_job(&job), Err(AppError::InvalidInput(_))));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `clump` is a 0.0..=1.0 knob (docs/SCATTER.md-pinned range in this
    /// task) — out-of-range values are REJECTED, not silently clamped, same
    /// house style as density/scale above; the boundary values 0.0 and 1.0
    /// are both valid (inclusive range).
    #[test]
    fn validate_scatter_job_rejects_out_of_range_clump_and_accepts_the_boundaries() {
        let dir = std::env::temp_dir().join(format!("stlpack_scatter_validate_clump_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let landscape = dir.join("landscape.stl");
        std::fs::write(&landscape, b"not a real stl, just needs to exist").unwrap();
        let landscape_path = landscape.to_string_lossy().into_owned();
        let out_path = dir.join("out.stl").to_string_lossy().into_owned();

        let mut params = valid_params();
        params.clump = -0.01;
        let job = ScatterJob { landscape_path: landscape_path.clone(), out_path: out_path.clone(), layers: vec![params] };
        assert!(matches!(validate_scatter_job(&job), Err(AppError::InvalidInput(_))));

        let mut params = valid_params();
        params.clump = 1.01;
        let job = ScatterJob { landscape_path: landscape_path.clone(), out_path: out_path.clone(), layers: vec![params] };
        assert!(matches!(validate_scatter_job(&job), Err(AppError::InvalidInput(_))));

        let mut params = valid_params();
        params.clump = 0.0;
        let job = ScatterJob { landscape_path: landscape_path.clone(), out_path: out_path.clone(), layers: vec![params] };
        assert!(validate_scatter_job(&job).is_ok(), "clump=0.0 (the boundary/default) must be accepted");

        let mut params = valid_params();
        params.clump = 1.0;
        let job = ScatterJob { landscape_path, out_path, layers: vec![params] };
        assert!(validate_scatter_job(&job).is_ok(), "clump=1.0 (the other boundary) must be accepted");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A JSON payload that omits `clump` (an older UI build, or a preset
    /// authored before this knob existed) must deserialize to 0.0 — the
    /// "no clumping, identical to before this feature" default.
    #[test]
    fn clump_defaults_to_zero_when_omitted() {
        let json = serde_json::json!({
            "seed": 1,
            "density_per_dm2": 10.0,
            "pieces": [{"piece": {"Generated": {"kind": "pebble"}}}]
        });
        let params: ScatterParams = serde_json::from_value(json).unwrap();
        assert_eq!(params.clump, 0.0);
    }

    #[test]
    fn validate_scatter_job_rejects_an_empty_layer_stack() {
        let dir = std::env::temp_dir().join(format!("stlpack_scatter_validate_nolayers_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let landscape = dir.join("landscape.stl");
        std::fs::write(&landscape, b"not a real stl, just needs to exist").unwrap();

        let job = ScatterJob {
            landscape_path: landscape.to_string_lossy().into_owned(),
            out_path: dir.join("out.stl").to_string_lossy().into_owned(),
            layers: vec![],
        };
        assert!(matches!(
            validate_scatter_job(&job),
            Err(AppError::InvalidInput(_))
        ));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A bad layer anywhere in the stack — not just the first — must fail
    /// the whole job before Blender launches.
    #[test]
    fn validate_scatter_job_rejects_a_bad_second_layer() {
        let dir = std::env::temp_dir().join(format!("stlpack_scatter_validate_badlayer2_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let landscape = dir.join("landscape.stl");
        std::fs::write(&landscape, b"not a real stl, just needs to exist").unwrap();

        let mut bad_layer = valid_params();
        bad_layer.pieces = vec![];
        let job = ScatterJob {
            landscape_path: landscape.to_string_lossy().into_owned(),
            out_path: dir.join("out.stl").to_string_lossy().into_owned(),
            layers: vec![valid_params(), bad_layer],
        };
        let err = validate_scatter_job(&job).expect_err("second layer's empty pieces must be rejected");
        assert!(matches!(err, AppError::InvalidInput(_)));
        assert!(err.to_string().contains("layer 1"), "error should name which layer failed: {err}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_scatter_job_accepts_a_well_formed_job() {
        let dir = std::env::temp_dir().join(format!("stlpack_scatter_validate3_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let landscape = dir.join("landscape.stl");
        std::fs::write(&landscape, b"not a real stl, just needs to exist").unwrap();

        let job = ScatterJob {
            landscape_path: landscape.to_string_lossy().into_owned(),
            out_path: dir.join("out.stl").to_string_lossy().into_owned(),
            layers: vec![valid_params()],
        };
        assert!(validate_scatter_job(&job).is_ok());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_scatter_job_accepts_a_well_formed_multi_layer_job() {
        let dir = std::env::temp_dir().join(format!("stlpack_scatter_validate_multi_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let landscape = dir.join("landscape.stl");
        std::fs::write(&landscape, b"not a real stl, just needs to exist").unwrap();

        let mut layer_two = valid_params();
        layer_two.seed = 42;
        let job = ScatterJob {
            landscape_path: landscape.to_string_lossy().into_owned(),
            out_path: dir.join("out.stl").to_string_lossy().into_owned(),
            layers: vec![valid_params(), layer_two],
        };
        assert!(validate_scatter_job(&job).is_ok());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The check-and-claim race the base-cut review flagged: a claim must
    /// happen under the SAME lock acquisition as the is-active check, or two
    /// concurrent callers could both observe "empty" and both claim.
    /// Exercised directly against a fresh, non-global registry so it's
    /// deterministic and doesn't touch the shared ACTIVE_SCATTER static.
    #[test]
    fn claim_active_job_rejects_a_second_claim_while_one_is_active() {
        let registry: Mutex<Option<ActiveJob>> = Mutex::new(None);
        let token_a = Arc::new(Notify::new());
        claim_active_job(&registry, "job-a", &token_a).expect("first claim should succeed");

        let token_b = Arc::new(Notify::new());
        let err = claim_active_job(&registry, "job-b", &token_b)
            .expect_err("second claim while job-a is active must be rejected");
        assert!(matches!(err, AppError::InvalidInput(_)));

        release_active_job(&registry, "job-a");
        claim_active_job(&registry, "job-b", &token_b).expect("claim after release should succeed");
    }

    #[test]
    fn release_active_job_only_clears_its_own_claim() {
        let registry: Mutex<Option<ActiveJob>> = Mutex::new(None);
        let token = Arc::new(Notify::new());
        claim_active_job(&registry, "job-a", &token).unwrap();

        // Releasing a different job id must not clobber job-a's claim.
        release_active_job(&registry, "job-b");
        assert!(registry.lock().unwrap().is_some());

        release_active_job(&registry, "job-a");
        assert!(registry.lock().unwrap().is_none());
    }

    // get_scatter_assets needs a real `AppHandle` to materialize bytes, so
    // it isn't unit-tested directly; BUNDLED_ASSETS itself is covered by
    // scatter_assets.rs's manifest-drift tests, and materialization is
    // exercised by the ignored end-to-end test below.

    /// Run scatter_landscape.py directly (bypassing spawn_and_parse, which
    /// deliberately never models `--debug`'s SCATTER_PIECE line — see
    /// ScatterToken::Done's own doc comment) and capture the FULL raw
    /// stdout, so the independence test below can inspect per-piece debug
    /// positions no production code path ever needs.
    async fn run_scatter_debug(blender: &BlenderInfo, script: &Path, job_path: &Path) -> String {
        let mut cmd = build_scatter_command(blender, script, job_path);
        cmd.arg("--debug");
        let output = cmd.output().await.expect("failed to launch blender for scatter");
        assert!(
            output.status.success(),
            "scatter (debug) failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Every `SCATTER_PIECE {...}` payload in a debug run's stdout, in
    /// emission order.
    fn scatter_piece_lines(stdout: &str) -> Vec<serde_json::Value> {
        stdout
            .lines()
            .filter_map(|line| line.trim().strip_prefix("SCATTER_PIECE "))
            .map(|json| serde_json::from_str(json).expect("SCATTER_PIECE payload must be valid JSON"))
            .collect()
    }

    /// End-to-end: generate a small watertight landscape with Blender
    /// itself, then exercise the LAYER STACK contract:
    ///
    ///   1. a 2-layer job (layer 0: generated pebbles+rocks; layer 1: a
    ///      bundled skull Asset + generated rocks) through spawn_and_parse
    ///      (the production path) — Finished-shaped output, manifold,
    ///      shells == 1 + total placed, `layers` == 2 on SCATTER_DONE;
    ///   2. determinism — the identical 2-layer job run twice exports
    ///      byte-identical STLs;
    ///   3. independence — layer 0's own SCATTER_PIECE positions (x/y/z/yaw/
    ///      size/kind) are IDENTICAL whether it runs alone or as the first
    ///      layer of the 2-layer stack, proving a later layer never perturbs
    ///      an earlier one's placement.
    ///
    /// Run with: cargo test -- --ignored scatters_layers_independently_and_deterministically
    #[tokio::test]
    #[ignore = "requires a local Blender install and ~30s"]
    async fn scatters_layers_independently_and_deterministically_with_real_blender() {
        let blender = crate::render::engine::detect_blender()
            .await
            .expect("Blender not found — install it or set BLENDER_BIN");
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/scatter_landscape.py");

        let dir = std::env::temp_dir().join(format!("stlpack_scatter_layers_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let landscape = dir.join("landscape.stl");
        generate_test_landscape(&blender, &dir, &landscape).await;
        assert!(landscape.is_file(), "landscape generation failed");

        // The same bytes get_scatter_assets/resolve_asset_path would
        // materialize for real — written directly here since this test has
        // no Tauri AppHandle to materialize through.
        let asset_id = "skull-hesperocyon";
        let asset_bytes = crate::basecutter::scatter_assets::bundled_asset_bytes_for_test(asset_id)
            .expect("skull-hesperocyon is in BUNDLED_ASSETS");
        let asset_path = dir.join(format!("{asset_id}.stl"));
        std::fs::write(&asset_path, asset_bytes).unwrap();
        let mut asset_paths = std::collections::HashMap::new();
        asset_paths.insert(asset_id.to_string(), asset_path.to_string_lossy().into_owned());

        // Layer 0: pebbles + rocks, generous density so independence has
        // several pieces to compare, not just one.
        let layer_pebbles_and_rocks = ScatterParams {
            seed: 7,
            density_per_dm2: 8.0,
            scale: (0.85, 1.15),
            scale_factor: 1.0,
            sink_mm: (0.0, 0.6),
            align_to_surface: true,
            max_slope_deg: 55.0,
            edge_margin_mm: 3.0,
            clump: 0.0,
            pieces: vec![pebble(0.6), rock(0.4)],
        };
        // Layer 1: the bundled skull + more rocks, a DIFFERENT seed and
        // density — layers are independent passes, not clones of each other.
        let layer_skull_and_rocks = ScatterParams {
            seed: 42,
            density_per_dm2: 4.0,
            scale: (0.85, 1.15),
            scale_factor: 1.0,
            sink_mm: (0.0, 0.6),
            align_to_surface: true,
            max_slope_deg: 55.0,
            edge_margin_mm: 3.0,
            clump: 0.0,
            pieces: vec![
                PieceChoice {
                    piece: ScatterPieceSource::Asset {
                        id: asset_id.to_string(),
                    },
                    weight: 0.5,
                },
                rock(0.5),
            ],
        };

        // The `asset_colors` counterpart to `asset_paths` above — same
        // pure resolution `run_scatter_job` does, computed once and reused
        // for every job below (harmless if a given job doesn't reference
        // every id in it — same as asset_paths already does for `dir`).
        let asset_colors =
            resolve_asset_colors(&[layer_pebbles_and_rocks.clone(), layer_skull_and_rocks.clone()]);

        // ---- 1. production path: 2-layer job through spawn_and_parse ----
        let stacked_out = dir.join("stacked.stl");
        let stacked_job = ScatterJob {
            landscape_path: landscape.to_string_lossy().into_owned(),
            out_path: stacked_out.to_string_lossy().into_owned(),
            layers: vec![layer_pebbles_and_rocks.clone(), layer_skull_and_rocks.clone()],
        };
        let stacked_job_path = write_job_file(&dir, &stacked_job, "stacked-job", &asset_paths, &asset_colors)
            .expect("write job file");

        let cancel_token = Notify::new();
        let mut tokens: Vec<ScatterToken> = Vec::new();
        let result = spawn_and_parse(&blender, &script, &stacked_job_path, &cancel_token, |token| {
            tokens.push(token.clone());
        })
        .await;

        let (out, placed, manifold, glb) = match result {
            Ok(v) => v,
            Err((e, tail)) => panic!("stacked scatter job failed: {e}\nstdout tail:\n{tail}"),
        };

        assert!(Path::new(&out).is_file(), "expected a scattered STL at {:?}", out);
        assert!(manifold, "scattered landscape is not manifold");
        assert!(
            placed > 0,
            "expected at least one piece placed across both layers"
        );
        // VTT GLB export design doc "Scatter": the scattered output always
        // gets a .glb twin, and it must actually exist with the right
        // magic bytes — the same "report what downstream will actually
        // see" discipline roundtrip_check already applies to manifoldness.
        let glb_path = glb.expect("SCATTER_DONE must report a glb path");
        assert!(Path::new(&glb_path).is_file(), "expected a GLB twin at {:?}", glb_path);
        let glb_bytes = std::fs::read(&glb_path).unwrap();
        assert!(
            glb_bytes.len() >= 4 && &glb_bytes[0..4] == b"glTF",
            "expected the GLB twin to start with the glTF magic, got: {:?}",
            &glb_bytes[..glb_bytes.len().min(16)]
        );
        assert!(tokens.iter().any(|t| matches!(t, ScatterToken::Started)));
        assert!(tokens.iter().any(|t| matches!(t, ScatterToken::Progress { .. })));
        assert!(
            matches!(tokens.last(), Some(ScatterToken::Done { .. })),
            "expected the token sequence to end with SCATTER_DONE, got: {:?}",
            tokens
        );

        // Loose shells (docs/SCATTER.md): terrain + one shell per placed
        // piece ACROSS THE WHOLE STACK. Read straight off SCATTER_DONE's
        // additive `shells`/`layers` fields (re-measured on the
        // round-tripped export by scatter_landscape.py's own
        // roundtrip_check) rather than duplicating that measurement here.
        let (shells, layers_reported) = tokens
            .iter()
            .find_map(|t| match t {
                ScatterToken::Done { shells, layers, .. } => Some((*shells, *layers)),
                _ => None,
            })
            .expect("SCATTER_DONE token must be present");
        let shells = shells.expect("SCATTER_DONE must report a shells count");
        assert!(
            shells > 1,
            "expected terrain + at least one piece shell, got {shells} shell(s)"
        );
        assert_eq!(
            shells,
            1 + placed,
            "shells must equal terrain (1) + placed pieces by construction, across the whole stack"
        );
        assert_eq!(
            layers_reported,
            Some(2),
            "SCATTER_DONE must report the 2-layer stack that actually ran"
        );

        // ---- 2. determinism: the SAME 2-layer job run twice ----
        let stacked_out_2 = dir.join("stacked-2.stl");
        let stacked_job_2 = ScatterJob {
            landscape_path: landscape.to_string_lossy().into_owned(),
            out_path: stacked_out_2.to_string_lossy().into_owned(),
            layers: vec![layer_pebbles_and_rocks.clone(), layer_skull_and_rocks.clone()],
        };
        let stacked_job_path_2 =
            write_job_file(&dir, &stacked_job_2, "stacked-job-2", &asset_paths, &asset_colors)
                .expect("write job file 2");
        let result_2 = spawn_and_parse(&blender, &script, &stacked_job_path_2, &Notify::new(), |_| {}).await;
        let (out_2, placed_2, _manifold_2, _glb_2) = match result_2 {
            Ok(v) => v,
            Err((e, tail)) => panic!("second stacked scatter run failed: {e}\nstdout tail:\n{tail}"),
        };
        assert_eq!(placed, placed_2, "same layers must place the same piece count");
        let bytes_1 = std::fs::read(&out).unwrap();
        let bytes_2 = std::fs::read(&out_2).unwrap();
        assert_eq!(
            bytes_1, bytes_2,
            "the same layer stack must export byte-identical STLs across runs"
        );

        // ---- 3. independence: layer 0 alone vs. layer 0 as part of the stack ----
        let single_out = dir.join("single.stl");
        let single_job = ScatterJob {
            landscape_path: landscape.to_string_lossy().into_owned(),
            out_path: single_out.to_string_lossy().into_owned(),
            layers: vec![layer_pebbles_and_rocks.clone()],
        };
        let single_job_path =
            write_job_file(&dir, &single_job, "single-job", &asset_paths, &asset_colors)
                .expect("write single job file");
        let single_stdout = run_scatter_debug(&blender, &script, &single_job_path).await;
        let single_pieces = scatter_piece_lines(&single_stdout);

        let stacked_debug_out = dir.join("stacked-debug.stl");
        let stacked_debug_job = ScatterJob {
            landscape_path: landscape.to_string_lossy().into_owned(),
            out_path: stacked_debug_out.to_string_lossy().into_owned(),
            layers: vec![layer_pebbles_and_rocks, layer_skull_and_rocks],
        };
        let stacked_debug_job_path = write_job_file(
            &dir,
            &stacked_debug_job,
            "stacked-debug-job",
            &asset_paths,
            &asset_colors,
        )
        .expect("write stacked debug job file");
        let stacked_stdout = run_scatter_debug(&blender, &script, &stacked_debug_job_path).await;
        let stacked_pieces = scatter_piece_lines(&stacked_stdout);

        let layer0_from_single: Vec<&serde_json::Value> = single_pieces
            .iter()
            .filter(|p| p["layer"] == 0)
            .collect();
        let layer0_from_stacked: Vec<&serde_json::Value> = stacked_pieces
            .iter()
            .filter(|p| p["layer"] == 0)
            .collect();

        assert!(
            !layer0_from_single.is_empty(),
            "expected layer 0 to place at least one piece on a ~40x40mm plate at density 8/dm2"
        );
        assert_eq!(
            layer0_from_single.len(),
            layer0_from_stacked.len(),
            "layer 0's placed-piece COUNT must not change when layer 1 is added"
        );
        for (alone, stacked) in layer0_from_single.iter().zip(layer0_from_stacked.iter()) {
            for field in ["kind", "x_mm", "y_mm", "z_mm", "yaw_deg", "size_mm", "floor_mm", "embed_depth_mm", "aligned_deg"] {
                assert_eq!(
                    alone[field], stacked[field],
                    "layer 0's piece field {field:?} moved when layer 1 was added \
                     (alone: {alone}, stacked: {stacked}) — independence is broken"
                );
            }
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Builds a small closed (watertight) bumpy blob with Blender itself —
    /// a subdivided cube with per-vertex jitter — and exports it as the
    /// landscape STL the end-to-end test scatters onto. Deliberately its own
    /// copy rather than a shared helper: basecutter::job's
    /// `generate_test_landscape` is private to that module's own test mod,
    /// and each embedded-script test suite builds its own throwaway fixture
    /// (same non-shared convention as the scripts themselves).
    async fn generate_test_landscape(blender: &BlenderInfo, dir: &Path, out: &Path) {
        let gen_script = dir.join("gen_landscape.py");
        let py = r#"
import bpy
import os
import random

bpy.ops.wm.read_factory_settings(use_empty=True)
bpy.ops.mesh.primitive_cube_add(size=40)
obj = bpy.context.object
bpy.ops.object.mode_set(mode='EDIT')
bpy.ops.mesh.subdivide(number_cuts=4)
bpy.ops.object.mode_set(mode='OBJECT')

random.seed(7)
for v in obj.data.vertices:
    v.co.z += random.uniform(-2.0, 2.0)
obj.data.update()

bpy.ops.object.select_all(action='DESELECT')
obj.select_set(True)
bpy.context.view_layer.objects.active = obj
out_path = os.environ["STLPACK_TEST_LANDSCAPE_OUT"]
bpy.ops.wm.stl_export(filepath=out_path, export_selected_objects=True)
"#;
        std::fs::write(&gen_script, py).unwrap();

        let mut cmd = crate::render::engine::new_command(Path::new(&blender.path));
        cmd.arg("--background")
            .arg("--factory-startup")
            .arg("--python")
            .arg(&gen_script)
            .env("STLPACK_TEST_LANDSCAPE_OUT", out.to_string_lossy().into_owned());
        let output = cmd
            .output()
            .await
            .expect("failed to launch blender for landscape generation");
        assert!(
            output.status.success(),
            "landscape generation failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
