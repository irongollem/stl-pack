//! Easter-egg integration with minihoard, Plinth's sibling CLI for
//! fetching a MyMiniFactory library — no binary installed, no sidebar entry.

use crate::error::AppError;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use specta::Type;
use crate::process::new_command;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use tauri::AppHandle;
use tauri_specta::Event;
use uuid::Uuid;

#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct MinihoardInfo {
    pub path: String,
    pub version: String,
    /// Whether this build speaks the `--json` protocol (>= 0.4.0); below
    /// that, only the legacy raw console works.
    pub supports_json: bool,
}

/// The first minihoard version that speaks the `--json` protocol this
/// module parses.
const MIN_JSON_VERSION: (u32, u32) = (0, 4);

/// Parse a `major.minor[.patch]` string far enough to compare against
/// [`MIN_JSON_VERSION`]. Anything unparseable is treated as too old (false),
/// so a garbled `--version` can never accidentally unlock the JSON path.
fn version_supports_json(version: &str) -> bool {
    let mut parts = version.trim().trim_start_matches('v').split('.');
    let major: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (major, minor) >= MIN_JSON_VERSION
}

#[derive(Serialize, Deserialize, Debug, Clone, Type, Event)]
pub enum MinihoardStatus {
    Line(MinihoardLine),
    Finished(MinihoardFinished),
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct MinihoardLine {
    pub job_id: String,
    pub line: String,
    /// stderr lines render dimmer — minihoard uses stderr for progress
    /// chatter, stdout for the actual answers.
    pub is_err: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct MinihoardFinished {
    pub job_id: String,
    pub success: bool,
    /// Present when the process couldn't run at all (spawn failure).
    pub error: Option<String>,
}

/// One run at a time: minihoard owns per-machine state (credentials,
/// download dirs), and the console UI shows a single stream anyway.
static ACTIVE_RUN: Lazy<Mutex<Option<(String, Child)>>> = Lazy::new(|| Mutex::new(None));

/// Subcommands the console may launch: read-only or download-only. Account
/// mutation (login/logout/configure) needs an interactive terminal, so it's
/// excluded; `sync-cookie` is the one credential command allowed through
/// because it never prompts (unlike `set-cookie`, which reads a pasted
/// secret from stdin).
const ALLOWED_SUBCOMMANDS: &[&str] = &[
    "list",
    "download",
    "get",
    "config",
    "where",
    "whoami",
    "sync-cookie",
];

fn binary_candidates() -> Vec<PathBuf> {
    let name = if cfg!(windows) {
        "minihoard.exe"
    } else {
        "minihoard"
    };
    let mut candidates: Vec<PathBuf> = Vec::new();
    // PATH first — wherever the user's shell would find it
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|dir| dir.join(name)));
    }
    // then minihoard's own install destinations, which a GUI app launched
    // from Finder/Explorer often does NOT have on its PATH
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".local/bin").join(name));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(local).join("minihoard/bin").join(name));
    }
    candidates
}

/// Find minihoard and prove it runs (`--version`). None simply means the
/// egg stays hidden.
#[tauri::command]
#[specta::specta]
pub async fn detect_minihoard() -> Option<MinihoardInfo> {
    tauri::async_runtime::spawn_blocking(|| {
        for candidate in binary_candidates() {
            if !candidate.is_file() {
                continue;
            }
            let Ok(output) = new_command(&candidate).arg("--version").output() else {
                continue;
            };
            if !output.status.success() {
                continue;
            }
            // "minihoard 0.3.1" -> "0.3.1"
            let stdout = String::from_utf8_lossy(&output.stdout);
            let version = stdout.split_whitespace().nth(1).unwrap_or("?").to_string();
            let supports_json = version_supports_json(&version);
            return Some(MinihoardInfo {
                path: candidate.to_string_lossy().into_owned(),
                version,
                supports_json,
            });
        }
        None
    })
    .await
    .ok()
    .flatten()
}

