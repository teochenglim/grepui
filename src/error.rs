//! API errors: always JSON `{"error": ...}`; database details never reach the client (R-Q3).

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

#[derive(Debug)]
pub struct ApiError(pub StatusCode, pub String);

impl ApiError {
    pub fn bad(m: impl Into<String>) -> Self {
        ApiError(StatusCode::BAD_REQUEST, m.into())
    }
    pub fn not_found(m: impl Into<String>) -> Self {
        ApiError(StatusCode::NOT_FOUND, m.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

/// How a database call failed.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("query timed out")]
    Timeout,
    #[error("database error")]
    Db(String),
}

impl ReadError {
    /// The underlying reason, for operator-facing messages (startup, logs). Never sent to HTTP clients.
    pub fn detail(&self) -> String {
        match self {
            ReadError::Timeout => "the query timed out".into(),
            ReadError::Db(d) => d.clone(),
        }
    }
}

impl From<ReadError> for ApiError {
    fn from(e: ReadError) -> Self {
        match e {
            ReadError::Timeout => ApiError(
                StatusCode::GATEWAY_TIMEOUT,
                "the query took too long and was cancelled".into(),
            ),
            ReadError::Db(detail) => {
                // Logged for the operator; the client gets a generic message (no URLs, no SQL).
                tracing::error!(detail = %detail, "database error");
                ApiError(
                    StatusCode::BAD_GATEWAY,
                    "the database returned an error".into(),
                )
            }
        }
    }
}
