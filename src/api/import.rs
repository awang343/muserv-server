use crate::api::error::{ApiError, ApiResult};
use crate::api::SharedState;
use crate::ingest::{self, CopyMode};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

#[derive(Debug, Default, Clone, Serialize)]
pub struct ImportState {
    pub running: bool,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub last_stats: Option<ImportStatsView>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportStatsView {
    pub scanned: u64,
    pub imported: u64,
    pub duplicates: u64,
    pub failed: u64,
}

impl From<&ingest::ImportStats> for ImportStatsView {
    fn from(s: &ingest::ImportStats) -> Self {
        Self {
            scanned: s.scanned,
            imported: s.imported,
            duplicates: s.duplicates,
            failed: s.failed,
        }
    }
}

pub fn routes() -> Router<SharedState> {
    Router::new().route("/import", get(get_status).post(trigger))
}

async fn get_status(
    State(state): State<SharedState>,
    Path(lib_id): Path<i64>,
) -> ApiResult<Json<ImportState>> {
    state.require_library(lib_id)?;
    let snap = state
        .import_states
        .lock()
        .expect("import_states poisoned")
        .get(&lib_id)
        .cloned()
        .unwrap_or_default();
    Ok(Json(snap))
}

/// Self-migrates the library: converts any legacy rows still pointing at
/// their original on-disk path into content-addressed storage, then picks up
/// any audio files sitting loose in the library root that were never tracked
/// at all — moving both into `.storage` so nothing is left duplicated
/// on disk outside of it.
async fn trigger(
    State(state): State<SharedState>,
    Path(lib_id): Path<i64>,
) -> ApiResult<(StatusCode, Json<ImportState>)> {
    let lib = state.require_library(lib_id)?;
    {
        let mut map = state.import_states.lock().expect("import_states poisoned");
        let s = map.entry(lib_id).or_default();
        if s.running {
            return Err(ApiError {
                status: StatusCode::CONFLICT,
                message: "import already running".into(),
            });
        }
        s.running = true;
        s.started_at = Some(chrono::Utc::now().timestamp());
        s.finished_at = None;
        s.last_error = None;
    }

    let bg = state.clone();
    tokio::spawn(async move {
        let result = async {
            let legacy = ingest::migrate_legacy_rows(&bg.pool, &lib, CopyMode::Move).await?;
            let loose = ingest::import_dir(&bg.pool, &lib, &lib.root(), CopyMode::Move).await?;
            anyhow::Ok(legacy.merge(loose))
        }
        .await;

        let mut map = bg.import_states.lock().expect("import_states poisoned");
        let s = map.entry(lib_id).or_default();
        s.running = false;
        s.finished_at = Some(chrono::Utc::now().timestamp());
        match result {
            Ok(stats) => {
                s.last_stats = Some(ImportStatsView::from(&stats));
            }
            Err(e) => {
                s.last_error = Some(format!("{e:#}"));
            }
        }
    });

    let snap = state
        .import_states
        .lock()
        .expect("import_states poisoned")
        .get(&lib_id)
        .cloned()
        .unwrap_or_default();
    Ok((StatusCode::ACCEPTED, Json(snap)))
}