/// Launch one whitelisted minihoard run and stream its output as events.
/// Returns the job id immediately; MinihoardStatus::Finished closes it.
#[tauri::command]
#[specta::specta]
pub async fn run_minihoard(
    app_handle: AppHandle,
    binary_path: String,
    args: Vec<String>,
) -> Result<String, AppError> {
    let subcommand = validated_subcommand(&args)?;
    // The binary path round-trips through the webview (detect → run); only
    // accept something detect could actually have produced.
    let binary = PathBuf::from(&binary_path);
    if !binary_candidates().contains(&binary) || !binary.is_file() {
        return Err(AppError::InvalidInput(
            "Unrecognized minihoard binary path".to_string(),
        ));
    }

    let mut active = ACTIVE_RUN
        .lock()
        .map_err(|e| AppError::ConfigError(format!("minihoard registry poisoned: {}", e)))?;
    if let Some((_, child)) = active.as_mut() {
        // reap a finished child instead of refusing forever
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => *active = None,
            Ok(None) => {
                return Err(AppError::InvalidInput(
                    "minihoard is already running — wait or cancel it first".to_string(),
                ))
            }
        }
    }

    let mut command = new_command(&binary);
    command
        .args(&args)
        // batch downloads confirm interactively; there is no terminal here
        .arg_if(subcommand == "download" || subcommand == "get", "-y")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| AppError::IoError(format!("Failed to launch minihoard: {}", e)))?;

    let job_id = Uuid::new_v4().to_string();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    *active = Some((job_id.clone(), child));
    drop(active);

    // One reader thread per pipe: interleaving preserves rough real-time
    // order, and a filled stderr pipe can't deadlock stdout (or vice versa).
    if let Some(stdout) = stdout {
        spawn_line_reader(app_handle.clone(), job_id.clone(), stdout, false);
    }
    if let Some(stderr) = stderr {
        spawn_line_reader(app_handle.clone(), job_id.clone(), stderr, true);
    }

    // Waiter: emits Finished and frees the single-run slot.
    let waiter_app = app_handle.clone();
    let waiter_job = job_id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let status = loop {
            let mut active = match ACTIVE_RUN.lock() {
                Ok(active) => active,
                Err(_) => return,
            };
            match active.as_mut() {
                Some((id, child)) if *id == waiter_job => match child.try_wait() {
                    Ok(Some(status)) => {
                        *active = None;
                        break Ok(status);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        *active = None;
                        break Err(e);
                    }
                },
                // cancelled (slot cleared) or replaced — nothing left to report
                _ => return,
            }
            drop(active);
            std::thread::sleep(std::time::Duration::from_millis(150));
        };
        let (success, error) = match status {
            Ok(status) => (status.success(), None),
            Err(e) => (false, Some(e.to_string())),
        };
        MinihoardStatus::Finished(MinihoardFinished {
            job_id: waiter_job,
            success,
            error,
        })
        .emit(&waiter_app)
        .ok();
    });

    Ok(job_id)
}

/// Kill the active run (the "stop" button). Emits Finished(success=false).
#[tauri::command]
#[specta::specta]
pub async fn cancel_minihoard(app_handle: AppHandle, job_id: String) -> Result<(), AppError> {
    let mut active = ACTIVE_RUN
        .lock()
        .map_err(|e| AppError::ConfigError(format!("minihoard registry poisoned: {}", e)))?;
    match active.take() {
        Some((id, mut child)) if id == job_id => {
            child.kill().ok();
            child.wait().ok(); // reap — no zombie
            MinihoardStatus::Finished(MinihoardFinished {
                job_id,
                success: false,
                error: None,
            })
            .emit(&app_handle)
            .ok();
            Ok(())
        }
        other => {
            *active = other; // put back a different run untouched
            Err(AppError::NotFoundError(format!(
                "No active minihoard run with ID: {}",
                job_id
            )))
        }
    }
}

