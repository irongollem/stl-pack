//! Tauri commands for the Base Cutter job pipeline. Thin: validation +
//! spawning here, the actual child-process/stdout-parsing lives in job.rs
//! (kept process-free-testable per docs/BASECUTTER.md phase 3). Mirrors
//! render/commands.rs's start_render/cancel_render shape.

use crate::basecutter::cutters::{top_face_of, CutterKind, Placement, PlinthParams};
use crate::basecutter::job::{self, BaseCutJob, BaseCutToken};
use crate::error::AppError;
use crate::models::events::{
    BaseCutCancelledStatus, BaseCutCutDoneStatus, BaseCutCutFailedStatus,
    BaseCutCutStartedStatus, BaseCutFailedStatus, BaseCutFinishedStatus, BaseCutStartedStatus,
    BaseCutStatus, BaseCutValidatedStatus, BaseCutValidatingStatus, BaseCutValidationReport,
};
use crate::render::engine;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::AppHandle;
use tauri_specta::Event;
use tokio::sync::Notify;
use uuid::Uuid;

/// Below this, the plinth's own taper/height has eaten the entire cut
/// footprint (a degenerate or negative top face) — the cut would be a
/// sliver or nothing at all, so it's rejected before Blender ever runs
/// rather than surfacing as an inscrutable boolean failure mid-job.
const MIN_CUT_DIMENSION_MM: f64 = 1.0;

/// The smallest span of the derived cut footprint — the dimension the
/// taper inset shrinks fastest for non-square shapes (an oval's minor axis,
/// a rect's short side).
fn min_cut_dimension_mm(kind: &CutterKind) -> f64 {
    match kind {
        CutterKind::Circle { diameter_mm } => *diameter_mm,
        CutterKind::Ellipse { major_mm, minor_mm } => major_mm.min(*minor_mm),
        CutterKind::Rect { width_mm, depth_mm } => width_mm.min(*depth_mm),
    }
}

/// How a placement should be named in an error message: its own name if it
/// has one, else a 1-based ordinal (placements are user-facing, so
/// "placement 1" reads better than a 0-based index).
fn placement_label(placement: &Placement, index: usize) -> String {
    match &placement.name {
        Some(name) => format!("'{name}'"),
        None => format!("placement {}", index + 1),
    }
}

/// Windows reserved device names — case-insensitive, and reserved with or
/// without an extension (`NUL.stl` is just as unusable as `NUL`). The
/// userbase is mostly Windows (per docs), so this matters in practice, not
/// just in theory.
const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Whether `segment` is safe to use as a single bare filename component —
/// shared by `validate_placements` (placement.name -> `{name}.stl` in
/// base_cut.py's unique_out_path) and generator::start_landscape_generation
/// (preset_id -> `{slug}-{seed}.stl`). Both values cross the Tauri IPC
/// boundary from an untrusted frontend and land in an `os.path.join`/
/// `PathBuf::join` call, where an absolute path or `..` component escapes
/// the intended output directory rather than staying inside it — this is
/// the actual trust boundary, the frontend's own char-blocklist
/// (src/utils/placementName.ts) is not enough on its own.
///
/// Rejects: empty (after trim), any path separator (`/` or `\` — both, not
/// just the host OS's, since the userbase is mostly Windows regardless of
/// where the app happens to run), `.`/`..` or a `..` component, a
/// `Component::Prefix` (drive letter / UNC prefix), any of the Windows
/// forbidden characters `< > : " | ? *`, control characters, or a Windows
/// reserved device name (CON, NUL, COM1, ...).
pub(crate) fn is_safe_filename_segment(segment: &str) -> bool {
    let trimmed = segment.trim();
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return false;
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return false;
    }
    const FORBIDDEN_CHARS: &[char] = &['<', '>', ':', '"', '|', '?', '*'];
    if trimmed.chars().any(|c| FORBIDDEN_CHARS.contains(&c) || c.is_control()) {
        return false;
    }
    // Belt-and-braces: Path::components() catches anything the manual
    // checks above missed (e.g. a `Component::Prefix` a raw char scan
    // wouldn't recognize as one on a non-Windows build host).
    let component_count = Path::new(trimmed).components().count();
    if component_count != 1 || !matches!(Path::new(trimmed).components().next(), Some(std::path::Component::Normal(_)))
    {
        return false;
    }
    let base = trimmed.split('.').next().unwrap_or(trimmed);
    if WINDOWS_RESERVED_NAMES
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(base))
    {
        return false;
    }
    true
}

/// Rust-side sanity bound on `BaseCutJob.topper_mm` — deliberately looser
/// than base_cut.py's [1.0, 3.0] clamp (docs/BASECUTTER.md "Pinned
/// interfaces": "the script clamps 1..3 anyway — Rust just guards
/// nonsense"). This only rejects values that can't possibly be a sane
/// request (non-finite, zero/negative, or wildly oversized); the script
/// remains the single source of truth for the actual usable range and
/// echoes back a clamp in CUT_DONE when it adjusts the value.
const MAX_SANE_TOPPER_MM: f64 = 10.0;

/// Guard for `BaseCutJob.topper_mm`, split out as a plain function so it's
/// unit-testable without spawning a job (same shape as `validate_placements`
/// below). `None` (normal seat-on-plinth mode) always passes.
fn validate_topper_mm(topper_mm: Option<f64>) -> Result<(), AppError> {
    match topper_mm {
        None => Ok(()),
        Some(t) if !t.is_finite() => Err(AppError::InvalidInput(
            "topper_mm must be a finite number".to_string(),
        )),
        Some(t) if t <= 0.0 => Err(AppError::InvalidInput(format!(
            "topper_mm must be positive, got {t}"
        ))),
        Some(t) if t > MAX_SANE_TOPPER_MM => Err(AppError::InvalidInput(format!(
            "topper_mm {t} is unreasonably large (max {MAX_SANE_TOPPER_MM}mm) — base_cut.py clamps the usable range to 1.0-3.0mm anyway"
        ))),
        Some(_) => Ok(()),
    }
}

/// Input guards for `start_base_cut`, split out as a plain function (no
/// AppHandle/Blender detection) so both guards are unit-testable without
/// spawning a job:
///
/// - a placement whose derived cut footprint (`cutters::top_face_of`) has
///   any dimension at or under `MIN_CUT_DIMENSION_MM` is rejected — the
///   plinth taper/height has eaten the whole footprint, so the cut would
///   be degenerate;
/// - two placements sharing a (non-empty) name are rejected — base_cut.py
///   names each output STL after the placement, so a collision means one
///   cut silently overwrites the other's file;
/// - a placement.name that isn't a safe single filename segment is
///   rejected — it flows unsanitized into base_cut.py's `unique_out_path`
///   as `os.path.join(out_dir, f"{name}.stl")`, and this IPC command is the
///   real trust boundary (the frontend's char-blocklist in
///   src/utils/placementName.ts is not enough on its own): a caller of the
///   Tauri bridge can send any string, and a `..`/absolute/drive-prefixed
///   name would write the STL outside out_dir entirely.
pub fn validate_placements(placements: &[Placement], plinth: &PlinthParams) -> Result<(), AppError> {
    let mut seen_names: HashSet<&str> = HashSet::new();
    for (index, placement) in placements.iter().enumerate() {
        let cut = top_face_of(&placement.cutter, plinth);
        let dim = min_cut_dimension_mm(&cut);
        if dim <= MIN_CUT_DIMENSION_MM {
            return Err(AppError::InvalidInput(format!(
                "{}: the plinth taper/height eats the whole footprint (derived cut dimension {:.2}mm is at or under the {}mm minimum)",
                placement_label(placement, index),
                dim,
                MIN_CUT_DIMENSION_MM
            )));
        }
        if let Some(name) = &placement.name {
            if !is_safe_filename_segment(name) {
                return Err(AppError::InvalidInput(format!(
                    "{}: name '{name}' is not a valid output filename — it must not contain a path separator, '..', a drive/UNC prefix, or the characters < > : \" | ? *",
                    placement_label(placement, index)
                )));
            }
            if !seen_names.insert(name.as_str()) {
                return Err(AppError::InvalidInput(format!(
                    "Duplicate placement name '{name}' — two placements with the same name would overwrite each other's output STL"
                )));
            }
        }
    }
    Ok(())
}

