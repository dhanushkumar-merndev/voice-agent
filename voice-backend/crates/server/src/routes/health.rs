//! `/healthz`, `/readyz` and `/metrics`.

use std::time::Duration;

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

use crate::state::AppState;

/// Per-dependency timeout for readiness checks.
const CHECK_TIMEOUT: Duration = Duration::from_secs(2);

/// Liveness: the process is up and serving HTTP.
pub async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

/// Readiness: not draining, and the database and object storage respond.
/// Returns 503 with per-check results otherwise.
pub async fn readyz(State(state): State<AppState>) -> Response {
    if state.lifecycle.is_draining() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "draining", "active_calls": state.lifecycle.active_calls() })),
        )
            .into_response();
    }

    let (db, storage) = tokio::join!(
        tokio::time::timeout(CHECK_TIMEOUT, common::db::ping(&state.db)),
        tokio::time::timeout(CHECK_TIMEOUT, state.storage.ping()),
    );
    let db_ok = match db {
        Ok(Ok(())) => true,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "readiness: database check failed");
            false
        }
        Err(_) => {
            tracing::warn!("readiness: database check timed out");
            false
        }
    };
    let storage_ok = match storage {
        Ok(Ok(())) => true,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "readiness: storage check failed");
            false
        }
        Err(_) => {
            tracing::warn!("readiness: storage check timed out");
            false
        }
    };

    let ready = db_ok && storage_ok;
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let body = json!({
        "status": if ready { "ready" } else { "not_ready" },
        "checks": { "database": db_ok, "storage": storage_ok },
    });
    (status, Json(body)).into_response()
}

/// Prometheus scrape endpoint.
pub async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        state.metrics.render(),
    )
}
