//! Router assembly.

use axum::{
    extract::{DefaultBodyLimit, Request},
    middleware::{from_fn, from_fn_with_state},
    routing::{delete, get, post},
    Json, Router,
};
use common::AppError;
use serde_json::{json, Value};
use tower::ServiceBuilder;
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};

use crate::{
    error::ApiError,
    middleware::{require_api_key, track_http},
    routes::{agents, calls, health, kb, plivo, stats},
    state::AppState,
};

/// Builds the full application router.
///
/// - `/healthz`, `/readyz`, `/metrics`: unauthenticated operations endpoints.
/// - `/v1/*`: REST API, requires `X-API-Key`.
/// - `/plivo/*`: Plivo webhooks (signature-checked) and the media WebSocket (token-checked).
pub fn build_router(state: AppState) -> Router {
    let mut api = Router::new()
        .route("/v1/ping", get(ping))
        .route("/v1/calls", post(calls::create_call).get(calls::list_calls))
        .route("/v1/calls/{id}", get(calls::get_call))
        .route("/v1/calls/{id}/hangup", post(calls::hangup_call))
        .route("/v1/calls/{id}/feedback", post(calls::call_feedback))
        .route("/v1/calls/{id}/review", post(calls::review_call))
        .route("/v1/costs", get(calls::cost_summary))
        .route("/v1/stats", get(stats::stats))
        .route(
            "/v1/agents",
            get(agents::list_agents).post(agents::create_agent),
        )
        .route(
            "/v1/agents/{id}",
            get(agents::get_agent)
                .put(agents::update_agent)
                .delete(agents::delete_agent),
        )
        .route("/v1/agents/prompt-edit", post(agents::edit_prompt))
        .route(
            "/v1/agents/{id}/training",
            get(agents::get_training).put(agents::put_training),
        )
        .route("/v1/agents/{id}/train.md", get(agents::download_training))
        .route("/v1/agents/{id}/lessons", get(agents::list_lessons))
        .route(
            "/v1/agents/{id}/lessons/{lesson}/apply",
            post(agents::apply_lesson),
        )
        .route(
            "/v1/agents/{id}/lessons/{lesson}/reject",
            post(agents::reject_lesson),
        )
        .route("/v1/options", get(stats::options))
        .route("/v1/knowledge-bases", post(kb::create_kb).get(kb::list_kbs))
        .route("/v1/knowledge-bases/{id}", delete(kb::delete_kb))
        .route(
            "/v1/knowledge-bases/{id}/documents/{doc}",
            get(kb::get_document).delete(kb::delete_document),
        )
        .route(
            "/v1/knowledge-bases/{id}/documents",
            post(kb::upload_document)
                .get(kb::list_documents)
                .layer(DefaultBodyLimit::max(kb::MAX_UPLOAD_BYTES + 64 * 1024)),
        )
        .route("/v1/knowledge-bases/{id}/search", post(kb::search));
    if state.config.is_local() {
        api = api.route("/v1/dev/sim-calls", post(calls::create_sim_call));
    }
    let api = api.route_layer(from_fn_with_state(state.clone(), require_api_key));

    let plivo = Router::new()
        .route("/plivo/answer/{id}", post(plivo::answer))
        .route("/plivo/ring/{id}", post(plivo::ring))
        .route("/plivo/hangup/{id}", post(plivo::hangup))
        .route("/plivo/recording/{id}", post(plivo::recording))
        .route("/plivo/stream-status/{id}", post(plivo::stream_status))
        .route("/plivo/stream/{id}", get(plivo::stream));

    let ops = Router::new()
        .route("/healthz", get(health::healthz))
        .route("/readyz", get(health::readyz))
        .route("/metrics", get(health::metrics));

    Router::new()
        .merge(ops)
        .merge(api)
        .merge(plivo)
        .route_layer(from_fn(track_http))
        .fallback(not_found)
        .layer(
            ServiceBuilder::new()
                .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
                .layer(TraceLayer::new_for_http().make_span_with(|req: &Request| {
                    let request_id = req
                        .headers()
                        .get("x-request-id")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("-");
                    tracing::info_span!(
                        "http",
                        method = %req.method(),
                        path = %req.uri().path(),
                        request_id = %request_id,
                    )
                }))
                .layer(PropagateRequestIdLayer::x_request_id()),
        )
        .with_state(state)
}

/// Authenticated no-op, handy for checking an API key.
async fn ping() -> Json<Value> {
    Json(json!({ "ok": true }))
}

async fn not_found() -> ApiError {
    ApiError(AppError::not_found("no such route"))
}
