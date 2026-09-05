use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl ApiError {
    pub fn not_found(what: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: format!("{what} not found"),
        }
    }
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: msg.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        tracing::error!(?e, "db error");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "database error".into(),
        }
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        tracing::error!(?e, "io error");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "io error".into(),
        }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