fn spawn_line_reader<R: std::io::Read + Send + 'static>(
    app_handle: AppHandle,
    job_id: String,
    pipe: R,
    is_err: bool,
) {
    tauri::async_runtime::spawn_blocking(move || {
        for line in BufReader::new(pipe).lines() {
            let Ok(line) = line else { break };
            // lines() only strips \n — Windows children emit \r\n
            let line = line.trim_end_matches('\r').to_string();
            MinihoardStatus::Line(MinihoardLine {
                job_id: job_id.clone(),
                line,
                is_err,
            })
            .emit(&app_handle)
            .ok();
        }
    });
}

/// Tiny ergonomic shim: `Command` has no conditional-arg builder.
trait ArgIf {
    fn arg_if(&mut self, condition: bool, arg: &str) -> &mut Self;
}
impl ArgIf for Command {
    fn arg_if(&mut self, condition: bool, arg: &str) -> &mut Self {
        if condition {
            self.arg(arg);
        }
        self
    }
}

fn validated_subcommand(args: &[String]) -> Result<String, AppError> {
    let subcommand = args.first().cloned().unwrap_or_default();
    if ALLOWED_SUBCOMMANDS.contains(&subcommand.as_str()) {
        Ok(subcommand)
    } else {
        Err(AppError::InvalidInput(format!(
            "'{}' isn't available from Plinth — run it in a terminal",
            subcommand
        )))
    }
}

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Errors from the typed commands, shaped as a `kind` discriminated union
/// so callers branch on the kind, never on message text.
#[derive(Serialize, Deserialize, Debug, Clone, Type)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
pub enum MinihoardError {
    /// No website session cookie is stored — offer `sync-cookie`.
    CookieMissing(String),
    /// The stored cookie no longer authenticates — offer `sync-cookie`.
    CookieExpired(String),
    /// OAuth isn't set up (login lives in a real terminal).
    Auth(String),
    /// minihoard has no usable config yet.
    Config(String),
    /// The binary is missing, too old, or otherwise unusable.
    Unavailable(String),
    /// Anything else minihoard reported.
    Failed(String),
}

/// Install + auth health, mirroring minihoard's `status --json`. Non-mutating;
/// `cookie_present` is presence only — validity is proven by a real `list`.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct MinihoardHealth {
    pub version: String,
    pub oauth_ok: bool,
    pub username: Option<String>,
    pub cookie_present: bool,
    pub library_dir: Option<String>,
}

/// One library object, mirroring minihoard's `list --json` `entry` payload.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct MinihoardEntry {
    pub id: u64,
    pub name: String,
    pub creator: Option<String>,
    pub creator_username: Option<String>,
    pub source: Option<String>,
    pub library_added_at: Option<String>,
    /// Publication date (ISO-8601), when known — the fallback "when is this
    /// from?" date for the ~88% of entries without a tribe release month.
    pub published_at: Option<String>,
    /// Creation date (ISO-8601) — always present; the last-resort date.
    pub created_at: Option<String>,
    pub yearmonth: Option<String>,
    pub tags: Vec<String>,
    pub downloaded: bool,
}

/// Validate a webview-supplied binary path the same way `run_minihoard` does:
/// only something `detect_minihoard` could have produced is accepted.
fn resolve_binary(binary_path: &str) -> Result<PathBuf, MinihoardError> {
    let binary = PathBuf::from(binary_path);
    if !binary_candidates().contains(&binary) || !binary.is_file() {
        return Err(MinihoardError::Unavailable(
            "unrecognized minihoard binary path".to_string(),
        ));
    }
    Ok(binary)
}

/// Map a CLI `{"event":"error","kind":…}` line to a typed [`MinihoardError`].
fn map_error_event(v: &serde_json::Value) -> MinihoardError {
    let msg = v["message"]
        .as_str()
        .unwrap_or("minihoard reported an error")
        .to_string();
    match v["kind"].as_str() {
        Some("cookie_missing") => MinihoardError::CookieMissing(msg),
        Some("cookie_expired") => MinihoardError::CookieExpired(msg),
        Some("auth") => MinihoardError::Auth(msg),
        Some("config") => MinihoardError::Config(msg),
        _ => MinihoardError::Failed(msg),
    }
}

