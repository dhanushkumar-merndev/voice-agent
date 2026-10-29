//! `/v1/calls` and the local-only `/v1/dev/sim-calls`.

use std::time::Duration;

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use common::{ids::CallId, AppError, ErrorCode};
use pipeline::CallCommand;
use serde_json::{json, Value};

use crate::{
    error::{ApiError, ApiResult},
    services::{
        calls::{self, CreateCall},
        idempotency::{self, Begin},
        util::{percentile, sha256_hex},
    },
    state::AppState,
};

fn parse_body<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, AppError> {
    serde_json::from_slice(body)
        .map_err(|e| AppError::bad_request(format!("invalid JSON body: {e}")))
}

/// `POST /v1/calls`: place an outbound call.
pub async fn create_call(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    if state.call_limiter.check().is_err() {
        return Err(ApiError(AppError::new(
            ErrorCode::RateLimited,
            "too many call requests; slow down",
        )));
    }
    let req: CreateCall = parse_body(&body)?;
    let key = idempotency::header(&headers)?;
    let claimed = match idempotency::begin(
        &state.db,
        common::ids::DEFAULT_TENANT,
        "POST /v1/calls",
        key,
        &sha256_hex(&body),
    )
    .await?
    {
        Begin::Replay(status, body) => return Ok((status, Json(body)).into_response()),
        Begin::Fresh(k) => k,
    };
    match calls::create(&state, req, false).await {
        Ok(slot) => {
            let body = json!({ "call_id": slot.id, "status": "queued" });
            idempotency::finish(&state.db, claimed, StatusCode::CREATED, &body).await;
            Ok((StatusCode::CREATED, Json(body)).into_response())
        }
        Err(e) => {
            idempotency::abandon(&state.db, claimed).await;
            Err(ApiError(e))
        }
    }
}

/// `POST /v1/dev/sim-calls`: a call without Plivo, for `voice-sim` (APP_ENV=local only).
pub async fn create_sim_call(State(state): State<AppState>, body: Bytes) -> ApiResult<Response> {
    let mut req: CreateCall = parse_body(&body)?;
    if req.to.trim().is_empty() {
        req.to = "+910000000000".into();
    }
    let slot = calls::create(&state, req, true).await?;
    let body = json!({
        "call_id": slot.id,
        "stream_url": calls::stream_url(&state, slot.id),
    });
    Ok((StatusCode::CREATED, Json(body)).into_response())
}

#[derive(sqlx::FromRow)]
struct CallRow {
    id: uuid::Uuid,
    status: String,
    to_number: String,
    lead_name: String,
    kb_id: Option<uuid::Uuid>,
    language: String,
    detected_language: Option<String>,
    voice: Option<String>,
    plivo_call_uuid: Option<String>,
    created_at: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
    answered_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
    duration_secs: Option<i32>,
    end_reason: Option<String>,
    recording_key: Option<String>,
    extracted_fields: Option<Value>,
    stt_seconds: f64,
    tts_chars: i32,
    llm_usage: Value,
    cost_inr: f64,
    cost_breakdown: Value,
    telephony_cost_inr: Option<f64>,
    telephony_billed_secs: Option<i32>,
    telephony_rate_inr_per_min: Option<f64>,
}

