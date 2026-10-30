//! Liveness (`/healthz`, no DB) and readiness (`/api/v1/health`, DB reachable) (R-F2).

use super::Shared;
use crate::query::build;
use axum::{Json, extract::State, http::StatusCode};
use serde_json::{Value, json};

pub async fn healthz() -> &'static str {
    "ok"
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
