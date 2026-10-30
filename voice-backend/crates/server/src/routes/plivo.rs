//! Plivo-facing routes: webhooks (signature-checked) and the media WebSocket.
//!
//! Webhooks are form-encoded POSTs signed with `X-Plivo-Signature-V3`;
//! unsigned or wrongly signed requests get 403. The media WebSocket is
//! authenticated with the per-call token embedded in the URL we gave
//! Plivo in the answer XML.

use axum::{
    body::Bytes,
    extract::{ws::WebSocketUpgrade, Path, Query, State},
    http::{header, HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use common::{
    ids::{CallId, DEFAULT_TENANT},
    AppError, ErrorCode,
};
use serde::Deserialize;
use serde_json::Value;
use subtle::ConstantTimeEq;
use telephony::{signature, xml};

use crate::{
    error::{ApiError, ApiResult},
    services::{calls, postcall, util::stream_token},
    state::AppState,
};

/// Parsed, verified webhook parameters.
struct Form(Vec<(String, String)>);

impl Form {
    fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .filter(|v| !v.is_empty())
    }
}

fn verify(state: &AppState, uri: &Uri, headers: &HeaderMap, body: &[u8]) -> Result<Form, AppError> {
    let params: Vec<(String, String)> = form_urlencoded::parse(body).into_owned().collect();
    let forbidden = |m: &str| AppError::new(ErrorCode::InvalidSignature, m.to_owned());
    let Some(token) = &state.config.plivo.auth_token else {
        return Err(forbidden("Plivo is not configured"));
    };
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let (Some(sig), Some(nonce)) = (
        header(signature::SIGNATURE_HEADER),
        header(signature::NONCE_HEADER),
    ) else {
        return Err(forbidden("missing Plivo signature"));
    };
    let path = uri.path_and_query().map_or(uri.path(), |p| p.as_str());
    let url = format!("{}{path}", state.config.public_base_url);
    if signature::verify_post(token.expose(), &url, &params, nonce, sig) {
        Ok(Form(params))
    } else {
        tracing::warn!(%url, "rejected webhook with invalid Plivo signature");
        Err(forbidden("invalid Plivo signature"))
    }
}

fn xml_response(body: String) -> Response {
    ([(header::CONTENT_TYPE, "application/xml")], body).into_response()
}

/// `POST /plivo/answer/{call_id}`: start recording and connect the media stream.
pub async fn answer(
    State(state): State<AppState>,
    Path(id): Path<CallId>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let form = verify(&state, &uri, &headers, &body)?;
    let Some(slot) = state.calls.get(id) else {
        tracing::warn!(call = %id, "answer for unknown call; hanging up");
        return Ok(xml_response(xml::hangup()));
    };
    if let Some(uuid) = form.get("CallUUID") {
        slot.set_plivo_uuid(uuid);
        sqlx::query(
            "UPDATE calls SET plivo_call_uuid = $2, status = 'in_progress', answered_at = COALESCE(answered_at, now()) WHERE id = $1",
        )
        .bind(id.0)
        .bind(uuid)
        .execute(&state.db)
        .await
        .map_err(AppError::from)?;

        // Record the whole call. Plivo's default time limit is 60 s.
        if let Some(plivo) = state.plivo.clone() {
            let uuid = uuid.to_owned();
            let callback = format!("{}/plivo/recording/{id}", state.config.public_base_url);
            let limit = slot.max_duration_secs + 60;
            tokio::spawn(async move {
                if let Err(e) = plivo.start_recording(&uuid, limit, &callback).await {
                    tracing::error!(call = %id, error = %e, "could not start recording");
                }
            });
        }
    }
    let ws = calls::stream_url(&state, id);
    let status_cb = format!("{}/plivo/stream-status/{id}", state.config.public_base_url);
    Ok(xml_response(xml::stream_answer(
        &ws,
        &status_cb,
        slot.max_duration_secs + 120,
    )))
}

/// `POST /plivo/ring/{call_id}`: the phone is ringing; prewarm connections.
pub async fn ring(
    State(state): State<AppState>,
    Path(id): Path<CallId>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<StatusCode> {
    let form = verify(&state, &uri, &headers, &body)?;
    if let Some(slot) = state.calls.get(id) {
        if let Some(uuid) = form.get("CallUUID") {
            slot.set_plivo_uuid(uuid);
        }
        sqlx::query("UPDATE calls SET status = 'ringing', plivo_call_uuid = COALESCE($2, plivo_call_uuid) WHERE id = $1 AND status = 'queued'")
            .bind(id.0)
            .bind(form.get("CallUUID"))
            .execute(&state.db)
            .await
            .map_err(AppError::from)?;
        tokio::spawn(calls::prewarm(state.clone(), slot));
    }
    Ok(StatusCode::OK)
}

/// Maps Plivo's final `CallStatus` to ours.
fn final_status(call_status: Option<&str>) -> &'static str {
    match call_status.unwrap_or_default() {
        "completed" => "completed",
        "busy" => "busy",
        "no-answer" | "timeout" | "cancel" => "no_answer",
        _ => "failed",
    }
}

/// `POST /plivo/hangup/{call_id}`: final status and duration.
pub async fn hangup(
    State(state): State<AppState>,
    Path(id): Path<CallId>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<StatusCode> {
    let form = verify(&state, &uri, &headers, &body)?;
    let status = final_status(form.get("CallStatus"));
    let duration: Option<i32> = form.get("Duration").and_then(|d| d.parse().ok());
    let reason = form
        .get("HangupCauseName")
        .or_else(|| form.get("HangupCause"))
        .or_else(|| form.get("CallStatus"));
    sqlx::query(
        "UPDATE calls SET status = $2, ended_at = COALESCE(ended_at, now()), \
         duration_secs = COALESCE($3, duration_secs, EXTRACT(EPOCH FROM (now() - answered_at))::int), \
         end_reason = COALESCE(end_reason, $4), plivo_call_uuid = COALESCE(plivo_call_uuid, $5) WHERE id = $1",
    )
    .bind(id.0)
    .bind(status)
    .bind(duration)
    .bind(reason)
    .bind(form.get("CallUUID"))
    .execute(&state.db)
    .await
    .map_err(AppError::from)?;
    // Unanswered calls never stream: drop the slot (and any prewarmed sockets).
    if let Some(slot) = state.calls.get(id) {
        if !slot.streaming.load(std::sync::atomic::Ordering::Relaxed) {
            state.calls.remove(id);
        }
    }
    if let Some(uuid) = form.get("CallUUID") {
        tokio::spawn(postcall::fetch_telephony_cost(
            state.clone(),
            id,
            uuid.to_owned(),
        ));
        if form.get("AnswerTime").is_some() || duration.is_some_and(|d| d > 0) {
            tokio::spawn(postcall::recording_fallback(
                state.clone(),
                id,
                uuid.to_owned(),
            ));
        }
    }
    tracing::info!(call = %id, %status, reason = ?reason, "call ended (plivo)");
    Ok(StatusCode::OK)
}

/// `POST /plivo/recording/{call_id}`: recording ready.
pub async fn recording(
    State(state): State<AppState>,
    Path(id): Path<CallId>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<StatusCode> {
    let form = verify(&state, &uri, &headers, &body)?;
    // The Record API callback posts one `response` field holding JSON;
    // XML <Record> callbacks use flat `RecordUrl`/`RecordingID` fields.
    let nested: Option<Value> = form
        .get("response")
        .and_then(|r| serde_json::from_str(r).ok());
    let field = |flat: &str, json_key: &str| -> Option<String> {
        form.get(flat).map(str::to_owned).or_else(|| {
            nested
                .as_ref()
                .and_then(|v| v.get(json_key))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
    };
    let Some(url) = field("record_url", "record_url").or_else(|| field("RecordUrl", "record_url"))
    else {
        let keys: Vec<&str> = form.0.iter().map(|(k, _)| k.as_str()).collect();
        tracing::warn!(call = %id, ?keys, "recording callback without record_url");
        return Err(ApiError(AppError::bad_request("record_url missing")));
    };
    tracing::info!(call = %id, "recording callback received");
    let recording_id =
        field("recording_id", "recording_id").or_else(|| field("RecordingID", "recording_id"));
    tokio::spawn(postcall::store_recording(
        state.clone(),
        DEFAULT_TENANT,
        id,
        url,
        recording_id,
    ));
    Ok(StatusCode::OK)
}

/// `POST /plivo/stream-status/{call_id}`: stream lifecycle events (logged).
pub async fn stream_status(
    State(state): State<AppState>,
    Path(id): Path<CallId>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<StatusCode> {
    let form = verify(&state, &uri, &headers, &body)?;
    tracing::info!(
        call = %id,
        event = ?form.get("Event"),
        reason = ?form.get("StatusReason"),
        "stream status"
    );
    Ok(StatusCode::OK)
}

/// Query of the media WebSocket URL.
#[derive(Deserialize)]
pub struct StreamQuery {
    t: String,
}

/// `GET /plivo/stream/{call_id}?t=<token>`: bidirectional call audio.
pub async fn stream(
    State(state): State<AppState>,
    Path(id): Path<CallId>,
    Query(q): Query<StreamQuery>,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    let expected = stream_token(state.config.api_key.expose(), id);
    if !bool::from(q.t.as_bytes().ct_eq(expected.as_bytes())) {
        return Err(ApiError(AppError::new(
            ErrorCode::InvalidSignature,
            "invalid stream token",
        )));
    }
    let Some(slot) = state.calls.get(id) else {
        return Err(ApiError(AppError::not_found(
            "call not found or already finished",
        )));
    };
    Ok(ws
        .max_message_size(1 << 20)
        .on_upgrade(move |socket| calls::run_media(state, slot, socket)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_final_status() {
        assert_eq!(final_status(Some("completed")), "completed");
        assert_eq!(final_status(Some("busy")), "busy");
        assert_eq!(final_status(Some("no-answer")), "no_answer");
        assert_eq!(final_status(Some("timeout")), "no_answer");
        assert_eq!(final_status(Some("failed")), "failed");
        assert_eq!(final_status(None), "failed");
    }
}
