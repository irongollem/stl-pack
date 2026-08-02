use crate::models::BlenderInfo;
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri_specta::Event;

#[derive(Serialize, Deserialize, Debug, Clone, Type, Event)]
pub enum CompressionStatus {
    Started(StartedStatus),
    Progress(ProgressStatus),
    Completed(CompletedStatus),
    Failed(FailedStatus),
    Cancelled(CancelledStatus),
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct StartedStatus {
    pub job_id: String,
    pub total_files: u32,
    pub total_size_kb: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct ProgressStatus {
    pub job_id: String,
    pub processed_files: u32,
    pub total_files: u32,
    pub processed_size_kb: u32,
    pub total_size_kb: u32,
    pub percent_size: u32,
    pub percent_files: u32,
    pub current_file: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct CompletedStatus {
    pub job_id: String,
    pub total_files: u32,
    pub total_size_kb: u32,
    pub elapsed_seconds: f64,
    pub folder_path: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct FailedStatus {
    pub job_id: String,
    pub error: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct CancelledStatus {
    pub job_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type, Event)]
pub enum ScanStatus {
    Started(ScanStartedStatus),
    Progress(ScanProgressStatus),
    Completed(ScanCompletedStatus),
    Failed(ScanFailedStatus),
    Cancelled(ScanCancelledStatus),
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct ScanStartedStatus {
    pub job_id: String,
    pub root: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct ScanProgressStatus {
    pub job_id: String,
    pub files_indexed: u32,
    pub current_dir: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct ScanCompletedStatus {
    pub job_id: String,
    pub total_files: u32,
    pub total_models: u32,
    pub elapsed_seconds: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct ScanFailedStatus {
    pub job_id: String,
    pub error: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct ScanCancelledStatus {
    pub job_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type, Event)]
pub enum DuplicateStatus {
    Started(DuplicateStartedStatus),
    Progress(DuplicateProgressStatus),
    Completed(DuplicateCompletedStatus),
    Failed(DuplicateFailedStatus),
    Cancelled(DuplicateCancelledStatus),
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct DuplicateStartedStatus {
    pub job_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct DuplicateProgressStatus {
    pub job_id: String,
    pub processed: u32,
    pub total: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct DuplicateCompletedStatus {
    pub job_id: String,
    pub group_count: u32,
    pub wasted_bytes: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct DuplicateFailedStatus {
    pub job_id: String,
    pub error: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct DuplicateCancelledStatus {
    pub job_id: String,
}

/// Backend mining stage progress (issue #15) — shaped exactly like
/// DuplicateStatus, since mine_geometry walks the index the same way
/// find_duplicates does (see catalog::geometry's module doc).
#[derive(Serialize, Deserialize, Debug, Clone, Type, Event)]
pub enum GeometryStatus {
    Started(GeometryStartedStatus),
    Progress(GeometryProgressStatus),
    Completed(GeometryCompletedStatus),
    Failed(GeometryFailedStatus),
    Cancelled(GeometryCancelledStatus),
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct GeometryStartedStatus {
    pub job_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct GeometryProgressStatus {
    pub job_id: String,
    pub processed: u32,
    pub total: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct GeometryCompletedStatus {
    pub job_id: String,
    pub mined: u32,
    pub already_known: u32,
    pub failed: u32,
    pub skipped_too_large: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct GeometryFailedStatus {
    pub job_id: String,
    pub error: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct GeometryCancelledStatus {
    pub job_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type, Event)]
pub enum PackStatus {
    Started(PackStartedStatus),
    Progress(PackProgressStatus),
    Completed(PackCompletedStatus),
    Failed(PackFailedStatus),
    Cancelled(PackCancelledStatus),
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct PackStartedStatus {
    pub job_id: String,
    /// "pack" or "unpack" — one event stream serves both directions.
    pub action: String,
    pub total_models: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct PackProgressStatus {
    pub job_id: String,
    /// "compress" | "verify" | "extract" — what the current model is doing.
    pub phase: String,
    pub current_model: String,
    /// 1-based position of the current model in the batch.
    pub model_index: u32,
    pub total_models: u32,
    pub processed_size_kb: u32,
    pub total_size_kb: u32,
    pub percent: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct PackCompletedStatus {
    pub job_id: String,
    pub action: String,
    pub succeeded: u32,
    pub total_models: u32,
    /// Loose files left in place because they changed since compression —
    /// surfaced so nothing silently stays behind.
    pub kept_files: Vec<String>,
    pub elapsed_seconds: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct PackFailedStatus {
    pub job_id: String,
    pub error: String,
    /// Models finished before the failure — their state change is real and
    /// re-running the batch resumes after them.
    pub succeeded: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct PackCancelledStatus {
    pub job_id: String,
    pub succeeded: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type, Event)]
pub enum BlenderProvisionStatus {
    Started(ProvisionStartedStatus),
    Progress(ProvisionProgressStatus),
    /// Post-download work; Progress stops once the bytes are all here.
    Extracting(ProvisionExtractingStatus),
    Completed(ProvisionCompletedStatus),
    Failed(ProvisionFailedStatus),
    Cancelled(ProvisionCancelledStatus),
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct ProvisionStartedStatus {
    pub job_id: String,
    /// The pinned Blender being installed, e.g. "5.1.2".
    pub version: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct ProvisionProgressStatus {
    pub job_id: String,
    /// f64 like wasted_bytes above — u32 caps at 4 GB.
    pub downloaded_bytes: f64,
    pub total_bytes: f64,
    pub percent: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct ProvisionExtractingStatus {
    pub job_id: String,
    /// "verify" | "extract" | "install" — mirrors PackProgressStatus.phase.
    pub phase: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct ProvisionCompletedStatus {
    pub job_id: String,
    pub info: BlenderInfo,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct ProvisionFailedStatus {
    pub job_id: String,
    pub error: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct ProvisionCancelledStatus {
    pub job_id: String,
}

/// Batch preview rendering: many models, ONE Blender launch. Deliberately
/// its own stream — RenderStatus is the studio's single-job protocol and
/// batch events flowing through it would hijack that UI.
#[derive(Serialize, Deserialize, Debug, Clone, Type, Event)]
pub enum BatchRenderStatus {
    Started(BatchRenderStartedStatus),
    Progress(BatchRenderProgressStatus),
    /// One model finished (ok or not) — previews land incrementally, so the
    /// catalog can refresh without waiting for the whole sweep.
    ModelFinished(BatchRenderModelStatus),
    Completed(BatchRenderCompletedStatus),
    Failed(BatchRenderFailedStatus),
    Cancelled(BatchRenderCancelledStatus),
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct BatchRenderStartedStatus {
    pub job_id: String,
    pub total_models: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct BatchRenderProgressStatus {
    pub job_id: String,
    /// Display name of the model currently rendering.
    pub current_model: String,
    /// 1-based position in the batch.
    pub model_index: u32,
    pub total_models: u32,
    /// Cycles sample progress of the current model.
    pub model_percent: u32,
    /// Whole-batch percent: (finished*100 + model_percent) / total.
    pub percent: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct BatchRenderModelStatus {
    pub job_id: String,
    pub model_index: u32,
    pub dir_path: String,
    pub variant_key: Option<String>,
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct BatchRenderCompletedStatus {
    pub job_id: String,
    pub succeeded: u32,
    pub failed: u32,
    pub total_models: u32,
    pub elapsed_seconds: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct BatchRenderFailedStatus {
    pub job_id: String,
    pub error: String,
    /// Models finished before the failure — their previews are already
    /// persisted and real.
    pub succeeded: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct BatchRenderCancelledStatus {
    pub job_id: String,
    pub succeeded: u32,
}

/// Base Cutter job progress — see docs/BASECUTTER.md "Pinned interfaces".
/// Shaped like BatchRenderStatus (started / per-step progress / finished /
/// failed / cancelled), but the steps mirror base_cut.py's own token
/// protocol (VALIDATING / VALIDATED / CUT_START / CUT_DONE / CUT_FAILED /
/// JOB_DONE) rather than render's sample-progress model. Cancellation gets
/// its own variant (mirroring BatchRenderCancelledStatus) rather than
/// flowing through Failed: commands.rs's run_base_cut_job matches
/// AppError::UserCancelled specifically so the frontend can tell "the user
/// stopped this" apart from "this actually broke".
#[derive(Serialize, Deserialize, Debug, Clone, Type, Event)]
pub enum BaseCutStatus {
    Started(BaseCutStartedStatus),
    Validating(BaseCutValidatingStatus),
    Validated(BaseCutValidatedStatus),
    CutStarted(BaseCutCutStartedStatus),
    CutDone(BaseCutCutDoneStatus),
    CutFailed(BaseCutCutFailedStatus),
    Finished(BaseCutFinishedStatus),
    Failed(BaseCutFailedStatus),
    Cancelled(BaseCutCancelledStatus),
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct BaseCutStartedStatus {
    pub job_id: String,
    pub total: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct BaseCutValidatingStatus {
    pub job_id: String,
}

/// Mirrors base_cut.py's `validate()` report dict exactly (see its
/// docstring): non-manifold edge count, bounding box, vertex count, and an
/// optional warning string added only when the landscape failed the check
/// but the script kept going anyway (the "Spike policy" in base_cut.py's
/// main(): report loudly, keep cutting, let the app-side gate harden later).
#[derive(Serialize, Deserialize, Debug, Clone, Default, Type)]
pub struct BaseCutValidationReport {
    #[serde(default)]
    pub non_manifold_edges: u32,
    #[serde(default)]
    pub dims_mm: [f64; 3],
    #[serde(default)]
    pub verts: u32,
    #[serde(default)]
    pub warning: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct BaseCutValidatedStatus {
    pub job_id: String,
    pub report: BaseCutValidationReport,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct BaseCutCutStartedStatus {
    pub job_id: String,
    pub index: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct BaseCutCutDoneStatus {
    pub job_id: String,
    pub index: u32,
    pub out_path: String,
    pub dims_mm: [f64; 3],
    pub manifold: bool,
    /// The union tripwire (normal seat-on-plinth mode only, see
    /// docs/BASECUTTER.md "The cut pipeline"): `Some(false)` when the
    /// plug/plinth union left more than one loose shell behind — the exact
    /// silent failure a base-cut accident revealed (CUT_DONE reported
    /// success, the STL held two loose shells). The cut still counts as
    /// success — the mesh may still be printable — this only makes the
    /// silent case visible. `None` in topper mode (nothing to fuse) or when
    /// the union fused into one shell.
    pub fused: Option<bool>,
    /// Loose-shell count backing `fused`, present alongside it.
    pub shells: Option<u32>,
    /// Present only when the job's requested `topper_mm` fell outside
    /// base_cut.py's [1.0, 3.0] clamp range — the effective value the
    /// script used instead.
    pub topper_mm_clamped: Option<f64>,
    /// `Some(true)` = this placement carried a magnet spec that topper mode
    /// ignored (there's no plinth to pocket it into).
    pub magnet_ignored: Option<bool>,
    /// Slice-mode scatter shells omitted because they could not be clipped
    /// into a closed, rim-bounded solid.
    pub scatter_skipped: Option<u32>,
    /// VTT GLB export design doc "Base cut": the cut's `.glb` twin path,
    /// glb-mode jobs only (`BaseCutJob.glb == true`) — `None` in the
    /// default (non-glb) mode.
    pub glb_path: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct BaseCutCutFailedStatus {
    pub job_id: String,
    pub index: u32,
    pub reason: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct BaseCutFinishedStatus {
    pub job_id: String,
    pub ok_count: u32,
    pub total: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct BaseCutFailedStatus {
    pub job_id: String,
    pub message: String,
    /// Last ~10 lines of Blender stdout — a post-mortem when the failure
    /// wasn't a clean CUT_FAILED/VALIDATION_FAILED token (e.g. a crash).
    pub stdout_tail: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct BaseCutCancelledStatus {
    pub job_id: String,
}

/// Landscape generator job progress — see docs/BASECUTTER.md "The landscape
/// generator (phase 6)". Deliberately its own stream, not folded into
/// BaseCutStatus: generation and cutting are different activities (one
/// bakes a heightfield, one cuts plugs from an existing one) that merely
/// share the one Blender process slot, so they get separate single-job
/// guards AND separate event families, same reasoning as BatchRenderStatus
/// getting its own stream instead of piggybacking on RenderStatus.
#[derive(Serialize, Deserialize, Debug, Clone, Type, Event)]
pub enum LandscapeGenStatus {
    Started(LandscapeGenStartedStatus),
    Finished(LandscapeGenFinishedStatus),
    Failed(LandscapeGenFailedStatus),
    Cancelled(LandscapeGenCancelledStatus),
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct LandscapeGenStartedStatus {
    pub job_id: String,
    pub seed: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct LandscapeGenFinishedStatus {
    pub job_id: String,
    pub out_path: String,
    /// The GLB twin's path (VTT GLB export design doc convention 4) — see
    /// gen_landscape.py's GENERATED token and LandscapeToken::Generated.
    pub glb_path: Option<String>,
    pub dims_mm: [f64; 3],
    pub manifold: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct LandscapeGenFailedStatus {
    pub job_id: String,
    pub message: String,
    /// Last ~10 lines of Blender stdout — a post-mortem when the failure
    /// wasn't a clean GENERATION_FAILED token (e.g. a crash before the
    /// script's own try/except, or an exit with no GENERATED at all).
    pub stdout_tail: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct LandscapeGenCancelledStatus {
    pub job_id: String,
}

/// Scatter job progress — see docs/SCATTER.md "Pinned interfaces" and
/// scatter_landscape.py's own docstring for the stdout token protocol this
/// mirrors (SCATTER_START / SCATTER_PROGRESS / SCATTER_DONE /
/// SCATTER_FAILED). Deliberately its own stream, not folded into
/// BaseCutStatus or LandscapeGenStatus: scatter is a third distinct activity
/// (docs/SCATTER.md "The architectural call: scatter is a LANDSCAPE
/// TRANSFORMER") that happens to share the one Blender process slot, same
/// reasoning as LandscapeGenStatus getting its own stream instead of
/// piggybacking on BaseCutStatus. Cancellation gets its own variant rather
/// than flowing through Failed — a user-initiated stop is Cancelled, never
/// Failed, matching BaseCutStatus/LandscapeGenStatus's convention.
#[derive(Serialize, Deserialize, Debug, Clone, Type, Event)]
pub enum ScatterStatus {
    Started(ScatterStartedStatus),
    Progress(ScatterProgressStatus),
    Finished(ScatterFinishedStatus),
    Failed(ScatterFailedStatus),
    Cancelled(ScatterCancelledStatus),
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct ScatterStartedStatus {
    pub job_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct ScatterProgressStatus {
    pub job_id: String,
    pub placed: u32,
    pub total: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct ScatterFinishedStatus {
    pub job_id: String,
    pub out_path: String,
    pub placed: u32,
    pub manifold: bool,
    /// The scattered output's GLB twin path (VTT GLB export design doc
    /// convention 4) — see scatter_landscape.py's SCATTER_DONE token and
    /// `basecutter::scatter::ScatterToken::Done`.
    pub glb_path: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct ScatterFailedStatus {
    pub job_id: String,
    pub message: String,
    /// Last ~10 lines of Blender stdout — a post-mortem when the failure
    /// wasn't a clean SCATTER_FAILED token (e.g. a crash before the
    /// script's own try/except, or an exit with no SCATTER_DONE at all).
    pub stdout_tail: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct ScatterCancelledStatus {
    pub job_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type, Event)]
pub enum RenderStatus {
    Started(RenderStartedStatus),
    Progress(RenderProgressStatus),
    Completed(RenderCompletedStatus),
    Failed(RenderFailedStatus),
    Cancelled(RenderCancelledStatus),
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct RenderStartedStatus {
    pub job_id: String,
    pub output_path: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct RenderProgressStatus {
    pub job_id: String,
    pub current_sample: u32,
    pub total_samples: u32,
    pub percent: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct RenderCompletedStatus {
    pub job_id: String,
    pub output_path: String,
    pub elapsed_seconds: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct RenderFailedStatus {
    pub job_id: String,
    pub error: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct RenderCancelledStatus {
    pub job_id: String,
}
