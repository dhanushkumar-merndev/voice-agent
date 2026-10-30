//! `/v1/stats` (dashboard totals and daily series) and `/v1/options`
//! (what a new call can be configured with).

use axum::{
    extract::{Query, State},
    Json,
};
use chrono::{Duration as ChronoDuration, NaiveDate, Timelike, Utc};
use common::{ids::DEFAULT_TENANT, AppError};
use serde_json::{json, Value};

use crate::{error::ApiResult, state::AppState};

/// Query for [`stats`].
#[derive(Debug, serde::Deserialize)]
pub struct StatsQuery {
    /// Days to cover, ending today (India time), 1-365 (default 30).
    days: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct Totals {
    calls: i64,
    answered: i64,
    talk_secs: i64,
    stt: f64,
    tts: f64,
    llm: f64,
    extraction: f64,
    stt_retranscribe: f64,
    review: f64,
    tts_greeting: f64,
    ai: f64,
    telephony: f64,
    telephony_pending: i64,
    stt_seconds: f64,
    tts_chars: i64,
    reviewed: i64,
    avg_review_score: Option<f64>,
}

#[derive(sqlx::FromRow)]
struct Day {
    day: NaiveDate,
    calls: i64,
    answered: i64,
    talk_secs: i64,
    ai: f64,
    telephony: f64,
}

#[derive(sqlx::FromRow)]
struct Latency {
    turns: i64,
    v2v_p50: Option<f64>,
    v2v_p95: Option<f64>,
    stt_p50: Option<f64>,
    stt_p95: Option<f64>,
    llm_p50: Option<f64>,
    llm_p95: Option<f64>,
    tts_p50: Option<f64>,
    tts_p95: Option<f64>,
}

fn inr(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

fn ms(v: Option<f64>) -> Option<i64> {
    v.map(|v| v.round() as i64)
}

/// Counts calls grouped by one SQL expression over the window.
async fn group_counts(
    state: &AppState,
    expr: &str,
    first: NaiveDate,
) -> Result<Vec<Value>, AppError> {
    let sql = format!(
        "SELECT {expr} AS k, count(*) FROM calls \
         WHERE tenant_id = $1 AND created_at >= ($2::date)::timestamp AT TIME ZONE 'Asia/Kolkata' \
         GROUP BY 1 ORDER BY 2 DESC"
    );
    let rows: Vec<(Option<String>, i64)> = sqlx::query_as(&sql)
        .bind(DEFAULT_TENANT.0)
        .bind(first)
        .fetch_all(&state.db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(k, n)| json!({ "key": k.unwrap_or_else(|| "unknown".into()), "calls": n }))
        .collect())
}

/// `GET /v1/stats?days=30`: totals, cost by service, a per-day series
/// (India dates, every day present), outcomes, languages, lead interest
/// and latency percentiles for the last `days` days.
pub async fn stats(
    State(state): State<AppState>,
    Query(q): Query<StatsQuery>,
) -> ApiResult<Json<Value>> {
    let days = q.days.unwrap_or(30).clamp(1, 365);
    let today = Utc::now()
        .with_timezone(&chrono_tz::Asia::Kolkata)
        .date_naive();
    let first = today - ChronoDuration::days(days - 1);
    let window =
        "tenant_id = $1 AND created_at >= ($2::date)::timestamp AT TIME ZONE 'Asia/Kolkata'";

    let t: Totals = sqlx::query_as(&format!(
        "SELECT count(*) AS calls, count(answered_at) AS answered, \
           COALESCE(sum(duration_secs), 0)::int8 AS talk_secs, \
           COALESCE(sum((cost_breakdown->>'stt')::float8), 0) AS stt, \
           COALESCE(sum((cost_breakdown->>'tts')::float8), 0) AS tts, \
           COALESCE(sum((cost_breakdown->>'llm')::float8), 0) AS llm, \
           COALESCE(sum((cost_breakdown->>'extraction')::float8), 0) AS extraction, \
           COALESCE(sum((cost_breakdown->>'stt_retranscribe')::float8), 0) AS stt_retranscribe, \
           COALESCE(sum((cost_breakdown->>'review')::float8), 0) AS review, \
           COALESCE(sum((cost_breakdown->>'tts_greeting')::float8), 0) AS tts_greeting, \
           COALESCE(sum(cost_inr), 0) AS ai, \
           COALESCE(sum(telephony_cost_inr), 0) AS telephony, \
           count(*) FILTER (WHERE answered_at IS NOT NULL AND telephony_cost_inr IS NULL) AS telephony_pending, \
           COALESCE(sum(stt_seconds), 0) AS stt_seconds, \
           COALESCE(sum(tts_chars), 0)::int8 AS tts_chars, \
           count(review_score) AS reviewed, avg(review_score)::float8 AS avg_review_score \
         FROM calls WHERE {window}"
    ))
    .bind(DEFAULT_TENANT.0)
    .bind(first)
    .fetch_one(&state.db)
    .await
    .map_err(AppError::from)?;

    let rows: Vec<Day> = sqlx::query_as(&format!(
        "SELECT (created_at AT TIME ZONE 'Asia/Kolkata')::date AS day, count(*) AS calls, \
           count(answered_at) AS answered, COALESCE(sum(duration_secs), 0)::int8 AS talk_secs, \
           COALESCE(sum(cost_inr), 0) AS ai, COALESCE(sum(telephony_cost_inr), 0) AS telephony \
         FROM calls WHERE {window} GROUP BY 1 ORDER BY 1"
    ))
    .bind(DEFAULT_TENANT.0)
    .bind(first)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::from)?;
    let other_days: Vec<(NaiveDate, f64)> = sqlx::query_as(
        "SELECT (created_at AT TIME ZONE 'Asia/Kolkata')::date, COALESCE(sum(cost_inr), 0) \
         FROM spend_ledger WHERE tenant_id = $1 \
           AND created_at >= ($2::date)::timestamp AT TIME ZONE 'Asia/Kolkata' GROUP BY 1",
    )
    .bind(DEFAULT_TENANT.0)
    .bind(first)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::from)?;
    let gst = 1.0 + state.assets.pricing.telephony.gst_percent / 100.0;
    let series: Vec<Value> = (0..days)
        .map(|i| {
            let day = first + ChronoDuration::days(i);
            let d = rows.iter().find(|r| r.day == day);
            let other = other_days
                .iter()
                .find(|(d, _)| *d == day)
                .map_or(0.0, |(_, v)| *v);
            json!({
                "date": day,
                "calls": d.map_or(0, |d| d.calls),
                "answered": d.map_or(0, |d| d.answered),
                "talk_minutes": inr(d.map_or(0, |d| d.talk_secs) as f64 / 60.0),
                "ai_cost": inr(d.map_or(0.0, |d| d.ai) + other),
                "telephony_cost": inr(d.map_or(0.0, |d| d.telephony)),
                "cost_incl_gst": inr((d.map_or(0.0, |d| d.ai + d.telephony) + other) * gst),
            })
        })
        .collect();

    let l: Latency = sqlx::query_as(
        "SELECT count(*) AS turns, \
           percentile_cont(0.5) WITHIN GROUP (ORDER BY first_audio_sent_ms) AS v2v_p50, \
           percentile_cont(0.95) WITHIN GROUP (ORDER BY first_audio_sent_ms) AS v2v_p95, \
           percentile_cont(0.5) WITHIN GROUP (ORDER BY stt_final_ms) AS stt_p50, \
           percentile_cont(0.95) WITHIN GROUP (ORDER BY stt_final_ms) AS stt_p95, \
           percentile_cont(0.5) WITHIN GROUP (ORDER BY llm_first_token_ms) AS llm_p50, \
           percentile_cont(0.95) WITHIN GROUP (ORDER BY llm_first_token_ms) AS llm_p95, \
           percentile_cont(0.5) WITHIN GROUP (ORDER BY tts_first_byte_ms) AS tts_p50, \
           percentile_cont(0.95) WITHIN GROUP (ORDER BY tts_first_byte_ms) AS tts_p95 \
         FROM turn_latency WHERE tenant_id = $1 \
           AND created_at >= ($2::date)::timestamp AT TIME ZONE 'Asia/Kolkata'",
    )
    .bind(DEFAULT_TENANT.0)
    .bind(first)
    .fetch_one(&state.db)
    .await
    .map_err(AppError::from)?;

    let (booked,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM appointments a JOIN calls c ON c.id = a.call_id \
         WHERE c.tenant_id = $1 AND c.created_at >= ($2::date)::timestamp AT TIME ZONE 'Asia/Kolkata'",
    )
    .bind(DEFAULT_TENANT.0)
    .bind(first)
    .fetch_one(&state.db)
    .await
    .map_err(AppError::from)?;

    #[allow(clippy::type_complexity)]
    let agents: Vec<(Option<uuid::Uuid>, Option<String>, i64, i64, i64, f64, i64, Option<f64>)> =
        sqlx::query_as(
            "SELECT c.agent_id, g.name, count(*), count(c.answered_at), \
               COALESCE(sum(c.duration_secs), 0)::int8, \
               COALESCE(sum(c.cost_inr + COALESCE(c.telephony_cost_inr, 0)), 0), \
               (SELECT count(*) FROM appointments a JOIN calls x ON x.id = a.call_id \
                  WHERE x.agent_id IS NOT DISTINCT FROM c.agent_id AND x.tenant_id = $1 \
                    AND x.created_at >= ($2::date)::timestamp AT TIME ZONE 'Asia/Kolkata'), \
               avg(c.review_score)::float8 \
             FROM calls c LEFT JOIN agents g ON g.id = c.agent_id \
             WHERE c.tenant_id = $1 AND c.created_at >= ($2::date)::timestamp AT TIME ZONE 'Asia/Kolkata' \
             GROUP BY c.agent_id, g.name ORDER BY 3 DESC",
        )
        .bind(DEFAULT_TENANT.0)
        .bind(first)
        .fetch_all(&state.db)
        .await
        .map_err(AppError::from)?;
    let by_agent: Vec<Value> = agents
        .iter()
        .map(|(id, name, calls, answered, secs, cost, booked, score)| {
            json!({
                "agent_id": id,
                "name": name.clone().unwrap_or_else(|| if id.is_some() { "(deleted agent)".into() } else { "(no agent)".into() }),
                "calls": calls,
                "answered": answered,
                "talk_minutes": inr(*secs as f64 / 60.0),
                "cost_excl_gst": inr(*cost),
                "cost_incl_gst": inr(cost * gst),
                "appointments": booked,
                "avg_review_score": score.map(|s| (s * 10.0).round() / 10.0),
            })
        })
        .collect();
    let by_status = group_counts(&state, "status", first).await?;
    let by_language = group_counts(&state, "COALESCE(detected_language, language)", first).await?;
    let by_end_reason = group_counts(&state, "end_reason", first).await?;
    let by_interest = group_counts(&state, "extracted_fields->>'interest_level'", first).await?;

    let other = off_call_spend(&state, window_tail(), first).await?;
    let talk_min = t.talk_secs as f64 / 60.0;
    // Per-call figures use call spend only; the total includes off-call spend.
    let calls_excl = t.ai + t.telephony;
    let excl = calls_excl + other.total;
    let rental = state.assets.pricing.telephony.number_rental_inr_per_month;
    Ok(Json(json!({
        "currency": "INR",
        "from": first,
        "to": today,
        "days": days,
        "gst_percent": state.assets.pricing.telephony.gst_percent,
        "totals": {
            "calls": t.calls,
            "answered": t.answered,
            "answer_rate": (t.calls > 0).then(|| inr(t.answered as f64 / t.calls as f64)),
            "talk_minutes": inr(talk_min),
            "avg_call_secs": (t.answered > 0).then(|| (t.talk_secs as f64 / t.answered as f64).round()),
            "appointments_booked": booked,
            "active_calls": state.calls.len(),
            "stt_seconds": inr(t.stt_seconds),
            "tts_chars": t.tts_chars,
            "reviewed_calls": t.reviewed,
            "avg_review_score": t.avg_review_score.map(|s| (s * 10.0).round() / 10.0),
        },
        "cost": {
            "ai": {
                "stt": inr(t.stt),
                "tts": inr(t.tts),
                "llm": inr(t.llm),
                "extraction": inr(t.extraction),
                "stt_retranscribe": inr(t.stt_retranscribe),
                "review": inr(t.review),
                "tts_greeting": inr(t.tts_greeting),
                "total": inr(t.ai),
            },
            "other": other.json(),
            "telephony": inr(t.telephony),
            "telephony_pending_calls": t.telephony_pending,
            "number_rental_per_month": inr(rental),
            "total_excl_gst": inr(excl),
            "total_incl_gst": inr(excl * gst),
            "per_talk_minute_excl_gst": (talk_min > 0.0).then(|| inr(calls_excl / talk_min)),
            "per_answered_call_excl_gst": (t.answered > 0).then(|| inr(calls_excl / t.answered as f64)),
            "per_answered_call_incl_gst": (t.answered > 0).then(|| inr(calls_excl * gst / t.answered as f64)),
        },
        "latency_ms": {
            "turns": l.turns,
            "voice_to_voice": { "p50": ms(l.v2v_p50), "p95": ms(l.v2v_p95) },
            "stt_final": { "p50": ms(l.stt_p50), "p95": ms(l.stt_p95) },
            "llm_first_token": { "p50": ms(l.llm_p50), "p95": ms(l.llm_p95) },
            "tts_first_byte": { "p50": ms(l.tts_p50), "p95": ms(l.tts_p95) },
        },
        "daily": series,
        "by_agent": by_agent,
        "by_status": by_status,
        "by_language": by_language,
        "by_end_reason": by_end_reason,
        "by_interest": by_interest,
    })))
}

