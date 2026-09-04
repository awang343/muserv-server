use crate::api::error::{ApiError, ApiResult};
use crate::api::SharedState;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio_util::io::ReaderStream;

pub fn routes() -> Router<SharedState> {
    Router::new().route("/tracks/{id}/stream", get(stream_track))
}

async fn stream_track(
    State(state): State<SharedState>,
    Path((lib_id, id)): Path<(i64, i64)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let lib = state.require_library(lib_id)?;
    let storage_path: Option<String> =
        sqlx::query_scalar("SELECT storage_path FROM tracks WHERE id = ?")
            .bind(id)
            .fetch_optional(state.pool(lib_id)?)
            .await?
            .ok_or_else(|| ApiError::not_found("track"))?;
    let storage_path = storage_path.ok_or_else(|| {
        ApiError::bad_request(
            "track not yet imported into content-addressed storage; run `muserv import`",
        )
    })?;
    let path = lib.root().join(&storage_path);

    let mut file = File::open(&path).await?;
    let total_size = file.metadata().await?.len();
    let mime = mime_guess::from_path(&path)
        .first_or_octet_stream()
        .to_string();

    let range_header = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_owned());

    let (status, start, end) = match range_header.as_deref().and_then(parse_range) {
        Some((s, e)) => {
            let end = e.unwrap_or(total_size.saturating_sub(1));
            if s >= total_size || end >= total_size || s > end {
                return Err(ApiError {
                    status: StatusCode::RANGE_NOT_SATISFIABLE,
                    message: "invalid range".into(),
                });
            }
            (StatusCode::PARTIAL_CONTENT, s, end)
        }
        None => (StatusCode::OK, 0, total_size.saturating_sub(1)),
    };

    let length = end - start + 1;
    if start > 0 {
        file.seek(SeekFrom::Start(start)).await?;
    }
    let limited = file.take(length);
    let stream = ReaderStream::new(limited);
    let body = Body::from_stream(stream);

    let mut resp = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, mime)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, length.to_string());

    if status == StatusCode::PARTIAL_CONTENT {
        let cr = format!("bytes {}-{}/{}", start, end, total_size);
        resp = resp.header(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&cr).expect("ascii"),
        );
    }

    Ok(resp.body(body).expect("valid response").into_response())
}

/// Parse a single `bytes=start-end` range. Returns `(start, Option<end>)`.
/// We only support the first range; multi-range is rare for audio playback.
fn parse_range(s: &str) -> Option<(u64, Option<u64>)> {
    let rest = s.strip_prefix("bytes=")?;
    let first = rest.split(',').next()?.trim();
    let (start, end) = first.split_once('-')?;
    let start: u64 = start.trim().parse().ok()?;
    let end = end.trim();
    let end = if end.is_empty() {
        None
    } else {
        Some(end.parse().ok()?)
    };
    Some((start, end))
}