/// Run a minihoard subcommand in `--json` mode to completion and return its
/// non-error NDJSON lines (parsed). A `{"event":"error",…}` line or a non-zero
/// exit becomes a typed `Err`. Blocking — call inside `spawn_blocking`.
fn run_json(binary: &Path, args: &[&str]) -> Result<Vec<serde_json::Value>, MinihoardError> {
    let output = new_command(binary)
        .args(args)
        .arg("--json")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| MinihoardError::Failed(format!("could not run minihoard: {e}")))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut values = Vec::new();
    let mut reported: Option<MinihoardError> = None;
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue; // ignore any stray non-JSON line
        };
        if v["event"] == "error" {
            reported = Some(map_error_event(&v));
        } else {
            values.push(v);
        }
    }

    if !output.status.success() {
        return Err(reported.unwrap_or_else(|| {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let tail = stderr.trim();
            MinihoardError::Failed(if tail.is_empty() {
                "minihoard exited with an error".to_string()
            } else {
                format!("minihoard: {tail}")
            })
        }));
    }
    Ok(values)
}

/// Probe minihoard's install + auth health (`status --json`). Never surfaces
/// auth failure as an error — a dead login or missing cookie is reported in the
/// returned fields, so the UI can render state rather than an exception.
#[tauri::command]
#[specta::specta]
pub async fn minihoard_status(binary_path: String) -> Result<MinihoardHealth, MinihoardError> {
    let binary = resolve_binary(&binary_path)?;
    tauri::async_runtime::spawn_blocking(move || {
        let values = run_json(&binary, &["status"])?;
        let status = values
            .into_iter()
            .find(|v| v["event"] == "status")
            .ok_or_else(|| MinihoardError::Failed("minihoard printed no status".to_string()))?;
        serde_json::from_value::<MinihoardHealth>(status)
            .map_err(|e| MinihoardError::Failed(format!("could not parse status: {e}")))
    })
    .await
    .map_err(|e| MinihoardError::Failed(format!("status task failed: {e}")))?
}

/// Fetch the whole library (`list --json`). Buffered — ~4k entries is fine, and
/// objectPreviews has no pagination, so this is one call, not a poll. A stale or
/// missing cookie comes back as a typed `CookieExpired`/`CookieMissing`.
#[tauri::command]
#[specta::specta]
pub async fn minihoard_list(binary_path: String) -> Result<Vec<MinihoardEntry>, MinihoardError> {
    let binary = resolve_binary(&binary_path)?;
    tauri::async_runtime::spawn_blocking(move || {
        let values = run_json(&binary, &["list"])?;
        values
            .into_iter()
            .filter(|v| v["event"] == "entry")
            .map(|v| {
                serde_json::from_value::<MinihoardEntry>(v)
                    .map_err(|e| MinihoardError::Failed(format!("could not parse entry: {e}")))
            })
            .collect()
    })
    .await
    .map_err(|e| MinihoardError::Failed(format!("list task failed: {e}")))?
}

/// One object's on-demand detail (minihoard `object <id> --json`) — the bits a
/// library row needs when expanded. Fetched lazily, one row at a time.
#[derive(Serialize, Deserialize, Clone, Debug, Type)]
pub struct MinihoardObject {
    pub id: u64,
    pub name: String,
    /// The official MyMiniFactory page (open externally).
    pub url: Option<String>,
    /// A small (~230px) preview image URL.
    pub thumbnail_url: Option<String>,
    /// A larger (~720px) image URL.
    pub image_url: Option<String>,
}

/// Fetch one object's detail (page url + preview image) for a row expand.
/// Needs minihoard >= 0.4.1 (the `object` subcommand); on an older binary this
/// fails, and the caller simply shows the row without a preview.
#[tauri::command]
#[specta::specta]
pub async fn minihoard_object(
    binary_path: String,
    id: u64,
) -> Result<MinihoardObject, MinihoardError> {
    let binary = resolve_binary(&binary_path)?;
    tauri::async_runtime::spawn_blocking(move || {
        let values = run_json(&binary, &["object", &id.to_string()])?;
        let object = values
            .into_iter()
            .find(|v| v["event"] == "object")
            .ok_or_else(|| MinihoardError::Failed("minihoard printed no object".to_string()))?;
        serde_json::from_value::<MinihoardObject>(object)
            .map_err(|e| MinihoardError::Failed(format!("could not parse object: {e}")))
    })
    .await
    .map_err(|e| MinihoardError::Failed(format!("object task failed: {e}")))?
}