/// `GET /v1/options`: personas, languages and voices, models and limits a
/// new call can use, plus whether calling is allowed right now.
pub async fn options(State(state): State<AppState>) -> Json<Value> {
    let a = &state.assets;
    let cfg = &state.config;
    let mut personas: Vec<Value> = a
        .personas
        .iter()
        .map(|(name, text)| {
            let first = text
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("");
            json!({ "name": name, "description": first.chars().take(200).collect::<String>(), "prompt": text })
        })
        .collect();
    personas.sort_by(|x, y| x["name"].as_str().cmp(&y["name"].as_str()));
    let mut languages: Vec<Value> = a
        .voices
        .languages
        .iter()
        .map(|(code, v)| {
            json!({
                "code": code,
                "speaker": v.speaker,
                "model": v.model.clone().unwrap_or_else(|| cfg.sarvam.tts_model.clone()),
                "agent_name": v.name,
            })
        })
        .collect();
    languages.sort_by(|x, y| x["code"].as_str().cmp(&y["code"].as_str()));
    let mut llm_models: Vec<&String> = a.pricing.llm.keys().collect();
    llm_models.sort();
    let mut tts_models: Vec<&String> = a.pricing.tts.keys().collect();
    tts_models.sort();
    let hours = &cfg.call_hours;
    let hour = Utc::now().with_timezone(&hours.timezone).hour();
    Json(json!({
        "personas": personas,
        "languages": languages,
        "fallback_language": a.voices.fallback_language,
        "llm_models": llm_models,
        "default_llm_model": cfg.llm.model,
        "tts_models": tts_models,
        "default_tts_model": cfg.sarvam.tts_model,
        "default_max_duration_secs": cfg.pipeline.default_max_duration_secs,
        "greeting_max_chars": 100,
        "call_hours": {
            "enforced": hours.enforced,
            "start_hour": hours.start_hour,
            "end_hour": hours.end_hour,
            "timezone": hours.timezone.name(),
            "open_now": !hours.enforced || (hour >= hours.start_hour && hour < hours.end_hour),
        },
        "embedding_model_ready": state.kb.available(),
    }))
}

