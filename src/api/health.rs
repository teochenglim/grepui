//! Liveness (`/healthz`, no DB) and readiness (`/api/v1/health`, DB reachable) (R-F2).

use super::Shared;
use crate::query::build;
use axum::{Json, extract::State, http::StatusCode};
use serde_json::{Value, json};

pub async fn healthz() -> &'static str {
    "ok"
}

/// Prometheus text for this process (R-O2).
pub async fn metrics(State(s): State<Shared>) -> impl axum::response::IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        s.metrics.render(),
    )
}

pub async fn health(State(s): State<Shared>) -> (StatusCode, Json<Value>) {
    match s.run(build::health()).await {
        Ok(_) => (StatusCode::OK, Json(json!({"status": "ok", "db": "up"}))),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"status": "degraded", "db": "down"})),
        ),
    }
}
