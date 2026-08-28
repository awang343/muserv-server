use crate::api::error::{ApiError, ApiResult};
use crate::api::SharedState;
use crate::ingest::{self, CopyMode, ImportStatsView};
use crate::libraries::Library;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path as StdPath, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use uuid::Uuid;

/// In-memory job registry, shared across the app via `AppState`. Jobs are
/// intentionally not persisted: they exist only to let clients poll progress
/// of a running downloader script, and are lost on server restart like any
/// other transient request state.
pub type JobStore = Arc<Mutex<HashMap<String, Job>>>;

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Clone, Serialize)]
pub struct Job {
    pub id: String,
    pub script: String,
    pub urls: Vec<String>,
    /// Index into `urls` of the one currently downloading; `None` once every
    /// url has finished downloading (including while the post-download
    /// import is running, and after the job has finished).
    pub current_index: Option<usize>,
    pub status: JobStatus,
    pub log: Vec<String>,
    /// Set once the post-download import has run.
    pub summary: Option<ImportStatsView>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

#[derive(Serialize)]
pub struct DownloaderInfo {
    pub name: String,
}

/// Routes nested under /api/libraries/{lib_id}.
pub fn routes() -> Router<SharedState> {
    Router::new()
        .route("/downloaders", get(list))
        .route("/downloaders/{name}/run", post(run))
        .route("/downloaders/jobs/{id}", get(job_status))
}

/// Lists executable files directly inside `downloaders_path`, sorted by name.
/// Returns an empty list (rather than an error) when no `downloaders_path` is
/// configured, since the feature is optional.
async fn list(
    State(state): State<SharedState>,
    Path(lib_id): Path<i64>,
) -> ApiResult<Json<Vec<DownloaderInfo>>> {
    state.require_library(lib_id)?;

    let Some(downloaders_path) = state.downloaders_path.clone() else {
        return Ok(Json(Vec::new()));
    };

    let names = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&downloaders_path)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if entry.metadata()?.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                names.push(name.to_string());
            }
        }
        names.sort();
        Ok(names)
    })
    .await
    .map_err(|e| ApiError::bad_request(format!("listing downloaders: {e}")))??;

    Ok(Json(
        names.into_iter().map(|name| DownloaderInfo { name }).collect(),
    ))
}

/// Resolves a script `name` to an absolute path inside the configured
/// `downloaders_path`, rejecting anything that isn't a direct child of it (no
/// path separators, no `.`/`..`) so a script name can never be used to escape
/// the allowlisted directory.
fn resolve_script(state: &SharedState, name: &str) -> ApiResult<PathBuf> {
    let downloaders_path = state
        .downloaders_path
        .as_ref()
        .ok_or_else(|| ApiError::bad_request("no downloaders_path configured"))?;

    if name.is_empty() || name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        return Err(ApiError::bad_request("invalid script name"));
    }

    let candidate = downloaders_path.join(name);
    let canonical = candidate
        .canonicalize()
        .map_err(|_| ApiError::not_found("script"))?;

    if canonical.parent() != Some(downloaders_path.as_path()) || !canonical.is_file() {
        return Err(ApiError::bad_request("invalid script name"));
    }

    Ok(canonical)
}

#[derive(Deserialize)]
struct RunBody {
    urls: Vec<String>,
}

#[derive(Serialize)]
struct RunResponse {
    job_id: String,
}

/// Kicks off a downloader script as a detached background job, once per url
/// in sequence (never in parallel — each run gets its own download directory
/// and must finish before the next starts), and returns immediately with the
/// job's id; clients poll `job_status` for progress. The script is invoked
/// via argv (never through a shell), with the url as its only argument and a
/// fresh staging directory to write audio files into. Once every url has
/// been downloaded, the staging directory is imported into the library's
/// content-addressed storage.
async fn run(
    State(state): State<SharedState>,
    Path((lib_id, name)): Path<(i64, String)>,
    Json(body): Json<RunBody>,
) -> ApiResult<Json<RunResponse>> {
    let lib = state.require_library(lib_id)?;
    let script = resolve_script(&state, &name)?;
    let urls: Vec<String> = body
        .urls
        .iter()
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .collect();
    if urls.is_empty() {
        return Err(ApiError::bad_request("at least one url is required"));
    }

    let job_id = Uuid::new_v4().to_string();
    let job = Job {
        id: job_id.clone(),
        script: name,
        urls: urls.clone(),
        current_index: Some(0),
        status: JobStatus::Running,
        log: Vec::new(),
        summary: None,
        started_at: chrono::Utc::now().to_rfc3339(),
        finished_at: None,
    };
    state
        .downloader_jobs
        .lock()
        .expect("downloader_jobs poisoned")
        .insert(job_id.clone(), job);

    let task_state = state.clone();
    let task_job_id = job_id.clone();
    tokio::spawn(run_job(task_state, task_job_id, lib, script, urls));

    Ok(Json(RunResponse { job_id }))
}