/// The `created_at` window used by [`stats`], for [`off_call_spend`].
fn window_tail() -> &'static str {
    "created_at >= ($2::date)::timestamp AT TIME ZONE 'Asia/Kolkata'"
}

/// Spend that belongs to no single call (`spend_ledger`) in a window.
pub(crate) struct OffCallSpend {
    /// Phrase audio synthesized for the shared cache.
    pub tts_cache: f64,
    /// "Edit with AI" prompt edits.
    pub prompt_edit: f64,
    /// Both.
    pub total: f64,
}

impl OffCallSpend {
    /// As returned by the API.
    pub fn json(&self) -> Value {
        json!({
            "tts_cache": inr(self.tts_cache),
            "prompt_edit": inr(self.prompt_edit),
            "total": inr(self.total),
        })
    }
}

/// Sums `spend_ledger` for the tenant where `window` holds; `window` uses
/// `$2` for the first date.
pub(crate) async fn off_call_spend(
    state: &AppState,
    window: &str,
    first: NaiveDate,
) -> Result<OffCallSpend, AppError> {
    let (tts_cache, prompt_edit): (f64, f64) = sqlx::query_as(&format!(
        "SELECT COALESCE(sum(cost_inr) FILTER (WHERE kind = 'tts_cache'), 0), \
           COALESCE(sum(cost_inr) FILTER (WHERE kind = 'prompt_edit'), 0) \
         FROM spend_ledger WHERE tenant_id = $1 AND {window}"
    ))
    .bind(DEFAULT_TENANT.0)
    .bind(first)
    .fetch_one(&state.db)
    .await?;
    Ok(OffCallSpend {
        tts_cache,
        prompt_edit,
        total: tts_cache + prompt_edit,
    })
}
