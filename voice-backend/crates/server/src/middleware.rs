//! Request middleware: API-key auth and HTTP metrics.

use std::time::Instant;

use axum::{
    extract::{MatchedPath, Request, State},
    middleware::Next,
    response::Response,
};
use common::{metric_names, AppError, ErrorCode};
use subtle::ConstantTimeEq;

use crate::{error::ApiError, state::AppState};

/// Header carrying the static API key.
pub const API_KEY_HEADER: &str = "x-api-key";

/// Rejects requests without the configured `X-API-Key` (constant-time compare).
pub async fn require_api_key(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let provided = req
        .headers()
        .get(API_KEY_HEADER)
        .map(|v| v.as_bytes())
        .unwrap_or_default();
    let expected = state.config.api_key.expose().as_bytes();
    if bool::from(provided.ct_eq(expected)) {
        Ok(next.run(req).await)
    } else {
        Err(ApiError(AppError::new(
            ErrorCode::Unauthorized,
            "missing or invalid X-API-Key header",
        )))
    }
}

/// Records request count and latency per matched route.
pub async fn track_http(req: Request, next: Next) -> Response {
    let start = Instant::now();
    let method = req.method().as_str().to_owned();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_owned(), |p| p.as_str().to_owned());

    let response = next.run(req).await;

    let status = response.status().as_u16().to_string();
    metrics::counter!(
        metric_names::HTTP_REQUESTS_TOTAL,
        "method" => method.clone(),
        "route" => route.clone(),
        "status" => status
    )
    .increment(1);
    metrics::histogram!(
        metric_names::HTTP_REQUEST_DURATION_MS,
        "method" => method,
        "route" => route
    )
    .record(start.elapsed().as_secs_f64() * 1000.0);
    response
}