/// A running base-cut job's id and its cancel token.
type ActiveBaseCutJob = (String, Arc<Notify>);

/// The single running base-cut job, if any (id + its cancel token). Unlike
/// render's ACTIVE_RENDERS map, only one base-cut job may run at a time
/// (docs/BASECUTTER.md "Job pipeline") — a plain Option is the simple guard
/// the doc calls for, no map needed.
static ACTIVE_BASE_CUT: Lazy<Mutex<Option<ActiveBaseCutJob>>> = Lazy::new(|| Mutex::new(None));

fn output_is_inside_catalog(output: &str, roots: &[String]) -> bool {
    let normalize = |path: &str| {
        Path::new(path)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(path))
            .to_string_lossy()
            .trim_end_matches(['/', '\\'])
            .replace('\\', "/")
            .to_lowercase()
    };
    let output = normalize(output);
    roots.iter().any(|root| {
        let root = normalize(root);
        output == root || output.starts_with(&format!("{root}/"))
    })
}

#[tauri::command]
#[specta::specta]
pub async fn start_base_cut(app_handle: AppHandle, job: BaseCutJob) -> Result<String, AppError> {
    if job.placements.is_empty() {
        return Err(AppError::InvalidInput(
            "No placements in the base-cut job".to_string(),
        ));
    }
    if !Path::new(&job.landscape_path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("stl"))
    {
        return Err(AppError::InvalidInput(
            "The landscape must be an .stl file".to_string(),
        ));
    }
    if !Path::new(&job.landscape_path).is_file() {
        return Err(AppError::NotFoundError(format!(
            "Landscape not found: {}",
            job.landscape_path
        )));
    }
    validate_placements(&job.placements, &job.plinth)?;
    validate_topper_mm(job.topper_mm)?;
    let settings = crate::settings::get_settings(app_handle.clone())
        .await
        .map_err(AppError::ConfigError)?;
    if output_is_inside_catalog(&job.out_dir, &settings.catalog_roots.unwrap_or_default()) {
        return Err(AppError::InvalidInput(
            "The raw output folder must be outside every catalog root — use Add to catalog for finished cuts"
                .to_string(),
        ));
    }

    let blender = engine::detect_blender_cached().await?;
    let script = job::materialize_base_cut_script(&app_handle)?;

    let job_id = Uuid::new_v4().to_string();
    let cancel_token = Arc::new(Notify::new());
    // Check-and-claim under ONE lock: a separate is-active check would let
    // two concurrent calls both pass it and spawn two Blender jobs.
    {
        let mut active = ACTIVE_BASE_CUT.lock().map_err(|e| {
            AppError::ConfigError(format!("Failed to access base-cut registry: {}", e))
        })?;
        if active.is_some() {
            return Err(AppError::InvalidInput(
                "A base-cut job is already running".to_string(),
            ));
        }
        *active = Some((job_id.clone(), Arc::clone(&cancel_token)));
    }

    let total = job.placements.len() as u32;
    BaseCutStatus::Started(BaseCutStartedStatus {
        job_id: job_id.clone(),
        total,
    })
    .emit(&app_handle)
    .ok();

    let job_id_clone = job_id.clone();
    tokio::spawn(async move {
        run_base_cut_job(app_handle, job_id_clone, blender, script, job, cancel_token).await;
    });

    Ok(job_id)
}

#[tauri::command]
#[specta::specta]
pub async fn cancel_base_cut(job_id: String) -> Result<(), AppError> {
    let active = ACTIVE_BASE_CUT
        .lock()
        .map_err(|e| AppError::ConfigError(format!("Failed to access base-cut registry: {}", e)))?;
    match active.as_ref() {
        Some((active_id, token)) if *active_id == job_id => {
            // notify_one(), not notify_waiters(): notify_waiters() only
            // wakes a future that is ALREADY polling notified() and stores
            // no permit, so a cancel landing in the window between
            // start_base_cut spawning the job task and that task reaching
            // spawn_and_parse's select loop would be dropped on the floor.
            // notify_one() stores a permit for exactly this case — the
            // next notified().await resolves immediately instead of
            // hanging until Blender finishes on its own.
            token.notify_one();
            Ok(())
        }
        _ => Err(AppError::NotFoundError(format!(
            "No active base-cut job with ID: {}",
            job_id
        ))),
    }
}

/// Drive one job through job::spawn_and_parse, translating each
/// BaseCutToken into a BaseCutStatus event as it arrives, then emit the
/// terminal Finished/Cancelled/Failed event. A cancelled run comes back as
/// `Err((AppError::UserCancelled(_), _))` (spawn_and_parse's select loop),
/// which is matched out separately so the frontend sees Cancelled rather
/// than Failed — same distinction render/commands.rs::run_render_job makes.
async fn run_base_cut_job(
    app_handle: AppHandle,
    job_id: String,
    blender: crate::models::BlenderInfo,
    script: std::path::PathBuf,
    job: BaseCutJob,
    cancel_token: Arc<Notify>,
) {
    let total = job.placements.len() as u32;
    let script_dir = script
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let result = match job::write_job_file(&script_dir, &job, &job_id) {
        Ok(job_path) => {
            let app_handle_ref = &app_handle;
            let job_id_ref = &job_id;
            let outcome = job::spawn_and_parse(
                &blender,
                &script,
                &job_path,
                &cancel_token,
                |token| handle_token(app_handle_ref, job_id_ref, token),
            )
            .await;
            std::fs::remove_file(&job_path).ok();
            outcome
        }
        Err(e) => Err((e, String::new())),
    };

    if let Ok(mut active) = ACTIVE_BASE_CUT.lock() {
        if active.as_ref().is_some_and(|(id, _)| id == &job_id) {
            *active = None;
        }
    }

    match result {
        Ok(ok_count) => {
            BaseCutStatus::Finished(BaseCutFinishedStatus {
                job_id,
                ok_count,
                total,
            })
            .emit(&app_handle)
            .ok();
        }
        Err((AppError::UserCancelled(_), _stdout_tail)) => {
            BaseCutStatus::Cancelled(BaseCutCancelledStatus { job_id })
                .emit(&app_handle)
                .ok();
        }
        Err((e, stdout_tail)) => {
            BaseCutStatus::Failed(BaseCutFailedStatus {
                job_id,
                message: e.to_string(),
                stdout_tail,
            })
            .emit(&app_handle)
            .ok();
        }
    }
}

/// Translate one BaseCutToken into its BaseCutStatus event and emit it.
fn handle_token(app_handle: &AppHandle, job_id: &str, token: &BaseCutToken) {
    match token {
        BaseCutToken::Validating => {
            BaseCutStatus::Validating(BaseCutValidatingStatus {
                job_id: job_id.to_string(),
            })
            .emit(app_handle)
            .ok();
        }
        // ValidationFailed also flows into the terminal Failed event
        // (spawn_and_parse kills the child and returns an error for it) —
        // surfacing it here too gives the frontend the report immediately
        // rather than only the flattened error string.
        BaseCutToken::Validated(report) | BaseCutToken::ValidationFailed(report) => {
            let report: BaseCutValidationReport =
                serde_json::from_value(report.clone()).unwrap_or_default();
            BaseCutStatus::Validated(BaseCutValidatedStatus {
                job_id: job_id.to_string(),
                report,
            })
            .emit(app_handle)
            .ok();
        }
        BaseCutToken::CutStart { index } => {
            BaseCutStatus::CutStarted(BaseCutCutStartedStatus {
                job_id: job_id.to_string(),
                index: *index,
            })
            .emit(app_handle)
            .ok();
        }
        BaseCutToken::CutDone {
            index,
            out,
            dims_mm,
            manifold,
            fused,
            shells,
            topper_mm_clamped,
            magnet_ignored,
            scatter_skipped,
            glb,
        } => {
            BaseCutStatus::CutDone(BaseCutCutDoneStatus {
                job_id: job_id.to_string(),
                index: *index,
                out_path: out.clone(),
                dims_mm: *dims_mm,
                manifold: *manifold,
                fused: *fused,
                shells: *shells,
                topper_mm_clamped: *topper_mm_clamped,
                magnet_ignored: *magnet_ignored,
                scatter_skipped: *scatter_skipped,
                glb_path: glb.clone(),
            })
            .emit(app_handle)
            .ok();
        }
        BaseCutToken::CutFailed { index, reason } => {
            BaseCutStatus::CutFailed(BaseCutCutFailedStatus {
                job_id: job_id.to_string(),
                index: *index,
                reason: reason.clone(),
            })
            .emit(app_handle)
            .ok();
        }
        // JOB_DONE carries the authoritative ok/total counts, but the
        // Finished event is emitted once by run_base_cut_job after
        // spawn_and_parse returns (it also needs to know whether the run
        // errored) — nothing to do here.
        BaseCutToken::JobDone { .. } => {}
    }
}