/// Rounds ₹ to paise precision for display.
fn inr(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

/// The `cost` object of a call: AI services, the phone charge and totals.
fn cost_json(call: &CallRow, gst_percent: f64) -> Value {
    let part = |k: &str| {
        call.cost_breakdown
            .get(k)
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
    };
    let telephony = call.telephony_cost_inr.unwrap_or(0.0);
    let total = call.cost_inr + telephony;
    let minutes = call
        .duration_secs
        .filter(|&d| d > 0)
        .map(|d| f64::from(d) / 60.0);
    json!({
        "currency": "INR",
        "ai": {
            "stt": inr(part("stt")),
            "tts": inr(part("tts")),
            "llm": inr(part("llm")),
            "extraction": inr(part("extraction")),
            // Post-call batch STT of the caller's turns.
            "stt_retranscribe": inr(part("stt_retranscribe")),
            "total": inr(call.cost_inr),
            // Reply characters played from cached audio (not billed by TTS).
            "tts_cached_chars": call.cost_breakdown.get("tts_cached_chars").and_then(Value::as_u64).unwrap_or(0),
        },
        "telephony": {
            "call": call.telephony_cost_inr.map(inr),
            "billed_secs": call.telephony_billed_secs,
            "rate_per_min": call.telephony_rate_inr_per_min,
            // Plivo's call record arrives shortly after hangup.
            "pending": call.telephony_cost_inr.is_none() && call.ended_at.is_some(),
            // Paid seconds after the call ended (billing blocks round up).
            "unused_paid_secs": call.telephony_billed_secs.zip(call.duration_secs).map(|(b, d)| (b - d).max(0)),
        },
        "per_minute_excl_gst": minutes.map(|m| inr(total / m)),
        "total_excl_gst": inr(total),
        "gst_percent": gst_percent,
        "total_incl_gst": inr(total * (1.0 + gst_percent / 100.0)),
    })
}

#[derive(sqlx::FromRow)]
struct TurnRow {
    seq: i32,
    role: String,
    text: String,
    live_text: Option<String>,
    language: Option<String>,
    interrupted: bool,
    started_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow, serde::Serialize)]
struct LatencyRow {
    turn_seq: i32,
    speech_end_ms: Option<i32>,
    stt_final_ms: Option<i32>,
    llm_first_token_ms: Option<i32>,
    tts_first_byte_ms: Option<i32>,
    first_audio_sent_ms: Option<i32>,
    spoken_chars: i32,
    cap_triggered: bool,
    llm_model: Option<String>,
    llm_attempts: i16,
}

fn stage(values: impl Iterator<Item = Option<i32>>) -> Value {
    let v: Vec<i64> = values.flatten().map(i64::from).collect();
    json!({ "p50": percentile(&v, 50.0), "p95": percentile(&v, 95.0), "n": v.len() })
}

