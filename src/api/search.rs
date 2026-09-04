use crate::api::error::{ApiError, ApiResult};
use crate::api::tracks::Track;
use crate::api::SharedState;
use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

pub fn routes() -> Router<SharedState> {
    Router::new().route("/search", get(search))
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: String,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// Parsed token: a namespace:value pair, optionally negated.
struct Token<'a> {
    negated: bool,
    namespace: &'a str,
    value: &'a str,
}

fn parse_query(q: &str) -> Result<Vec<Token<'_>>, ApiError> {
    let mut out = Vec::new();
    for raw in q.split_whitespace() {
        let (negated, rest) = match raw.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, raw),
        };
        let (ns, val) = rest
            .split_once(':')
            .ok_or_else(|| ApiError::bad_request(format!("token '{raw}' missing ':'")))?;
        if val.is_empty() {
            return Err(ApiError::bad_request(format!(
                "token '{raw}' has empty value"
            )));
        }
        out.push(Token {
            negated,
            namespace: ns,
            value: val,
        });
    }
    Ok(out)
}

async fn search(
    State(state): State<SharedState>,
    Path(lib_id): Path<i64>,
    Query(q): Query<SearchQuery>,
) -> ApiResult<Json<Vec<Track>>> {
    state.require_library(lib_id)?;
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    let offset = q.offset.unwrap_or(0).max(0);

    let tokens = parse_query(&q.q)?;
    if tokens.is_empty() {
        return Err(ApiError::bad_request("query is empty"));
    }

    let mut sql = String::from(
        "SELECT id, hash, original_filename, title, album, artist, album_artist, \
         track_no, disc_no, duration_ms, year, bitrate, sample_rate, channels, added_at \
         FROM tracks t WHERE 1=1",
    );
    let mut binds: Vec<String> = Vec::new();

    for tok in &tokens {
        let exists_clause = "EXISTS (SELECT 1 FROM track_tags tt \
             JOIN tags tg ON tg.id = tt.tag_id \
             WHERE tt.track_id = t.id AND tg.namespace = ? AND tg.value = ?)";
        if tok.negated {
            sql.push_str(" AND NOT ");
        } else {
            sql.push_str(" AND ");
        }
        sql.push_str(exists_clause);
        binds.push(tok.namespace.to_string());
        binds.push(tok.value.to_string());
    }

    sql.push_str(" ORDER BY album_artist, album, disc_no, track_no, title LIMIT ? OFFSET ?");

    // Safe: dynamic part only toggles fixed clauses; all values are bound below.
    let mut query = sqlx::query_as::<_, Track>(sqlx::AssertSqlSafe(sql));
    for b in &binds {
        query = query.bind(b);
    }
    let rows = query
        .bind(limit)
        .bind(offset)
        .fetch_all(state.pool(lib_id)?)
        .await?;
    Ok(Json(rows))
}