/// The pseudo-designer folder cut output lands under (docs/BASECUTTER.md
/// phase 5, "export-into-catalog"). A real studio name would misattribute
/// bases the user cut themselves; "Plinth Bases" reads as its own shelf and
/// — per catalog::layout's designer/release/model tiers — the group name
/// and cut date stay in SEPARATE segments (see export_cuts's doc comment
/// for why that separation is the whole reason this parses cleanly).
const PLINTH_DESIGNER: &str = "Plinth Bases";

/// A successful cut as it crosses from the transient Blender output area
/// into the durable catalog. Identity and footprint travel explicitly — a
/// filename collision suffix is storage trivia, never model metadata.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct CutCatalogArtifact {
    pub id: String,
    pub source_path: String,
    pub cutter: CutterKind,
    pub mode: CutCatalogMode,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Type)]
#[serde(rename_all = "lowercase")]
pub enum CutCatalogMode {
    Base,
    Topper,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, Type)]
pub struct CutCatalogExportSummary {
    pub release_dir: String,
    pub added: u32,
    pub updated: u32,
    pub unchanged: u32,
    pub repaired: u32,
    pub warnings: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, Type)]
pub struct PlinthRepairSummary {
    pub repaired: u32,
    pub unchanged: u32,
    pub warnings: Vec<String>,
}

/// Proleptic-Gregorian (year, month, day) from a Unix timestamp, UTC. No
/// date/time crate is in Cargo.toml (elsewhere in the codebase a raw
/// SystemTime/UNIX_EPOCH duration is the calendar-free norm, e.g.
/// catalog/pack.rs's packed_at) — this is Howard Hinnant's well-known
/// division-only civil-from-days conversion:
/// http://howardhinnant.github.io/date_algorithms.html
fn civil_from_unix_seconds(secs: u64) -> (i64, u32, u32) {
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
}

/// "This month" in UTC — stamps the export folder's release date with when
/// the copy actually happened.
fn current_year_month_utc() -> (i64, u32) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (year, month, _day) = civil_from_unix_seconds(secs);
    (year, month)
}

fn format_mm(value: f64) -> String {
    if value.fract().abs() < f64::EPSILON {
        format!("{}", value as i64)
    } else {
        let formatted = format!("{value:.3}");
        formatted
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

fn footprint_label(cutter: &CutterKind) -> String {
    match cutter {
        CutterKind::Circle { diameter_mm } => format!("{} mm Round", format_mm(*diameter_mm)),
        CutterKind::Ellipse { major_mm, minor_mm } => {
            format!("{}×{} mm Oval", format_mm(*major_mm), format_mm(*minor_mm))
        }
        CutterKind::Rect { width_mm, depth_mm } if (width_mm - depth_mm).abs() < f64::EPSILON => {
            format!("{} mm Square", format_mm(*width_mm))
        }
        CutterKind::Rect { width_mm, depth_mm } => format!(
            "{}×{} mm Rectangle",
            format_mm(*width_mm),
            format_mm(*depth_mm)
        ),
    }
}

fn parse_release_segment(name: &str) -> Option<(&str, &str)> {
    let bytes = name.as_bytes();
    if bytes.len() < 9
        || bytes[4] != b'-'
        || bytes[7] != b' '
        || !bytes[..4].iter().all(u8::is_ascii_digit)
        || !bytes[5..7].iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    let month: u32 = name[5..7].parse().ok()?;
    if !(1..=12).contains(&month) {
        return None;
    }
    Some((&name[..7], name[8..].trim()))
}

fn sidecar_value(path: &Path) -> Result<serde_json::Value, AppError> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| AppError::IoError(format!("Failed to read {}: {e}", path.display())))?;
    serde_json::from_str(&contents)
        .map_err(|e| AppError::JsonError(format!("Failed to parse {}: {e}", path.display())))
}

fn write_sidecar_value(path: &Path, value: &serde_json::Value) -> Result<(), AppError> {
    let contents = serde_json::to_string_pretty(value)
        .map_err(|e| AppError::JsonError(format!("Failed to encode {}: {e}", path.display())))?;
    std::fs::write(path, contents)
        .map_err(|e| AppError::IoError(format!("Failed to write {}: {e}", path.display())))
}

fn normalized_name_key(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Bring early Base Cutter exports up to the same metadata contract as new
/// ones. Files do not move here: cleanup remains the one explicit folder
/// mutation workflow. Existing sidecar fields are preserved unless they are
/// absent; names gain their collection prefix so bare `round32-1` identities
/// cannot merge across releases in the globally name-keyed catalog.
pub fn repair_plinth_base_exports_in_root(root: &Path) -> Result<PlinthRepairSummary, AppError> {
    repair_plinth_base_exports_with_names(root, &mut HashSet::new())
}

fn repair_plinth_base_exports_with_names(
    root: &Path,
    used_names: &mut HashSet<String>,
) -> Result<PlinthRepairSummary, AppError> {
    let mut summary = PlinthRepairSummary::default();
    let designer_dir = root.join(PLINTH_DESIGNER);
    if !designer_dir.is_dir() {
        return Ok(summary);
    }
    let releases = std::fs::read_dir(&designer_dir).map_err(|e| {
        AppError::IoError(format!("Failed to inspect {}: {e}", designer_dir.display()))
    })?;
    let mut release_paths = releases
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    release_paths.sort();
    for release_path in release_paths {
        let release_label = release_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some((release_date, collection)) = parse_release_segment(&release_label) else {
            continue;
        };
        if collection.is_empty() {
            continue;
        }
        let model_entries = match std::fs::read_dir(&release_path) {
            Ok(entries) => entries,
            Err(e) => {
                summary
                    .warnings
                    .push(format!("Could not inspect {}: {e}", release_path.display()));
                continue;
            }
        };
        let mut model_paths = model_entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        model_paths.sort();
        for model_path in model_paths {
            let sidecar_path = model_path.join("model.json");
            if !sidecar_path.is_file() {
                continue;
            }
            let mut value = match sidecar_value(&sidecar_path) {
                Ok(value) if value.is_object() => value,
                Ok(_) => {
                    summary
                        .warnings
                        .push(format!("{} is not a JSON object", sidecar_path.display()));
                    continue;
                }
                Err(e) => {
                    summary.warnings.push(e.to_string());
                    continue;
                }
            };
            let object = value.as_object_mut().expect("checked object above");
            let before = serde_json::to_string(object).unwrap_or_default();
            if object
                .get("id")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|id| Uuid::parse_str(id).is_err())
            {
                object.insert(
                    "id".to_string(),
                    serde_json::Value::String(Uuid::new_v4().to_string()),
                );
            }
            object.insert(
                "designer".to_string(),
                serde_json::Value::String(PLINTH_DESIGNER.to_string()),
            );
            object.insert(
                "release_name".to_string(),
                serde_json::Value::String(collection.to_string()),
            );
            object.insert(
                "release_date".to_string(),
                serde_json::Value::String(release_date.to_string()),
            );

            let old_name = object
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(String::from)
                .unwrap_or_else(|| {
                    model_path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "Generated base".to_string())
                });
            let mut repaired_name =
                if normalized_name_key(&old_name).starts_with(&normalized_name_key(collection)) {
                    old_name
                } else {
                    format!("{collection} — {old_name}")
                };
            if !used_names.insert(repaired_name.to_lowercase()) {
                let base = repaired_name.clone();
                for ordinal in 2.. {
                    let candidate = format!("{base} — {ordinal:02}");
                    if used_names.insert(candidate.to_lowercase()) {
                        repaired_name = candidate;
                        break;
                    }
                }
            }
            object.insert("name".to_string(), serde_json::Value::String(repaired_name));

            let tags = object
                .entry("tags")
                .or_insert_with(|| serde_json::Value::Array(Vec::new()));
            if let Some(tags) = tags.as_array_mut() {
                for tag in ["generated", "terrain base"] {
                    if !tags.iter().any(|value| value.as_str() == Some(tag)) {
                        tags.push(serde_json::Value::String(tag.to_string()));
                    }
                }
            }
            object
                .entry("generated")
                .or_insert_with(|| serde_json::json!({ "tool": "base_cutter", "migrated": true }));

            let after = serde_json::to_string(object).unwrap_or_default();
            if before == after {
                summary.unchanged += 1;
            } else if let Err(e) = write_sidecar_value(&sidecar_path, &value) {
                summary.warnings.push(e.to_string());
            } else {
                summary.repaired += 1;
            }
        }
    }
    Ok(summary)
}