/// `GET /v1/calls/{id}`: status, transcript, extracted fields, recording, latency, cost.
pub async fn get_call(
    State(state): State<AppState>,
    Path(id): Path<CallId>,
) -> ApiResult<Json<Value>> {
    let call: CallRow = sqlx::query_as("SELECT * FROM calls WHERE id = $1 AND tenant_id = $2")
        .bind(id.0)
        .bind(common::ids::DEFAULT_TENANT.0)
        .fetch_optional(&state.db)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("call not found"))?;
    let turns: Vec<TurnRow> = sqlx::query_as(
        "SELECT seq, role, text, live_text, language, interrupted, started_at FROM call_turns WHERE call_id = $1 ORDER BY seq",
    )
    .bind(id.0)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::from)?;
    let latency: Vec<LatencyRow> = sqlx::query_as(
        "SELECT turn_seq, speech_end_ms, stt_final_ms, llm_first_token_ms, tts_first_byte_ms, first_audio_sent_ms, \
         spoken_chars, cap_triggered, llm_model, llm_attempts FROM turn_latency WHERE call_id = $1 ORDER BY turn_seq",
    )
    .bind(id.0)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::from)?;
    let appointments: Vec<(DateTime<Utc>, i32, String, String)> = sqlx::query_as(
        "SELECT starts_at, duration_mins, name, status FROM appointments \
         WHERE call_id = $1 ORDER BY starts_at",
    )
    .bind(id.0)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::from)?;
    let followups: Vec<(String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT topic, created_at FROM call_followups WHERE call_id = $1 ORDER BY created_at",
    )
    .bind(id.0)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::from)?;

    let recording_url = match &call.recording_key {
        Some(key) => match state
            .storage
            .presign_get(key, Duration::from_secs(300))
            .await
        {
            Ok(url) => Some(url),
            Err(e) => {
                tracing::warn!(call = %id, error = %e, "could not sign recording URL");
                None
            }
        },
        None => None,
    };

    Ok(Json(json!({
        "call_id": call.id,
        "status": call.status,
        "to": call.to_number,
        "lead_name": call.lead_name,
        "knowledge_base_id": call.kb_id,
        "language": call.language,
        "detected_language": call.detected_language,
        "voice": call.voice,
        "plivo_call_uuid": call.plivo_call_uuid,
        "created_at": call.created_at,
        "started_at": call.started_at,
        "answered_at": call.answered_at,
        "ended_at": call.ended_at,
        "duration_secs": call.duration_secs,
        "end_reason": call.end_reason,
        "in_progress": state.calls.get(id).is_some(),
        "transcript": turns.iter().map(|t| json!({
            "seq": t.seq,
            "role": t.role,
            "text": t.text,
            "language": t.language,
            "interrupted": t.interrupted,
            // What the realtime STT heard, when re-transcription changed it.
            "live_text": t.live_text,
            "at": t.started_at,
        })).collect::<Vec<_>>(),
        "extracted_fields": call.extracted_fields,
        "follow_ups": followups.iter().map(|(t, at)| json!({ "topic": t, "at": at })).collect::<Vec<_>>(),
        "appointments": appointments.iter().map(|(at, mins, name, status)| json!({
            "starts_at": at, "duration_mins": mins, "name": name, "status": status,
        })).collect::<Vec<_>>(),
        "recording_url": recording_url,
        "recording_url_expires_in_secs": recording_url.as_ref().map(|_| 300),
        "latency": {
            "turns": latency.len(),
            "voice_to_voice_ms": stage(latency.iter().map(|l| l.first_audio_sent_ms)),
            "stt_final_ms": stage(latency.iter().map(|l| l.stt_final_ms)),
            "llm_first_token_ms": stage(latency.iter().map(|l| l.llm_first_token_ms)),
            "tts_first_byte_ms": stage(latency.iter().map(|l| l.tts_first_byte_ms)),
            "per_turn": latency,
        },
        "usage": {
            "stt_seconds": call.stt_seconds,
            "tts_chars": call.tts_chars,
            "llm": call.llm_usage,
        },
        "cost": cost_json(&call, state.assets.pricing.telephony.gst_percent),
        "cost_inr": inr(call.cost_inr + call.telephony_cost_inr.unwrap_or(0.0)),
    })))
}

/// Query for [`cost_summary`].
#[derive(Debug, serde::Deserialize)]
pub struct CostQuery {
    /// Month as `YYYY-MM` (India time); defaults to the current month.
    month: Option<String>,
}

#[derive(sqlx::FromRow)]
struct CostTotals {
    calls: i64,
    answered: i64,
    talk_secs: i64,
    stt: f64,
    tts: f64,
    llm: f64,
    extraction: f64,
    stt_retranscribe: f64,
    ai: f64,
    telephony: f64,
    telephony_pending: i64,
}

