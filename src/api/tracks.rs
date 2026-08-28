use crate::api::error::{ApiError, ApiResult};
use crate::api::SharedState;
use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

pub fn routes() -> Router<SharedState> {
    Router::new()
        .route("/tracks", get(list_tracks))
        .route("/tracks/{id}", get(get_track))
        .route("/albums", get(list_albums))
        .route("/artists", get(list_artists))
}

#[derive(Debug, Serialize, FromRow)]
pub struct Track {
    pub id: i64,
    pub hash: Option<String>,
    pub original_filename: Option<String>,
    pub title: Option<String>,
    pub album: Option<String>,
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub track_no: Option<i64>,
    pub disc_no: Option<i64>,
    pub duration_ms: Option<i64>,
    pub year: Option<i64>,
    pub bitrate: Option<i64>,
    pub sample_rate: Option<i64>,
    pub channels: Option<i64>,
    pub added_at: i64,
}

const TRACK_COLS: &str = "id, hash, original_filename, title, album, artist, \
                          album_artist, track_no, disc_no, duration_ms, year, bitrate, \
                          sample_rate, channels, added_at";

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub album: Option<String>,
    pub artist: Option<String>,
    pub album_artist: Option<String>,
}

async fn list_tracks(
    State(state): State<SharedState>,
    Path(lib_id): Path<i64>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Vec<Track>>> {
    state.require_library(lib_id)?;
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    let offset = q.offset.unwrap_or(0).max(0);

    let mut sql = format!("SELECT {TRACK_COLS} FROM tracks WHERE 1=1");
    let mut binds: Vec<String> = Vec::new();
    if let Some(v) = &q.album {
        sql.push_str(" AND album = ?");
        binds.push(v.clone());
    }
    if let Some(v) = &q.artist {
        sql.push_str(" AND artist = ?");
        binds.push(v.clone());
    }
    if let Some(v) = &q.album_artist {
        sql.push_str(" AND album_artist = ?");
        binds.push(v.clone());
    }
    sql.push_str(" ORDER BY album_artist, album, disc_no, track_no, title LIMIT ? OFFSET ?");

    let mut q = sqlx::query_as::<_, Track>(&sql);
    for b in &binds {
        q = q.bind(b);
    }
    let rows = q.bind(limit).bind(offset).fetch_all(state.pool(lib_id)?).await?;
    Ok(Json(rows))
}

async fn get_track(
    State(state): State<SharedState>,
    Path((lib_id, id)): Path<(i64, i64)>,
) -> ApiResult<Json<Track>> {
    state.require_library(lib_id)?;
    let row = sqlx::query_as::<_, Track>(&format!(
        "SELECT {TRACK_COLS} FROM tracks WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(state.pool(lib_id)?)
    .await?
    .ok_or_else(|| ApiError::not_found("track"))?;
    Ok(Json(row))
}

#[derive(Debug, Serialize, FromRow)]
pub struct AlbumRow {
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub year: Option<i64>,
    pub track_count: i64,
}

async fn list_albums(
    State(state): State<SharedState>,
    Path(lib_id): Path<i64>,
) -> ApiResult<Json<Vec<AlbumRow>>> {
    state.require_library(lib_id)?;
    let rows = sqlx::query_as::<_, AlbumRow>(
        "SELECT album, album_artist, MIN(year) AS year, COUNT(*) AS track_count \
         FROM tracks WHERE album IS NOT NULL \
         GROUP BY album, album_artist ORDER BY album_artist, album",
    )
    .fetch_all(state.pool(lib_id)?)
    .await?;
    Ok(Json(rows))
}

#[derive(Debug, Serialize, FromRow)]
pub struct ArtistRow {
    pub artist: Option<String>,
    pub track_count: i64,
}

async fn list_artists(
    State(state): State<SharedState>,
    Path(lib_id): Path<i64>,
) -> ApiResult<Json<Vec<ArtistRow>>> {
    let _ = state.require_library(lib_id)?;
    let rows = sqlx::query_as::<_, ArtistRow>(
        "SELECT artist, COUNT(*) AS track_count FROM tracks \
         WHERE artist IS NOT NULL GROUP BY artist ORDER BY artist",
    )
    .fetch_all(state.pool(lib_id)?)
    .await?;
    Ok(Json(rows))
}
