use crate::api::error::{ApiError, ApiResult};
use crate::api::SharedState;
use crate::users::User;
use axum::extract::{Path, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use base64::Engine;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

/// The requester's identity and permissions, attached to the request by
/// `authenticate` and read by `require_library_read` and by handlers that
/// gate a specific action (e.g. playlist writes, downloader runs).
#[derive(Clone)]
pub enum CurrentUser {
    /// No `[[user]]` sections are configured — the whole API is open, same
    /// as today's behavior with no `auth_token` set.
    Open,
    Authenticated(Arc<User>),
}

impl CurrentUser {
    pub fn can_read(&self, library_id: i64) -> bool {
        match self {
            CurrentUser::Open => true,
            CurrentUser::Authenticated(u) => u.access(library_id).read,
        }
    }

    pub fn can_write(&self, library_id: i64) -> bool {
        match self {
            CurrentUser::Open => true,
            CurrentUser::Authenticated(u) => u.access(library_id).write,
        }
    }

    pub fn can_upload(&self, library_id: i64) -> bool {
        match self {
            CurrentUser::Open => true,
            CurrentUser::Authenticated(u) => u.access(library_id).upload,
        }
    }

    pub fn require_write(&self, library_id: i64) -> ApiResult<()> {
        if self.can_write(library_id) {
            Ok(())
        } else {
            Err(ApiError::forbidden("write access required"))
        }
    }

    pub fn require_upload(&self, library_id: i64) -> ApiResult<()> {
        if self.can_upload(library_id) {
            Ok(())
        } else {
            Err(ApiError::forbidden("upload access required"))
        }
    }
}

/// Authenticates the request and attaches a `CurrentUser` to it. If no users
/// are configured, every request is treated as `CurrentUser::Open`.
/// Otherwise requires `Authorization: Basic <base64(username:token)>`
/// matching a configured user, comparing the token in constant time.
pub async fn authenticate(State(state): State<SharedState>, mut req: Request, next: Next) -> Response {
    if state.users.is_empty() {
        req.extensions_mut().insert(CurrentUser::Open);
        return next.run(req).await;
    }

    let creds = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_basic);

    let user = creds.and_then(|(username, token)| {
        state
            .users
            .iter()
            .find(|u| u.username == username)
            .filter(|u| constant_time_eq(token.as_bytes(), u.token.as_bytes()))
    });

    match user {
        Some(u) => {
            req.extensions_mut()
                .insert(CurrentUser::Authenticated(Arc::new(u.clone())));
            next.run(req).await
        }
        None => unauthorized(),
    }
}

/// Layered onto the library-scoped subrouter (nested under
/// `/api/libraries/{lib_id}`), after `authenticate`. 404s — the same
/// response as a nonexistent library id — if the library doesn't exist or
/// the current user lacks read access, so unreadable libraries are
/// indistinguishable from ones that were never configured.
///
/// Extracts `lib_id` via `Path<HashMap<...>>` rather than `Path<i64>`:
/// nested routes carry extra path params (playlist id, track id, ...), and a
/// single-value `Path` extractor requires exactly one param in the whole
/// matched route.
pub async fn require_library_read(
    State(state): State<SharedState>,
    Extension(user): Extension<CurrentUser>,
    Path(params): Path<HashMap<String, String>>,
    req: Request,
    next: Next,
) -> Response {
    let readable = params
        .get("lib_id")
        .and_then(|s| s.parse::<i64>().ok())
        .is_some_and(|id| state.require_library(id).is_ok() && user.can_read(id));

    if !readable {
        return ApiError::not_found("library").into_response();
    }

    next.run(req).await
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Basic realm=\"muserv\"")],
        Json(json!({ "error": "unauthorized" })),
    )
        .into_response()
}

fn parse_basic(value: &str) -> Option<(String, String)> {
    let b64 = value.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (username, token) = text.split_once(':')?;
    Some((username.to_string(), token.to_string()))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_basic_header() {
        let header = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("alan:secret")
        );
        assert_eq!(
            parse_basic(&header),
            Some(("alan".to_string(), "secret".to_string()))
        );
    }

    #[test]
    fn rejects_non_basic_header() {
        assert_eq!(parse_basic("Bearer sometoken"), None);
    }

    #[test]
    fn rejects_malformed_base64() {
        assert_eq!(parse_basic("Basic not-valid-base64!!"), None);
    }

    #[test]
    fn constant_time_eq_matches_equal_slices() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    #[test]
    fn open_user_can_do_everything() {
        let user = CurrentUser::Open;
        assert!(user.can_read(1) && user.can_write(1) && user.can_upload(1));
    }
}