/// `GET /v1/costs?month=YYYY-MM`: every cost for a month: AI services,
/// phone charges and the number rent, before and after GST.
pub async fn cost_summary(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<CostQuery>,
) -> ApiResult<Json<Value>> {
    let month = q.month.unwrap_or_else(|| {
        Utc::now()
            .with_timezone(&chrono_tz::Asia::Kolkata)
            .format("%Y-%m")
            .to_string()
    });
    let first = chrono::NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d")
        .map_err(|_| AppError::bad_request("month must be YYYY-MM"))?;
    let t: CostTotals = sqlx::query_as(
        "SELECT count(*) AS calls, count(answered_at) AS answered, \
           COALESCE(sum(duration_secs), 0)::int8 AS talk_secs, \
           COALESCE(sum((cost_breakdown->>'stt')::float8), 0) AS stt, \
           COALESCE(sum((cost_breakdown->>'tts')::float8), 0) AS tts, \
           COALESCE(sum((cost_breakdown->>'llm')::float8), 0) AS llm, \
           COALESCE(sum((cost_breakdown->>'extraction')::float8), 0) AS extraction, \
           COALESCE(sum((cost_breakdown->>'stt_retranscribe')::float8), 0) AS stt_retranscribe, \
           COALESCE(sum(cost_inr), 0) AS ai, \
           COALESCE(sum(telephony_cost_inr), 0) AS telephony, \
           count(*) FILTER (WHERE answered_at IS NOT NULL AND telephony_cost_inr IS NULL) AS telephony_pending \
         FROM calls WHERE tenant_id = $1 \
           AND created_at >= ($2::date)::timestamp AT TIME ZONE 'Asia/Kolkata' \
           AND created_at < (($2::date) + interval '1 month') AT TIME ZONE 'Asia/Kolkata'",
    )
    .bind(common::ids::DEFAULT_TENANT.0)
    .bind(first)
    .fetch_one(&state.db)
    .await
    .map_err(AppError::from)?;

    let rates = &state.assets.pricing.telephony;
    let rental = rates.number_rental_inr_per_month;
    let total = t.ai + t.telephony + rental;
    let gst = rates.gst_percent;
    let per_call = |v: f64| (t.answered > 0).then(|| inr(v / t.answered as f64));
    Ok(Json(json!({
        "month": month,
        "currency": "INR",
        "calls": t.calls,
        "answered_calls": t.answered,
        "talk_minutes": inr(t.talk_secs as f64 / 60.0),
        "ai": {
            "stt": inr(t.stt),
            "tts": inr(t.tts),
            "llm": inr(t.llm),
            "extraction": inr(t.extraction),
            "stt_retranscribe": inr(t.stt_retranscribe),
            "total": inr(t.ai),
        },
        "telephony": {
            "calls": inr(t.telephony),
            "calls_waiting_for_plivo_cost": t.telephony_pending,
            "number_rental": inr(rental),
            "total": inr(t.telephony + rental),
        },
        "total_excl_gst": inr(total),
        "gst_percent": gst,
        "total_incl_gst": inr(total * (1.0 + gst / 100.0)),
        "per_talk_minute_excl_gst": (t.talk_secs > 0).then(|| inr((t.ai + t.telephony) / (t.talk_secs as f64 / 60.0))),
        "per_answered_call_excl_gst": {
            "ai_and_phone": per_call(t.ai + t.telephony),
            "including_number_rental": per_call(total),
        },
    })))
}

/// `POST /v1/calls/{id}/hangup`: end a call.
pub async fn hangup_call(
    State(state): State<AppState>,
    Path(id): Path<CallId>,
) -> ApiResult<Response> {
    if let Some(slot) = state.calls.get(id) {
        if slot.command(CallCommand::Hangup) {
            return Ok((
                StatusCode::ACCEPTED,
                Json(json!({ "status": "hangup_requested" })),
            )
                .into_response());
        }
        // Not streaming yet: hang up the ringing call, or cancel the
        // request if Plivo has not reported a call UUID yet.
        if let Some(plivo) = &state.plivo {
            let result = match (slot.plivo_uuid(), slot.request_uuid()) {
                (Some(uuid), _) => Some(plivo.hangup(&uuid).await),
                (None, Some(request)) => Some(plivo.cancel_request(&request).await),
                (None, None) => None,
            };
            if let Some(result) = result {
                result.map_err(|e| AppError::new(ErrorCode::ProviderError, e.to_string()))?;
                return Ok((
                    StatusCode::ACCEPTED,
                    Json(json!({ "status": "hangup_requested" })),
                )
                    .into_response());
            }
        }
    }
    let row: Option<(String,)> =
        sqlx::query_as("SELECT status FROM calls WHERE id = $1 AND tenant_id = $2")
            .bind(id.0)
            .bind(common::ids::DEFAULT_TENANT.0)
            .fetch_optional(&state.db)
            .await
            .map_err(AppError::from)?;
    match row {
        None => Err(ApiError(AppError::not_found("call not found"))),
        Some((status,)) => Err(ApiError(AppError::new(
            ErrorCode::Conflict,
            format!("call is not active (status: {status})"),
        ))),
    }
}
