use crate::api::error::{ApiError, ApiResult};
use crate::api::SharedState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

/// Routes nested under /api/libraries/{lib_id}.
pub fn library_routes() -> Router<SharedState> {
    Router::new()
        .route(
            "/tracks/{id}/tags",
            get(list_track_tags).post(add_user_tag),
        )
        .route("/tracks/{id}/tags/{tag_id}", delete(remove_user_tag))
        .route("/tags", get(list_tags_in_library))
}

#[derive(Debug, Serialize, FromRow)]
pub struct TrackTagRow {
    pub tag_id: i64,
    pub namespace: String,
    pub value: String,
    pub added_at: i64,
}

async fn require_track_in_lib(
    state: &SharedState,
    lib_id: i64,
    track_id: i64,
) -> Result<(), ApiError> {
    state.require_library(lib_id)?;
    let exists: Option<i64> =
        sqlx::query_scalar("SELECT id FROM tracks WHERE id = ?")
            .bind(track_id)
            .fetch_optional(state.pool(lib_id)?)
            .await?;
    if exists.is_none() {
        return Err(ApiError::not_found("track"));
    }
    Ok(())
}

async fn list_track_tags(
    State(state): State<SharedState>,
    Path((lib_id, id)): Path<(i64, i64)>,
) -> ApiResult<Json<Vec<TrackTagRow>>> {
    require_track_in_lib(&state, lib_id, id).await?;
    let rows = sqlx::query_as::<_, TrackTagRow>(
        "SELECT t.id AS tag_id, t.namespace, t.value, tt.added_at \
         FROM track_tags tt JOIN tags t ON t.id = tt.tag_id \
         WHERE tt.track_id = ? \
         ORDER BY t.namespace, t.value",
    )
    .bind(id)
    .fetch_all(state.pool(lib_id)?)
    .await?;
    Ok(Json(rows))
}

#[derive(Debug, Deserialize)]
pub struct NewTag {
    pub namespace: String,
    pub value: String,
}

#[derive(Debug, Serialize)]
pub struct AddedTag {
    pub tag_id: i64,
    pub namespace: String,
    pub value: String,
}

async fn add_user_tag(
    State(state): State<SharedState>,
    Path((lib_id, id)): Path<(i64, i64)>,
    Json(body): Json<NewTag>,
) -> ApiResult<(StatusCode, Json<AddedTag>)> {
    require_track_in_lib(&state, lib_id, id).await?;
    let namespace = body.namespace.trim();
    let value = body.value.trim();
    if value.is_empty() {
        return Err(ApiError::bad_request("value must be non-empty"));
    }

    let now = chrono::Utc::now().timestamp();
    let mut tx = state.pool(lib_id)?.begin().await?;

    let tag_id: i64 = sqlx::query_scalar(
        r#"
        INSERT INTO tags (namespace, value) VALUES (?, ?)
        ON CONFLICT(namespace, value) DO UPDATE SET namespace = namespace
        RETURNING id
        "#,
    )
    .bind(namespace)
    .bind(value)
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT OR IGNORE INTO track_tags (track_id, tag_id, added_at) VALUES (?, ?, ?)",
    )
    .bind(id)
    .bind(tag_id)
    .bind(now)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(AddedTag {
            tag_id,
            namespace: namespace.to_string(),
            value: value.to_string(),
        }),
    ))
}

async fn remove_user_tag(
    State(state): State<SharedState>,
    Path((lib_id, track_id, tag_id)): Path<(i64, i64, i64)>,
) -> ApiResult<StatusCode> {
    require_track_in_lib(&state, lib_id, track_id).await?;
    let res = sqlx::query(
        "DELETE FROM track_tags WHERE track_id = ? AND tag_id = ?",
    )
    .bind(track_id)
    .bind(tag_id)
    .execute(state.pool(lib_id)?)
    .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::not_found("tag on track"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize, FromRow)]
pub struct TagCount {
    pub tag_id: i64,
    pub namespace: String,
    pub value: String,
    pub track_count: i64,
}

async fn list_tags_in_library(
    State(state): State<SharedState>,
    Path(lib_id): Path<i64>,
) -> ApiResult<Json<Vec<TagCount>>> {
    state.require_library(lib_id)?;
    let rows = sqlx::query_as::<_, TagCount>(
        "SELECT t.id AS tag_id, t.namespace, t.value, COUNT(DISTINCT tt.track_id) AS track_count \
         FROM tags t JOIN track_tags tt ON tt.tag_id = t.id \
         JOIN tracks tr ON tr.id = tt.track_id \
         GROUP BY t.id ORDER BY track_count DESC, t.namespace, t.value",
    )
    .fetch_all(state.pool(lib_id)?)
    .await?;
    Ok(Json(rows))
}