fn artifact_id_in_dir(dir: &Path) -> Option<String> {
    sidecar_value(&dir.join("model.json"))
        .ok()?
        .get("id")?
        .as_str()
        .map(String::from)
}

fn find_artifact_dir(roots: &[String], id: &str) -> Option<PathBuf> {
    for root in roots {
        let designer_dir = Path::new(root).join(PLINTH_DESIGNER);
        let Ok(releases) = std::fs::read_dir(designer_dir) else {
            continue;
        };
        for release in releases.flatten().filter(|entry| entry.path().is_dir()) {
            let Ok(models) = std::fs::read_dir(release.path()) else {
                continue;
            };
            for model in models.flatten().filter(|entry| entry.path().is_dir()) {
                if artifact_id_in_dir(&model.path()).as_deref() == Some(id) {
                    return Some(model.path());
                }
            }
        }
    }
    None
}

fn all_plinth_model_names(roots: &[String]) -> HashSet<String> {
    let mut names = HashSet::new();
    for root in roots {
        let designer_dir = Path::new(root).join(PLINTH_DESIGNER);
        let Ok(releases) = std::fs::read_dir(designer_dir) else {
            continue;
        };
        for release in releases.flatten().filter(|entry| entry.path().is_dir()) {
            let Ok(models) = std::fs::read_dir(release.path()) else {
                continue;
            };
            for model in models.flatten().filter(|entry| entry.path().is_dir()) {
                if let Ok(value) = sidecar_value(&model.path().join("model.json")) {
                    if let Some(name) = value.get("name").and_then(serde_json::Value::as_str) {
                        names.insert(name.to_lowercase());
                    }
                }
            }
        }
    }
    names
}

fn next_artifact_name(
    collection: &str,
    cutter: &CutterKind,
    mode: CutCatalogMode,
    used_names: &mut HashSet<String>,
) -> String {
    let mode_label = if mode == CutCatalogMode::Topper {
        " Topper"
    } else {
        ""
    };
    let base = format!("{collection} — {}{mode_label}", footprint_label(cutter));
    for ordinal in 1.. {
        let candidate = format!("{base} — {ordinal:02}");
        if used_names.insert(candidate.to_lowercase()) {
            return candidate;
        }
    }
    unreachable!("ran out of artifact ordinals")
}

fn artifact_tags(cutter: &CutterKind, mode: CutCatalogMode) -> Vec<String> {
    let shape = match cutter {
        CutterKind::Circle { .. } => "round",
        CutterKind::Ellipse { .. } => "oval",
        CutterKind::Rect { width_mm, depth_mm } if (width_mm - depth_mm).abs() < f64::EPSILON => {
            "square"
        }
        CutterKind::Rect { .. } => "rectangle",
    };
    vec![
        "generated".to_string(),
        if mode == CutCatalogMode::Topper {
            "terrain topper".to_string()
        } else {
            "terrain base".to_string()
        },
        shape.to_string(),
        footprint_label(cutter).to_lowercase(),
    ]
}

fn artifact_sidecar(
    artifact: &CutCatalogArtifact,
    model_name: &str,
    collection: &str,
    release_date: &str,
    existing: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut value = existing
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({ "name": model_name }));
    let object = value.as_object_mut().expect("json object");
    object.insert(
        "id".to_string(),
        serde_json::Value::String(artifact.id.clone()),
    );
    object
        .entry("name")
        .or_insert_with(|| serde_json::Value::String(model_name.to_string()));
    object.insert(
        "designer".to_string(),
        serde_json::Value::String(PLINTH_DESIGNER.to_string()),
    );
    object.insert(
        "release_name".to_string(),
        serde_json::Value::String(collection.to_string()),
    );
    object.insert(
        "release_date".to_string(),
        serde_json::Value::String(release_date.to_string()),
    );
    let tags = object
        .entry("tags")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    if let Some(tags) = tags.as_array_mut() {
        for tag in artifact_tags(&artifact.cutter, artifact.mode) {
            if !tags.iter().any(|value| value.as_str() == Some(&tag)) {
                tags.push(serde_json::Value::String(tag));
            }
        }
    }
    object.insert(
        "generated".to_string(),
        serde_json::json!({
            "tool": "base_cutter",
            "mode": match artifact.mode { CutCatalogMode::Base => "base", CutCatalogMode::Topper => "topper" },
            "footprint": artifact.cutter,
        }),
    );
    match artifact.cutter {
        CutterKind::Circle { diameter_mm } => {
            object.insert(
                "base_round_mm".to_string(),
                serde_json::Value::String(format_mm(diameter_mm)),
            );
        }
        CutterKind::Rect { width_mm, depth_mm } if (width_mm - depth_mm).abs() < f64::EPSILON => {
            object.insert(
                "base_square_mm".to_string(),
                serde_json::Value::String(format_mm(width_mm)),
            );
        }
        _ => {}
    }
    value
}

fn copy_artifact_file(source: &Path, destination: &Path) -> Result<bool, AppError> {
    if destination.is_file() && crate::catalog::normalize::same_content(destination, source) {
        return Ok(false);
    }
    std::fs::copy(source, destination).map_err(|e| {
        AppError::IoError(format!(
            "Failed to copy {} to {}: {e}",
            source.display(),
            destination.display()
        ))
    })?;
    Ok(true)
}