async fn job_status(
    State(state): State<SharedState>,
    Path((lib_id, id)): Path<(i64, String)>,
) -> ApiResult<Json<Job>> {
    state.require_library(lib_id)?;
    let jobs = state
        .downloader_jobs
        .lock()
        .expect("downloader_jobs poisoned");
    match jobs.get(&id) {
        Some(job) => Ok(Json(job.clone())),
        None => Err(ApiError::not_found("job")),
    }
}

fn append_log(state: &SharedState, job_id: &str, line: String) {
    if let Some(job) = state
        .downloader_jobs
        .lock()
        .expect("downloader_jobs poisoned")
        .get_mut(job_id)
    {
        job.log.push(line);
    }
}

fn set_current_index(state: &SharedState, job_id: &str, index: Option<usize>) {
    if let Some(job) = state
        .downloader_jobs
        .lock()
        .expect("downloader_jobs poisoned")
        .get_mut(job_id)
    {
        job.current_index = index;
    }
}

fn finish_job(state: &SharedState, job_id: &str, status: JobStatus, summary: Option<ImportStatsView>) {
    if let Some(job) = state
        .downloader_jobs
        .lock()
        .expect("downloader_jobs poisoned")
        .get_mut(job_id)
    {
        job.status = status;
        job.summary = summary;
        job.finished_at = Some(chrono::Utc::now().to_rfc3339());
    }
}

/// Runs the downloader script once per url, strictly in sequence, writing
/// into a throwaway staging directory under the OS temp dir (not the
/// library itself — the script's output isn't part of the library until
/// it's been ingested). Once every url has been downloaded (or failed), the
/// staging directory is imported into the library's content-addressed
/// storage (moving each file into `.storage`, deduping by hash) and then
/// removed.
async fn run_job(state: SharedState, job_id: String, lib: Library, script: PathBuf, urls: Vec<String>) {
    let total = urls.len();
    let staging_root = std::env::temp_dir().join("muserv-downloads").join(&job_id);
    let mut any_failed = false;

    for (index, url) in urls.iter().enumerate() {
        set_current_index(&state, &job_id, Some(index));
        append_log(&state, &job_id, format!("=== [{}/{total}] {url} ===", index + 1));

        let dest_dir = staging_root.join(index.to_string());
        if let Err(e) = tokio::fs::create_dir_all(&dest_dir).await {
            append_log(&state, &job_id, format!("failed to create download dir: {e}"));
            any_failed = true;
            continue;
        }

        let ok = run_one(&state, &job_id, &script, url, &dest_dir).await;
        any_failed |= !ok;
    }

    set_current_index(&state, &job_id, None);
    append_log(&state, &job_id, "=== importing downloaded files ===".to_string());

    let pool = state.pools.get(&lib.id).expect("library removed while job running").clone();
    let (summary, import_failed) = match ingest::import_dir(&pool, &lib, &staging_root, CopyMode::Move).await {
        Ok(stats) => {
            append_log(
                &state,
                &job_id,
                format!(
                    "import: scanned={} imported={} duplicates={} failed={}",
                    stats.scanned, stats.imported, stats.duplicates, stats.failed,
                ),
            );
            (Some(ImportStatsView::from(&stats)), false)
        }
        Err(e) => {
            append_log(&state, &job_id, format!("import failed: {e:#}"));
            (None, true)
        }
    };

    let _ = tokio::fs::remove_dir_all(&staging_root).await;

    finish_job(
        &state,
        &job_id,
        if any_failed || import_failed {
            JobStatus::Failed
        } else {
            JobStatus::Completed
        },
        summary,
    );
}

/// Runs the downloader script for a single url to completion, streaming its
/// stdout/stderr into the job log. Returns whether the script started and
/// exited successfully.
async fn run_one(
    state: &SharedState,
    job_id: &str,
    script: &StdPath,
    url: &str,
    dest_dir: &StdPath,
) -> bool {
    let mut cmd = Command::new(script);
    cmd.arg(url)
        .env("MUSERV_DOWNLOAD_DIR", dest_dir)
        .current_dir(dest_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            append_log(state, job_id, format!("failed to start script: {e}"));
            return false;
        }
    };

    let stdout = child.stdout.take().expect("child spawned with piped stdout");
    let stderr = child.stderr.take().expect("child spawned with piped stderr");

    let stdout_state = state.clone();
    let stdout_job_id = job_id.to_string();
    let stdout_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            append_log(&stdout_state, &stdout_job_id, line);
        }
    });

    let stderr_state = state.clone();
    let stderr_job_id = job_id.to_string();
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            append_log(&stderr_state, &stderr_job_id, format!("[stderr] {line}"));
        }
    });

    let _ = stdout_task.await;
    let _ = stderr_task.await;

    match child.wait().await {
        Ok(status) if status.success() => true,
        Ok(status) => {
            append_log(state, job_id, format!("script exited with {status}"));
            false
        }
        Err(e) => {
            append_log(state, job_id, format!("failed to wait on script: {e}"));
            false
        }
    }
}