/// Streaming status for a typed download run: one `Started`, a run of
/// per-object events, then exactly one terminal (`Finished` | `Failed` |
/// `Cancelled`). User cancel is `Cancelled`, never `Failed`; every variant
/// carries `job_id`.
#[derive(Serialize, Deserialize, Debug, Clone, Type, Event)]
pub enum MinihoardDownloadStatus {
    Started(MdStarted),
    ObjectStart(MdObjectStart),
    FileProgress(MdFileProgress),
    ObjectDone(MdObjectDone),
    ObjectFailed(MdObjectFailed),
    Finished(MdFinished),
    Failed(MdFailed),
    Cancelled(MdCancelled),
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct MdStarted {
    pub job_id: String,
    /// Number of objects requested, available before the first `object_start`.
    pub total: usize,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct MdObjectStart {
    pub job_id: String,
    pub id: u64,
    pub name: String,
    pub index: usize,
    pub total: usize,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct MdFileProgress {
    pub job_id: String,
    pub id: u64,
    pub bytes_done: u64,
    /// Absent when the server didn't send a Content-Length.
    pub bytes_total: Option<u64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct MdObjectDone {
    pub job_id: String,
    pub id: u64,
    /// Final on-disk release folder (post flatten/rename).
    pub dir: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct MdObjectFailed {
    pub job_id: String,
    pub id: u64,
    pub reason: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct MdFinished {
    pub job_id: String,
    pub ok: usize,
    pub failed: usize,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct MdFailed {
    pub job_id: String,
    pub error: MinihoardError,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct MdCancelled {
    pub job_id: String,
}

/// The active typed download. Separate from `ACTIVE_RUN` (the raw console) so a
/// health check or library refresh can't be blocked by a running download, and
/// cancel targets the download specifically. One download at a time.
static ACTIVE_DOWNLOAD: Lazy<Mutex<Option<ActiveDownload>>> = Lazy::new(|| Mutex::new(None));

struct ActiveDownload {
    job_id: String,
    child: Child,
    /// Set once a terminal event (`Finished`/`Failed`) has been emitted by the
    /// stdout reader, so the waiter doesn't emit a second one.
    terminal: Arc<AtomicBool>,
}

/// Start a typed download of the given object ids. Returns a job id immediately;
/// progress arrives as `MinihoardDownloadStatus` events. One run at a time.
#[tauri::command]
#[specta::specta]
pub async fn start_minihoard_download(
    app_handle: AppHandle,
    binary_path: String,
    ids: Vec<u64>,
) -> Result<String, MinihoardError> {
    if ids.is_empty() {
        return Err(MinihoardError::Failed("no objects selected".to_string()));
    }
    let binary = resolve_binary(&binary_path)?;

    let mut active = ACTIVE_DOWNLOAD
        .lock()
        .map_err(|e| MinihoardError::Failed(format!("download registry poisoned: {e}")))?;
    if let Some(run) = active.as_mut() {
        match run.child.try_wait() {
            Ok(Some(_)) | Err(_) => *active = None, // reap a finished run
            Ok(None) => {
                return Err(MinihoardError::Failed(
                    "a download is already running — wait or cancel it first".to_string(),
                ))
            }
        }
    }

    // `get <ids…> --json -y`: -y skips the batch confirm (no terminal here),
    // --json silences the human printer and streams events.
    let mut args: Vec<String> = vec!["get".to_string()];
    args.extend(ids.iter().map(|id| id.to_string()));
    args.push("--json".to_string());
    args.push("-y".to_string());

    let mut child = new_command(&binary)
        .args(&args)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| MinihoardError::Failed(format!("failed to launch minihoard: {e}")))?;

    let job_id = Uuid::new_v4().to_string();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let terminal = Arc::new(AtomicBool::new(false));
    *active = Some(ActiveDownload {
        job_id: job_id.clone(),
        child,
        terminal: terminal.clone(),
    });
    drop(active);

    MinihoardDownloadStatus::Started(MdStarted {
        job_id: job_id.clone(),
        total: ids.len(),
    })
    .emit(&app_handle)
    .ok();

    // stderr is captured only as a post-mortem tail for an unexpected exit.
    let stderr_tail = Arc::new(Mutex::new(Vec::<String>::new()));
    if let Some(stderr) = stderr {
        let tail = stderr_tail.clone();
        tauri::async_runtime::spawn_blocking(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Ok(mut t) = tail.lock() {
                    t.push(line);
                    let overflow = t.len().saturating_sub(50);
                    if overflow > 0 {
                        t.drain(0..overflow); // keep only the last 50 lines
                    }
                }
            }
        });
    }

    if let Some(stdout) = stdout {
        spawn_download_reader(app_handle.clone(), job_id.clone(), terminal.clone(), stdout);
    }

    spawn_download_waiter(app_handle, job_id.clone(), terminal, stderr_tail);
    Ok(job_id)
}

/// Read the child's NDJSON stdout and translate each line into a typed event.
/// `job_done` → `Finished`, `error` → `Failed`; both set `terminal` so the
/// waiter stays quiet.
fn spawn_download_reader<R: std::io::Read + Send + 'static>(
    app_handle: AppHandle,
    job_id: String,
    terminal: Arc<AtomicBool>,
    pipe: R,
) {
    tauri::async_runtime::spawn_blocking(move || {
        for line in BufReader::new(pipe).lines().map_while(Result::ok) {
            let line = line.trim_end_matches('\r');
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let u64_of = |k: &str| v[k].as_u64().unwrap_or(0);
            let usize_of = |k: &str| v[k].as_u64().unwrap_or(0) as usize;
            let event = match v["event"].as_str() {
                Some("object_start") => MinihoardDownloadStatus::ObjectStart(MdObjectStart {
                    job_id: job_id.clone(),
                    id: u64_of("id"),
                    name: v["name"].as_str().unwrap_or_default().to_string(),
                    index: usize_of("index"),
                    total: usize_of("total"),
                }),
                Some("file_progress") => MinihoardDownloadStatus::FileProgress(MdFileProgress {
                    job_id: job_id.clone(),
                    id: u64_of("id"),
                    bytes_done: u64_of("bytes_done"),
                    bytes_total: v["bytes_total"].as_u64(),
                }),
                Some("object_done") => MinihoardDownloadStatus::ObjectDone(MdObjectDone {
                    job_id: job_id.clone(),
                    id: u64_of("id"),
                    dir: v["dir"].as_str().unwrap_or_default().to_string(),
                }),
                Some("object_failed") => MinihoardDownloadStatus::ObjectFailed(MdObjectFailed {
                    job_id: job_id.clone(),
                    id: u64_of("id"),
                    reason: v["reason"].as_str().unwrap_or_default().to_string(),
                }),
                Some("job_done") => {
                    terminal.store(true, Ordering::SeqCst);
                    MinihoardDownloadStatus::Finished(MdFinished {
                        job_id: job_id.clone(),
                        ok: usize_of("ok"),
                        failed: usize_of("failed"),
                    })
                }
                Some("error") => {
                    terminal.store(true, Ordering::SeqCst);
                    MinihoardDownloadStatus::Failed(MdFailed {
                        job_id: job_id.clone(),
                        error: map_error_event(&v),
                    })
                }
                _ => continue,
            };
            event.emit(&app_handle).ok();
        }
    });
}

/// Wait for the child to exit and free the single-run slot. If it ended without
/// a terminal event (crashed, or was killed by something other than our cancel),
/// emit `Failed` with the stderr tail so the queue never hangs.
fn spawn_download_waiter(
    app_handle: AppHandle,
    job_id: String,
    terminal: Arc<AtomicBool>,
    stderr_tail: Arc<Mutex<Vec<String>>>,
) {
    tauri::async_runtime::spawn_blocking(move || {
        loop {
            let mut active = match ACTIVE_DOWNLOAD.lock() {
                Ok(active) => active,
                Err(_) => return,
            };
            match active.as_mut() {
                Some(run) if run.job_id == job_id => match run.child.try_wait() {
                    Ok(Some(_)) | Err(_) => {
                        *active = None;
                        break;
                    }
                    Ok(None) => {}
                },
                // slot cleared (cancelled) or replaced — cancel owns the event
                _ => return,
            }
            drop(active);
            std::thread::sleep(std::time::Duration::from_millis(150));
        }

        // The reader emits the happy/error terminal; only cover the gap where
        // the process died without printing one.
        if !terminal.load(Ordering::SeqCst) {
            let tail = stderr_tail
                .lock()
                .ok()
                .map(|t| t.join("\n"))
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "minihoard exited unexpectedly".to_string());
            MinihoardDownloadStatus::Failed(MdFailed {
                job_id,
                error: MinihoardError::Failed(tail),
            })
            .emit(&app_handle)
            .ok();
        }
    });
}

/// Cancel the active typed download (the queue's stop button). Emits
/// `Cancelled` and marks the run terminal so the waiter stays silent.
#[tauri::command]
#[specta::specta]
pub async fn cancel_minihoard_download(
    app_handle: AppHandle,
    job_id: String,
) -> Result<(), MinihoardError> {
    let mut active = ACTIVE_DOWNLOAD
        .lock()
        .map_err(|e| MinihoardError::Failed(format!("download registry poisoned: {e}")))?;
    match active.take() {
        Some(mut run) if run.job_id == job_id => {
            run.terminal.store(true, Ordering::SeqCst);
            run.child.kill().ok();
            run.child.wait().ok(); // reap — no zombie
            MinihoardDownloadStatus::Cancelled(MdCancelled { job_id })
                .emit(&app_handle)
                .ok();
            Ok(())
        }
        other => {
            *active = other; // a different run — leave it be
            Err(MinihoardError::Failed(format!(
                "no active download with id {job_id}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_safe_subcommands_pass_the_whitelist() {
        for allowed in [
            "list",
            "download",
            "get",
            "config",
            "where",
            "whoami",
            "sync-cookie",
        ] {
            assert!(validated_subcommand(&[allowed.to_string()]).is_ok());
        }
        for refused in ["login", "logout", "configure", "upgrade", "set-cookie", ""] {
            assert!(
                validated_subcommand(&[refused.to_string()]).is_err(),
                "'{}' must be refused",
                refused
            );
        }
        assert!(validated_subcommand(&[]).is_err());
    }

    #[test]
    fn json_support_gates_on_0_4() {
        assert!(!version_supports_json("0.3.9"));
        assert!(version_supports_json("0.4.0"));
        assert!(version_supports_json("v0.4.0")); // tolerate a leading v
        assert!(version_supports_json("0.4.1"));
        assert!(version_supports_json("1.0.0"));
        assert!(!version_supports_json("?")); // garbled --version never unlocks it
        assert!(!version_supports_json(""));
    }

    #[test]
    fn error_events_map_to_typed_kinds() {
        let of = |kind: &str| {
            map_error_event(&serde_json::json!({
                "event": "error", "kind": kind, "message": "m"
            }))
        };
        assert!(matches!(of("cookie_missing"), MinihoardError::CookieMissing(_)));
        assert!(matches!(of("cookie_expired"), MinihoardError::CookieExpired(_)));
        assert!(matches!(of("auth"), MinihoardError::Auth(_)));
        assert!(matches!(of("config"), MinihoardError::Config(_)));
        assert!(matches!(of("something_new"), MinihoardError::Failed(_)));
    }
}