pub fn export_cut_artifacts(
    artifacts: &[CutCatalogArtifact],
    root: &str,
    collection: &str,
    catalog_roots: &[String],
    year_month: (i64, u32),
) -> Result<CutCatalogExportSummary, AppError> {
    if artifacts.is_empty() {
        return Err(AppError::InvalidInput(
            "No cut artifacts to export".to_string(),
        ));
    }
    let collection = collection.trim();
    if collection.is_empty() {
        return Err(AppError::InvalidInput(
            "A collection name is required".to_string(),
        ));
    }
    let root_norm = crate::catalog::commands::normalized_root(root);
    let catalog_roots = catalog_roots
        .iter()
        .map(|candidate| crate::catalog::commands::normalized_root(candidate))
        .collect::<Vec<_>>();
    if !catalog_roots
        .iter()
        .any(|candidate| candidate == &root_norm)
    {
        return Err(AppError::InvalidInput(format!(
            "'{root}' is not a configured catalog folder — add it in Settings first"
        )));
    }
    let root_path = Path::new(&root_norm);
    if !root_path.is_dir() {
        return Err(AppError::NotFoundError(format!(
            "Catalog folder not found: {root_norm}"
        )));
    }

    let mut ids = HashSet::new();
    for artifact in artifacts {
        Uuid::parse_str(&artifact.id).map_err(|_| {
            AppError::InvalidInput(format!("Invalid cut artifact ID: {}", artifact.id))
        })?;
        if !ids.insert(artifact.id.to_lowercase()) {
            return Err(AppError::InvalidInput(format!(
                "Cut artifact {} is listed twice",
                artifact.id
            )));
        }
        let source = Path::new(&artifact.source_path);
        if !source.is_file()
            || !source
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("stl"))
        {
            return Err(AppError::InvalidInput(format!(
                "Cut artifact is not a readable STL: {}",
                artifact.source_path
            )));
        }
    }

    // Catalog scans are rooted operations. Updating a stable artifact in a
    // different configured root while the caller scans only `root` would
    // leave that other root's index stale, so fail before writing anything.
    for artifact in artifacts {
        if let Some(existing_dir) = find_artifact_dir(&catalog_roots, &artifact.id) {
            if !existing_dir.starts_with(root_path) {
                let owner = catalog_roots
                    .iter()
                    .find(|candidate| existing_dir.starts_with(Path::new(candidate)))
                    .map(String::as_str)
                    .unwrap_or("another configured catalog folder");
                return Err(AppError::InvalidInput(format!(
                    "Cut artifact {} is already cataloged under {owner}; export it to that catalog folder to update it",
                    artifact.id
                )));
            }
        }
    }

    let repaired = repair_plinth_base_exports_in_root(root_path)?;
    let release_date = format!("{:04}-{:02}", year_month.0, year_month.1);
    let release_dir = crate::catalog::layout::release_dir(
        root_path,
        PLINTH_DESIGNER,
        collection,
        Some(&release_date),
    );
    std::fs::create_dir_all(&release_dir)?;
    let mut used_names = all_plinth_model_names(&catalog_roots);
    let mut summary = CutCatalogExportSummary {
        release_dir: release_dir.to_string_lossy().into_owned(),
        repaired: repaired.repaired,
        warnings: repaired.warnings,
        ..Default::default()
    };

    for artifact in artifacts {
        let existing = find_artifact_dir(&catalog_roots, &artifact.id);
        let existing_value = existing
            .as_ref()
            .and_then(|dir| sidecar_value(&dir.join("model.json")).ok())
            .filter(serde_json::Value::is_object);
        let model_name = existing_value
            .as_ref()
            .and_then(|value| value.get("name"))
            .and_then(serde_json::Value::as_str)
            .map(String::from)
            .unwrap_or_else(|| {
                next_artifact_name(collection, &artifact.cutter, artifact.mode, &mut used_names)
            });
        let effective_collection = existing_value
            .as_ref()
            .and_then(|value| value.get("release_name"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or(collection)
            .to_string();
        let effective_release_date = existing_value
            .as_ref()
            .and_then(|value| value.get("release_date"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&release_date)
            .to_string();
        if let Some(existing_dir) = &existing {
            if existing_dir.parent() != Some(release_dir.as_path()) {
                summary.warnings.push(format!(
                    "{} was already cataloged in {}; its stable identity was updated there instead of duplicated",
                    model_name,
                    existing_dir.display()
                ));
            }
        }
        let model_dir = existing.unwrap_or_else(|| {
            crate::catalog::layout::model_dir(
                root_path,
                PLINTH_DESIGNER,
                Some(collection),
                Some(&release_date),
                &model_name,
            )
        });
        let was_existing = model_dir.is_dir();
        std::fs::create_dir_all(&model_dir)?;
        let file_stem = crate::catalog::layout::sanitize_segment(&model_name);
        let dest_stl = if was_existing {
            std::fs::read_dir(&model_dir)
                .ok()
                .and_then(|entries| {
                    entries.flatten().map(|entry| entry.path()).find(|path| {
                        path.is_file()
                            && path
                                .extension()
                                .is_some_and(|extension| extension.eq_ignore_ascii_case("stl"))
                    })
                })
                .unwrap_or_else(|| model_dir.join(format!("{file_stem}.stl")))
        } else {
            model_dir.join(format!("{file_stem}.stl"))
        };
        let changed = copy_artifact_file(Path::new(&artifact.source_path), &dest_stl)?;
        let source_glb = Path::new(&artifact.source_path).with_extension("glb");
        if source_glb.is_file() {
            copy_artifact_file(&source_glb, &dest_stl.with_extension("glb"))?;
        }
        let sidecar_path = model_dir.join("model.json");
        let existing_sidecar = sidecar_value(&sidecar_path).ok();
        write_sidecar_value(
            &sidecar_path,
            &artifact_sidecar(
                artifact,
                &model_name,
                &effective_collection,
                &effective_release_date,
                existing_sidecar.or(existing_value),
            ),
        )?;
        if !was_existing {
            summary.added += 1;
        } else if changed {
            summary.updated += 1;
        } else {
            summary.unchanged += 1;
        }
    }
    Ok(summary)
}

#[tauri::command]
#[specta::specta]
pub async fn export_cuts_to_catalog(
    app_handle: AppHandle,
    artifacts: Vec<CutCatalogArtifact>,
    root: String,
    collection: String,
) -> Result<CutCatalogExportSummary, AppError> {
    let settings = crate::settings::get_settings(app_handle.clone())
        .await
        .map_err(AppError::ConfigError)?;
    let catalog_roots = settings.catalog_roots.unwrap_or_default();
    let year_month = current_year_month_utc();
    tauri::async_runtime::spawn_blocking(move || {
        export_cut_artifacts(&artifacts, &root, &collection, &catalog_roots, year_month)
    })
    .await
    .map_err(|e| AppError::IoError(format!("Export task panicked: {}", e)))?
}

#[tauri::command]
#[specta::specta]
pub async fn repair_plinth_base_exports(
    app_handle: AppHandle,
) -> Result<PlinthRepairSummary, AppError> {
    let settings = crate::settings::get_settings(app_handle)
        .await
        .map_err(AppError::ConfigError)?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut total = PlinthRepairSummary::default();
        let mut used_names = HashSet::new();
        for root in settings.catalog_roots.unwrap_or_default() {
            match repair_plinth_base_exports_with_names(Path::new(&root), &mut used_names) {
                Ok(summary) => {
                    total.repaired += summary.repaired;
                    total.unchanged += summary.unchanged;
                    total.warnings.extend(summary.warnings);
                }
                Err(error) => total.warnings.push(error.to_string()),
            }
        }
        Ok(total)
    })
    .await
    .map_err(|e| AppError::IoError(format!("Base export repair task panicked: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::scanner::ModelJson;

    fn placement(name: Option<&str>, cutter: CutterKind) -> Placement {
        Placement {
            cutter,
            x_mm: 0.0,
            y_mm: 0.0,
            rotation_deg: 0.0,
            magnet: None,
            name: name.map(str::to_string),
        }
    }

    #[test]
    fn accepts_a_normal_placement_set() {
        let placements = vec![
            placement(Some("round32"), CutterKind::Circle { diameter_mm: 32.0 }),
            placement(
                Some("square25"),
                CutterKind::Rect {
                    width_mm: 25.0,
                    depth_mm: 25.0,
                },
            ),
            placement(None, CutterKind::Circle { diameter_mm: 40.0 }),
        ];
        assert!(validate_placements(&placements, &PlinthParams::default()).is_ok());
    }

    #[test]
    fn rejects_a_footprint_the_taper_eats_entirely() {
        // A 2mm circle under the default plinth (inset ~= 0.991mm/side,
        // shrink ~= 1.983mm) derives to a footprint under the 1.0mm floor.
        let placements = vec![placement(
            Some("tiny"),
            CutterKind::Circle { diameter_mm: 2.0 },
        )];
        let err = validate_placements(&placements, &PlinthParams::default())
            .expect_err("a near-zero cut footprint must be rejected");
        let msg = err.to_string();
        assert!(msg.contains("'tiny'"), "error should name the placement: {msg}");
        assert!(
            msg.contains("taper") || msg.contains("footprint"),
            "error should explain why: {msg}"
        );
    }

    #[test]
    fn rejects_a_footprint_the_taper_eats_entirely_using_the_ordinal_when_unnamed() {
        let placements = vec![placement(None, CutterKind::Circle { diameter_mm: 2.0 })];
        let err = validate_placements(&placements, &PlinthParams::default())
            .expect_err("must still be rejected without a name");
        assert!(
            err.to_string().contains("placement 1"),
            "error should fall back to a 1-based ordinal: {err}"
        );
    }

    #[test]
    fn rejects_duplicate_placement_names() {
        let placements = vec![
            placement(Some("round32"), CutterKind::Circle { diameter_mm: 32.0 }),
            placement(Some("round32"), CutterKind::Circle { diameter_mm: 40.0 }),
        ];
        let err = validate_placements(&placements, &PlinthParams::default())
            .expect_err("duplicate names must be rejected");
        assert!(
            err.to_string().contains("round32"),
            "error should name the duplicate: {err}"
        );
    }

    // ---- validate_topper_mm (docs/BASECUTTER.md's BaseCutJob.topper_mm) ----

    #[test]
    fn topper_mm_none_is_always_fine() {
        assert!(validate_topper_mm(None).is_ok());
    }

    #[test]
    fn topper_mm_accepts_any_sane_positive_value() {
        // Rust's guard is deliberately looser than base_cut.py's [1.0, 3.0]
        // clamp — values outside that range are still valid REQUESTS, the
        // script just clamps and echoes back the adjustment.
        assert!(validate_topper_mm(Some(1.5)).is_ok());
        assert!(validate_topper_mm(Some(0.1)).is_ok());
        assert!(validate_topper_mm(Some(10.0)).is_ok());
    }

    #[test]
    fn topper_mm_rejects_zero_and_negative() {
        for bad in [0.0, -1.0, -0.001] {
            let err = validate_topper_mm(Some(bad)).expect_err("must reject non-positive");
            assert!(err.to_string().contains("positive"), "{err}");
        }
    }

    #[test]
    fn topper_mm_rejects_absurdly_large_values() {
        let err = validate_topper_mm(Some(10.001)).expect_err("must reject > 10mm");
        assert!(err.to_string().contains("unreasonably large"), "{err}");
    }

    #[test]
    fn topper_mm_rejects_non_finite() {
        let err = validate_topper_mm(Some(f64::NAN)).expect_err("must reject NaN");
        assert!(err.to_string().contains("finite"), "{err}");
        let err = validate_topper_mm(Some(f64::INFINITY)).expect_err("must reject infinity");
        assert!(err.to_string().contains("finite"), "{err}");
    }

    // ---- placement.name path-traversal guard (IPC is the trust boundary:
    // a caller of the Tauri bridge can send any string, regardless of what
    // the frontend's char-blocklist would allow through a UI) ----

    #[test]
    fn rejects_placement_names_that_escape_out_dir() {
        for bad in ["../evil", "a/b", "a\\b", "C:evil", "/etc/passwd", "..", "."] {
            let placements = vec![placement(
                Some(bad),
                CutterKind::Circle { diameter_mm: 32.0 },
            )];
            let err = validate_placements(&placements, &PlinthParams::default())
                .expect_err(&format!("'{bad}' must be rejected as a placement name"));
            assert!(
                err.to_string().contains("not a valid output filename"),
                "error should explain why '{bad}' was rejected: {err}"
            );
        }
    }

    #[test]
    fn accepts_an_ordinary_placement_name() {
        let placements = vec![placement(
            Some("round32"),
            CutterKind::Circle { diameter_mm: 32.0 },
        )];
        assert!(validate_placements(&placements, &PlinthParams::default()).is_ok());
    }

    #[test]
    fn allows_multiple_unnamed_placements() {
        // Unnamed placements get index-derived output names (base_0.stl,
        // base_1.stl, ...) in base_cut.py, so they never collide with each
        // other even though `name` is None for both.
        let placements = vec![
            placement(None, CutterKind::Circle { diameter_mm: 32.0 }),
            placement(None, CutterKind::Circle { diameter_mm: 32.0 }),
        ];
        assert!(validate_placements(&placements, &PlinthParams::default()).is_ok());
    }

    // ---- export_cuts_to_catalog (docs/BASECUTTER.md phase 5) ----

    /// civil_from_unix_seconds pinned against `date -u -r <secs>` reference
    /// points (epoch, a Y2K leap day, a 2024 leap day, and a 2026 date) so a
    /// future edit to the conversion can't silently drift the calendar.
    #[test]
    fn civil_from_unix_seconds_matches_known_dates() {
        assert_eq!(civil_from_unix_seconds(0), (1970, 1, 1));
        assert_eq!(civil_from_unix_seconds(951_782_400), (2000, 2, 29));
        assert_eq!(civil_from_unix_seconds(1_709_164_800), (2024, 2, 29));
        assert_eq!(civil_from_unix_seconds(1_784_073_600), (2026, 7, 15));
        assert_eq!(civil_from_unix_seconds(1_767_225_600), (2026, 1, 1));
        assert_eq!(civil_from_unix_seconds(946_598_400), (1999, 12, 31));
    }

    /// A temp dir standing in for one catalog root, cleaned up on drop via
    /// its own Drop impl — the same manual-cleanup style scanner.rs's tests
    /// use (this crate has no tempfile dependency).
    struct TempRoot(PathBuf);
    impl TempRoot {
        fn new(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "stlpack_basecutter_export_{}_{}",
                label,
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempRoot {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn write_stub_stl(dir: &Path, name: &str) -> String {
        let path = dir.join(name);
        std::fs::write(&path, b"solid stub\nendsolid stub\n").unwrap();
        path.to_string_lossy().into_owned()
    }

    /// Compatibility harness for the historical export tests below. It
    /// deliberately routes through the structured production exporter;
    /// content-derived UUIDs make a byte-identical re-export the same
    /// artifact while different geometry remains a distinct artifact.
    fn export_cuts(
        paths: &[String],
        root: &str,
        collection: &str,
        catalog_roots: &[String],
        year_month: (i64, u32),
    ) -> Result<String, AppError> {
        let artifacts = paths
            .iter()
            .map(|path| {
                let bytes = std::fs::read(path).unwrap_or_default();
                let stem = Path::new(path)
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("cut");
                let mut identity = stem.as_bytes().to_vec();
                identity.extend_from_slice(&bytes);
                let hash = blake3::hash(&identity);
                let mut id = [0_u8; 16];
                id.copy_from_slice(&hash.as_bytes()[..16]);
                CutCatalogArtifact {
                    id: Uuid::from_bytes(id).to_string(),
                    source_path: path.clone(),
                    cutter: if stem.starts_with("square25") {
                        CutterKind::Rect {
                            width_mm: 25.0,
                            depth_mm: 25.0,
                        }
                    } else {
                        CutterKind::Circle { diameter_mm: 32.0 }
                    },
                    mode: CutCatalogMode::Base,
                }
            })
            .collect::<Vec<_>>();
        export_cut_artifacts(&artifacts, root, collection, catalog_roots, year_month)
            .map(|summary| summary.release_dir)
    }

    /// Pins the folder shape this module deliberately chose: three tiers
    /// (Designer/Release/Model) so the release-date segment never has to
    /// double as the model's name segment — see export_cuts's doc comment.
    #[test]
    fn export_places_each_cut_under_its_own_model_folder() {
        let root = TempRoot::new("layout");
        let src = TempRoot::new("layout_src");
        let round = write_stub_stl(src.path(), "round32.stl");
        let square = write_stub_stl(src.path(), "square25.stl");
        let roots = vec![root.path().to_string_lossy().into_owned()];

        let dest = export_cuts(
            &[round, square],
            &root.path().to_string_lossy(),
            "Test Regiment",
            &roots,
            (2026, 7),
        )
        .expect("export should succeed");

        assert_eq!(
            Path::new(&dest),
            root.path()
                .join("Plinth Bases")
                .join("2026-07 Test Regiment")
        );
        assert!(root
            .path()
            .join("Plinth Bases/2026-07 Test Regiment/Test Regiment — 32 mm Round — 01/Test Regiment — 32 mm Round — 01.stl")
            .is_file());
        assert!(root
            .path()
            .join("Plinth Bases/2026-07 Test Regiment/Test Regiment — 25 mm Square — 01/Test Regiment — 25 mm Square — 01.stl")
            .is_file());
    }

    // ---- .glb sidecar copy (VTT GLB export design doc "Base cut":
    // "commands.rs: export_cuts_to_catalog copies the .glb sidecar when it
    // exists next to a cut STL") ----

    /// A glb-mode cut's `.glb` twin (same stem, next to the STL — exactly
    /// what base_cut.py writes) rides along into the catalog under the
    /// same name the STL itself landed under.
    #[test]
    fn export_copies_a_glb_sidecar_when_present() {
        let root = TempRoot::new("glb_sidecar");
        let src = TempRoot::new("glb_sidecar_src");
        let stl = write_stub_stl(src.path(), "round32.stl");
        let glb_contents = b"glTF\x02\x00\x00\x00stub-binary-glb-payload";
        std::fs::write(src.path().join("round32.glb"), glb_contents).unwrap();
        let roots = vec![root.path().to_string_lossy().into_owned()];

        export_cuts(
            &[stl],
            &root.path().to_string_lossy(),
            "Group",
            &roots,
            (2026, 7),
        )
        .expect("export should succeed");

        let dest_glb = root.path().join(
            "Plinth Bases/2026-07 Group/Group — 32 mm Round — 01/Group — 32 mm Round — 01.glb",
        );
        assert!(
            dest_glb.is_file(),
            "expected the .glb sidecar to be copied alongside the STL"
        );
        assert_eq!(
            std::fs::read(&dest_glb).unwrap(),
            glb_contents,
            "the copied .glb must be byte-identical to the source sidecar"
        );
    }

    /// glb:false cuts (and any STL that never had a base_cut.py sidecar at
    /// all) must not spuriously grow a `.glb` file in the catalog.
    #[test]
    fn export_skips_glb_copy_when_no_sidecar_exists() {
        let root = TempRoot::new("no_glb_sidecar");
        let src = TempRoot::new("no_glb_sidecar_src");
        let stl = write_stub_stl(src.path(), "round32.stl");
        let roots = vec![root.path().to_string_lossy().into_owned()];

        export_cuts(
            &[stl],
            &root.path().to_string_lossy(),
            "Group",
            &roots,
            (2026, 7),
        )
        .expect("export should succeed");

        let dest_glb = root.path().join(
            "Plinth Bases/2026-07 Group/Group — 32 mm Round — 01/Group — 32 mm Round — 01.glb",
        );
        assert!(
            !dest_glb.is_file(),
            "no sidecar existed on disk, so none should have been copied"
        );
    }

    /// Re-exporting the same UUID updates the one GLB twin in place instead
    /// of growing filename-suffixed duplicate parts.
    #[test]
    fn reexport_updates_one_glb_twin_in_place() {
        let root = TempRoot::new("glb_sidecar_suffix");
        let roots = vec![root.path().to_string_lossy().into_owned()];

        let src1 = TempRoot::new("glb_sidecar_suffix_src1");
        let first = write_stub_stl(src1.path(), "round32.stl");
        std::fs::write(src1.path().join("round32.glb"), b"first-glb").unwrap();
        export_cuts(
            &[first],
            &root.path().to_string_lossy(),
            "Group",
            &roots,
            (2026, 7),
        )
        .unwrap();

        let src2 = TempRoot::new("glb_sidecar_suffix_src2");
        let second = write_stub_stl(src2.path(), "round32.stl");
        std::fs::write(src2.path().join("round32.glb"), b"second-glb").unwrap();
        export_cuts(
            &[second],
            &root.path().to_string_lossy(),
            "Group",
            &roots,
            (2026, 7),
        )
        .unwrap();

        let model_dir = root
            .path()
            .join("Plinth Bases/2026-07 Group/Group — 32 mm Round — 01");
        let glb = model_dir.join("Group — 32 mm Round — 01.glb");
        assert_eq!(std::fs::read(glb).unwrap(), b"second-glb");
        assert_eq!(
            std::fs::read_dir(model_dir)
                .unwrap()
                .flatten()
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "glb"))
                .count(),
            1
        );
    }

    /// The scanner's designer resolution is model.json designer ->
    /// release.json -> a known-designers folder lexicon, in that order, and
    /// "Plinth Bases" is in none of them — without a sidecar an export would
    /// scan back in as an undesignered heuristic model. Parses the written
    /// file into the scanner's own ModelJson type, not just a raw JSON blob,
    /// so this fails if the shape the scanner expects ever changes underfoot.
    #[test]
    fn export_writes_a_model_json_sidecar_the_scanner_can_read() {
        let root = TempRoot::new("sidecar");
        let src = TempRoot::new("sidecar_src");
        let round = write_stub_stl(src.path(), "round32.stl");
        let roots = vec![root.path().to_string_lossy().into_owned()];

        export_cuts(
            &[round],
            &root.path().to_string_lossy(),
            "Test Regiment",
            &roots,
            (2026, 7),
        )
        .expect("export should succeed");

        let sidecar_path = root
            .path()
            .join("Plinth Bases/2026-07 Test Regiment/Test Regiment — 32 mm Round — 01/model.json");
        let contents = std::fs::read_to_string(&sidecar_path).expect("sidecar written");
        let parsed: ModelJson =
            serde_json::from_str(&contents).expect("sidecar must parse as the scanner's ModelJson");
        assert_eq!(parsed.name, "Test Regiment — 32 mm Round — 01");
        assert_eq!(parsed.designer.as_deref(), Some(PLINTH_DESIGNER));
        assert_eq!(parsed.release_name.as_deref(), Some("Test Regiment"));
        assert_eq!(parsed.release_date.as_deref(), Some("2026-07"));
        assert_eq!(parsed.base_round_mm.as_deref(), Some("32"));
    }

    #[test]
    fn export_rejects_a_root_not_in_catalog_roots() {
        let root = TempRoot::new("unconfigured");
        let src = TempRoot::new("unconfigured_src");
        let stl = write_stub_stl(src.path(), "round32.stl");

        let err = export_cuts(
            &[stl],
            &root.path().to_string_lossy(),
            "Group",
            &[], // nothing configured
            (2026, 7),
        )
        .expect_err("an unconfigured root must be rejected");
        assert!(
            err.to_string().contains("not a configured catalog folder"),
            "error should explain why: {err}"
        );
    }

    #[test]
    fn reexport_rejects_updating_an_artifact_in_another_catalog_root() {
        let first_root = TempRoot::new("cross_root_first");
        let second_root = TempRoot::new("cross_root_second");
        let src = TempRoot::new("cross_root_src");
        let source_path = write_stub_stl(src.path(), "round32.stl");
        let roots = vec![
            first_root.path().to_string_lossy().into_owned(),
            second_root.path().to_string_lossy().into_owned(),
        ];
        let artifact = CutCatalogArtifact {
            id: Uuid::new_v4().to_string(),
            source_path,
            cutter: CutterKind::Circle { diameter_mm: 32.0 },
            mode: CutCatalogMode::Base,
        };

        export_cut_artifacts(
            std::slice::from_ref(&artifact),
            &first_root.path().to_string_lossy(),
            "Group",
            &roots,
            (2026, 7),
        )
        .expect("first export should succeed");

        let error = export_cut_artifacts(
            &[artifact],
            &second_root.path().to_string_lossy(),
            "Group",
            &roots,
            (2026, 7),
        )
        .expect_err("a different root cannot be updated behind its index");

        assert!(error.to_string().contains("already cataloged under"));
        assert!(second_root
            .path()
            .read_dir()
            .expect("second root remains readable")
            .next()
            .is_none());
    }

    #[test]
    fn export_rejects_a_missing_source() {
        let root = TempRoot::new("missing_src");
        let roots = vec![root.path().to_string_lossy().into_owned()];
        let missing = root.path().join("nope.stl").to_string_lossy().into_owned();

        let err = export_cuts(
            std::slice::from_ref(&missing),
            &root.path().to_string_lossy(),
            "Group",
            &roots,
            (2026, 7),
        )
        .expect_err("a missing source must be rejected");
        assert!(
            err.to_string().contains(&missing),
            "error should name the missing source: {err}"
        );
    }

    #[test]
    fn export_rejects_the_same_path_listed_twice() {
        let root = TempRoot::new("dup_input");
        let src = TempRoot::new("dup_input_src");
        let stl = write_stub_stl(src.path(), "round32.stl");
        let roots = vec![root.path().to_string_lossy().into_owned()];

        let err = export_cuts(
            &[stl.clone(), stl],
            &root.path().to_string_lossy(),
            "Group",
            &roots,
            (2026, 7),
        )
        .expect_err("the same source listed twice must be rejected");
        assert!(
            err.to_string().contains("listed twice"),
            "error should explain why: {err}"
        );
    }

    /// Re-exporting the same artifact is idempotent: it remains one model
    /// with one STL rather than becoming a false multipart model.
    #[test]
    fn reexport_is_idempotent_instead_of_creating_another_part() {
        let root = TempRoot::new("collision");
        let src = TempRoot::new("collision_src");
        let roots = vec![root.path().to_string_lossy().into_owned()];

        let first = write_stub_stl(src.path(), "round32.stl");
        let dest1 = export_cuts(
            &[first],
            &root.path().to_string_lossy(),
            "Group",
            &roots,
            (2026, 7),
        )
        .unwrap();

        // A second cut that happens to produce the same file name (a fresh
        // temp dir, so this isn't the "same path twice" guard above).
        let second_src = TempRoot::new("collision_src2");
        let second = write_stub_stl(second_src.path(), "round32.stl");
        let dest2 = export_cuts(
            &[second],
            &root.path().to_string_lossy(),
            "Group",
            &roots,
            (2026, 7),
        )
        .unwrap();

        assert_eq!(dest1, dest2, "same group -> same release dir");
        let model_dir = Path::new(&dest1).join("Group — 32 mm Round — 01");
        assert!(model_dir.join("Group — 32 mm Round — 01.stl").is_file());
        assert_eq!(
            std::fs::read_dir(model_dir)
                .unwrap()
                .flatten()
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "stl"))
                .count(),
            1,
            "a byte-identical re-export must not create another part"
        );
    }

    /// Two different UUIDs remain two models even when Blender happened to
    /// give their scratch outputs the same stem in separate sessions.
    #[test]
    fn distinct_bases_sharing_a_default_stem_scan_as_separate_models() {
        let root = TempRoot::new("stem_collision");
        let roots = vec![root.path().to_string_lossy().into_owned()];

        let src1 = TempRoot::new("stem_collision_src1");
        let first = src1.path().join("round285.stl");
        std::fs::write(&first, b"solid base one\nendsolid base one\n").unwrap();
        export_cuts(
            &[first.to_string_lossy().into_owned()],
            &root.path().to_string_lossy(),
            "Sunday Batch",
            &roots,
            (2026, 7),
        )
        .expect("first export should succeed");

        // A second, later job run: a DIFFERENT base, but base_cut.py's
        // per-job unique_out_path independently starts back at the bare
        // "round285.stl" name in this run's own out_dir.
        let src2 = TempRoot::new("stem_collision_src2");
        let second = src2.path().join("round285.stl");
        std::fs::write(
            &second,
            b"solid base two, totally different geometry\nendsolid base two\n",
        )
        .unwrap();
        export_cuts(
            &[second.to_string_lossy().into_owned()],
            &root.path().to_string_lossy(),
            "Sunday Batch",
            &roots,
            (2026, 7),
        )
        .expect("second export should succeed");

        let cancel = std::sync::atomic::AtomicBool::new(false);
        let outcome =
            crate::catalog::scanner::scan(root.path(), &cancel, &[], |_, _| {}).unwrap();

        assert_eq!(
            outcome.models.len(),
            2,
            "two unrelated bases must scan as two models, not one multi-part model: {:?}",
            outcome
                .models
                .iter()
                .map(|m| (m.dir_path.clone(), m.file_count))
                .collect::<Vec<_>>()
        );
        for model in &outcome.models {
            assert_eq!(
                model.file_count, 1,
                "each base must own its files, not absorb a sibling cut's file"
            );
        }
    }

    /// Generated metadata is merged into an existing sidecar rather than
    /// replacing user-owned fields such as a curated display name.
    #[test]
    fn export_does_not_overwrite_an_existing_model_json_sidecar() {
        let root = TempRoot::new("sidecar_reexport");
        let src = TempRoot::new("sidecar_reexport_src");
        let roots = vec![root.path().to_string_lossy().into_owned()];

        let first = write_stub_stl(src.path(), "round32.stl");
        export_cuts(
            &[first],
            &root.path().to_string_lossy(),
            "Group",
            &roots,
            (2026, 7),
        )
        .unwrap();

        let sidecar_path = root
            .path()
            .join("Plinth Bases/2026-07 Group/Group — 32 mm Round — 01/model.json");
        // simulate a user hand-edit landing between the two exports
        let id = sidecar_value(&sidecar_path).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        std::fs::write(
            &sidecar_path,
            serde_json::json!({
                "id": id,
                "name": "Round 32 (renamed)",
                "notes": "hand curated",
            })
            .to_string(),
        )
        .unwrap();

        let second_src = TempRoot::new("sidecar_reexport_src2");
        let second = write_stub_stl(second_src.path(), "round32.stl");
        export_cuts(
            &[second],
            &root.path().to_string_lossy(),
            "Group",
            &roots,
            (2026, 7),
        )
        .unwrap();

        let contents = std::fs::read_to_string(&sidecar_path).unwrap();
        assert!(
            contents.contains("renamed"),
            "re-export must not clobber the user's hand-edited sidecar: {contents}"
        );
        assert!(contents.contains("hand curated"));
    }

    #[test]
    fn repairs_legacy_plinth_sidecars_without_moving_files() {
        let root = TempRoot::new("repair_legacy");
        let model_dir = root
            .path()
            .join("Plinth Bases/2026-07 Ruined Chapel/round32-1");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("round32-1.stl"), b"solid legacy").unwrap();
        std::fs::write(
            model_dir.join("model.json"),
            r#"{"name":"round32-1","designer":"Plinth Bases","tags":["painted"]}"#,
        )
        .unwrap();
        let duplicate_dir = root
            .path()
            .join("Plinth Bases/2026-07 Ruined Chapel/round32-2");
        std::fs::create_dir_all(&duplicate_dir).unwrap();
        std::fs::write(duplicate_dir.join("round32-2.stl"), b"solid other").unwrap();
        std::fs::write(
            duplicate_dir.join("model.json"),
            r#"{"name":"round32-1","designer":"Plinth Bases"}"#,
        )
        .unwrap();

        let summary = repair_plinth_base_exports_in_root(root.path()).unwrap();
        assert_eq!(summary.repaired, 2);
        assert!(summary.warnings.is_empty());
        assert!(
            model_dir.join("round32-1.stl").is_file(),
            "repair never moves files"
        );
        let value = sidecar_value(&model_dir.join("model.json")).unwrap();
        assert_eq!(value["name"], "Ruined Chapel — round32-1");
        assert_eq!(value["release_name"], "Ruined Chapel");
        assert_eq!(value["release_date"], "2026-07");
        assert!(Uuid::parse_str(value["id"].as_str().unwrap()).is_ok());
        assert!(value["tags"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("painted")));
        assert!(value["tags"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("generated")));
        let duplicate = sidecar_value(&duplicate_dir.join("model.json")).unwrap();
        assert_eq!(duplicate["name"], "Ruined Chapel — round32-1 — 02");

        let second = repair_plinth_base_exports_in_root(root.path()).unwrap();
        assert_eq!(second.repaired, 0);
        assert_eq!(second.unchanged, 2);
    }

    #[test]
    fn stable_uuid_updates_changed_geometry_without_creating_a_second_model() {
        let root = TempRoot::new("stable_uuid_update");
        let src = TempRoot::new("stable_uuid_update_src");
        let path = src.path().join("blender-name-does-not-matter.stl");
        std::fs::write(&path, b"solid first").unwrap();
        let roots = vec![root.path().to_string_lossy().into_owned()];
        let artifact = CutCatalogArtifact {
            id: Uuid::new_v4().to_string(),
            source_path: path.to_string_lossy().into_owned(),
            cutter: CutterKind::Ellipse {
                major_mm: 60.0,
                minor_mm: 35.0,
            },
            mode: CutCatalogMode::Topper,
        };

        let first = export_cut_artifacts(
            std::slice::from_ref(&artifact),
            &root.path().to_string_lossy(),
            "Marsh Temple",
            &roots,
            (2026, 7),
        )
        .unwrap();
        assert_eq!(first.added, 1);
        std::fs::write(&path, b"solid revised").unwrap();
        let second = export_cut_artifacts(
            &[artifact],
            &root.path().to_string_lossy(),
            "Marsh Temple",
            &roots,
            (2026, 7),
        )
        .unwrap();
        assert_eq!(second.updated, 1);

        let release = root.path().join("Plinth Bases/2026-07 Marsh Temple");
        let model_dirs = std::fs::read_dir(&release)
            .unwrap()
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .collect::<Vec<_>>();
        assert_eq!(model_dirs.len(), 1);
        let model_dir = model_dirs[0].path();
        assert!(model_dir
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("60×35 mm Oval Topper"));
        let stls = std::fs::read_dir(model_dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "stl"))
            .collect::<Vec<_>>();
        assert_eq!(stls.len(), 1);
        assert_eq!(std::fs::read(&stls[0]).unwrap(), b"solid revised");
    }

    #[test]
    fn raw_output_folder_must_not_be_inside_a_catalog_root() {
        let roots = vec!["C:\\Miniatures\\Catalog".to_string()];
        assert!(output_is_inside_catalog(
            "c:\\miniatures\\catalog\\raw-cuts",
            &roots
        ));
        assert!(output_is_inside_catalog("C:\\Miniatures\\Catalog", &roots));
        assert!(!output_is_inside_catalog(
            "C:\\Miniatures\\Catalog Backup",
            &roots
        ));
        assert!(!output_is_inside_catalog("C:\\Temp\\raw-cuts", &roots));
    }
}
