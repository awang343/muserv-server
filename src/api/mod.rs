use crate::api::error::{ApiError, ApiResult};
use crate::libraries::Library;
use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

pub mod auth;
pub mod downloaders;
pub mod error;
pub mod playlists;
pub mod search;
pub mod stream;
pub mod tags;
pub mod tracks;

#[derive(Clone)]
pub struct AppState {
    pub pools: HashMap<i64, SqlitePool>,
    pub auth_token: Option<String>,
    pub libraries: Vec<Library>,
    pub downloaders_path: Option<PathBuf>,
    pub downloader_jobs: downloaders::JobStore,
}

pub type SharedState = Arc<AppState>;

impl AppState {
    /// Resolve a library id from the URL and return a clone of the Library row.
    /// Returns a 404 if the id is not configured.
    pub fn require_library(&self, id: i64) -> Result<Library, ApiError> {
        self.libraries
            .iter()
            .find(|l| l.id == id)
            .cloned()
            .ok_or_else(|| ApiError::not_found("library"))
    }

    /// Resolve a library id from the URL to that library's own db pool.
    /// Returns a 404 if the id is not configured.
    pub fn pool(&self, id: i64) -> Result<&SqlitePool, ApiError> {
        self.pools
            .get(&id)
            .ok_or_else(|| ApiError::not_found("library"))
    }
}

pub fn router(
    libraries: Vec<(Library, SqlitePool)>,
    auth_token: Option<String>,
    downloaders_path: Option<PathBuf>,
) -> Router {
    let pools = libraries.iter().map(|(l, p)| (l.id, p.clone())).collect();
    let libraries = libraries.into_iter().map(|(l, _)| l).collect();
    let state = Arc::new(AppState {
        pools,
        auth_token,
        libraries,
        downloaders_path,
        downloader_jobs: Arc::new(Mutex::new(HashMap::new())),
    });

    let library_scoped = Router::new()
        .merge(tracks::routes())
        .merge(tags::library_routes())
        .merge(search::routes())
        .merge(playlists::routes())
        .merge(downloaders::routes())
        .merge(stream::routes());

    let protected = Router::new()
        .route("/api/libraries", get(list_libraries))
        .route("/api/libraries/{lib_id}", get(get_library))
        .nest("/api/libraries/{lib_id}", library_scoped)
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_bearer,
        ));

    let open = Router::new().route("/health", get(|| async { Json(json!({ "ok": true })) }));

    Router::new()
        .merge(open)
        .merge(protected)
        .with_state(state)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
}

async fn list_libraries(State(state): State<SharedState>) -> Json<Vec<Library>> {
    Json(state.libraries.clone())
}

async fn get_library(
    State(state): State<SharedState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Library>> {
    Ok(Json(state.require_library(id)?))
}
