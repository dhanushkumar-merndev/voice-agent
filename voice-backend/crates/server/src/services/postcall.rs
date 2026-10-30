//! After the call: persist transcript, latency and cost; extract lead
//! fields with the LLM; move the recording from Plivo to our storage.

use std::time::Duration;

use common::{
    config::Retranscribe,
    ids::{CallId, TenantId, DEFAULT_TENANT},
    metric_names, AppError,
};
use pipeline::CallOutcome;
use providers::llm::{ChatMessage, ChatRequest, LlmEndpoint, Role};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    agents::{self, LeadField},
    booking::{clean_spoken_name, has_indian_script},
    registry::CallSlot,
    util::percentile,
};
use crate::state::AppState;

/// Lead fields extracted after the call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractedFields {
    /// Caller's name as confirmed on the call.
    pub name: Option<String>,
    /// `high`, `medium`, `low` or `none`.
    pub interest_level: Option<InterestLevel>,
    /// Budget mentioned, verbatim.
    pub budget: Option<String>,
    /// Language the caller preferred (BCP-47 if known).
    pub preferred_language: Option<String>,
    /// Agreed follow-up date (YYYY-MM-DD) or description.
    pub follow_up_date: Option<String>,
    /// Objections raised.
    pub objections: Vec<String>,
    /// Two- or three-sentence summary.
    pub summary: String,
    /// The agent's own fields (`collect_fields` of the call), by key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<serde_json::Map<String, Value>>,
}

/// Interest level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InterestLevel {
    /// Ready to proceed.
    High,
    /// Interested, undecided.
    Medium,
    /// Mostly not interested.
    Low,
    /// Not interested or wrong person.
    None,
}

fn extraction_schema(fields: &[LeadField]) -> Value {
    let nullable = |t: &str| json!({ "type": [t, "null"] });
    let mut schema = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["name", "interest_level", "budget", "preferred_language", "follow_up_date", "objections", "summary"],
        "properties": {
            "name": nullable("string"),
            "interest_level": { "type": ["string", "null"], "enum": ["high", "medium", "low", "none", null] },
            "budget": nullable("string"),
            "preferred_language": nullable("string"),
            "follow_up_date": nullable("string"),
            "objections": { "type": "array", "items": { "type": "string" } },
            "summary": { "type": "string" }
        }
    });
    if !fields.is_empty() {
        if let Some(r) = schema["required"].as_array_mut() {
            r.push(json!("custom"));
        }
        schema["properties"]["custom"] = agents::custom_schema(fields);
    }
    schema
}

/// Saves everything the call produced and kicks off extraction.
pub async fn finalize(state: &AppState, slot: &CallSlot, outcome: CallOutcome) {
    if let Err(e) = persist(state, slot, &outcome).await {
        tracing::error!(call = %slot.id, error = %e, "could not persist call outcome");
    }
    state.calls.remove(slot.id);
    metrics::counter!(metric_names::CALLS_TOTAL, "outcome" => outcome.end_reason.clone())
        .increment(1);

    let has_user_turns = outcome
        .turns
        .iter()
        .any(|t| t.role == pipeline::Speaker::User);
    if has_user_turns {
        let clips = match state.config.sarvam.retranscribe_caller_turns {
            Retranscribe::All => caller_clips(&outcome),
            Retranscribe::NonEnglish if !english_only(&outcome) => caller_clips(&outcome),
            _ => Vec::new(),
        };
        let state = state.clone();
        let id = slot.id;
        tokio::spawn(async move {
            retranscribe(&state, id, clips).await;
            if let Err(e) = extract(&state, id).await {
                tracing::warn!(call = %id, error = %e, "lead extraction failed");
            }
            if state.config.llm.review_calls {
                if let Err(e) = super::training::review(&state, DEFAULT_TENANT, id, None).await {
                    tracing::warn!(call = %id, error = %e, "call review failed");
                }
            }
        });
    }
}

/// Whether the whole call was in English: the call's language and every
/// caller turn's language are English, and the caller's text has no Indian
/// script. Code-mixed speech is sometimes labelled English but is written
/// partly in its own script, so the script check catches it.
fn english_only(outcome: &CallOutcome) -> bool {
    let english = |l: &str| l.starts_with("en");
    outcome.detected_language.as_deref().is_none_or(english)
        && outcome
            .turns
            .iter()
            .filter(|t| t.role == pipeline::Speaker::User)
            .all(|t| t.language.as_deref().is_none_or(english) && !has_indian_script(&t.text))
}

/// Shortest caller clip worth re-transcribing (0.3 s of 8 kHz μ-law).
const MIN_CLIP_BYTES: usize = 2_400;
/// Batch STT requests in flight per call.
const RETRANSCRIBE_CONCURRENCY: usize = 4;

/// The caller turns' audio by turn number, skipping clips that are too short
/// or too long for the batch endpoint.
fn caller_clips(outcome: &CallOutcome) -> Vec<(i32, bytes::Bytes)> {
    let max_bytes = (providers::sarvam::BATCH_MAX_SECONDS * f64::from(audio::SAMPLE_RATE)) as usize;
    outcome
        .turns
        .iter()
        .filter(|t| t.role == pipeline::Speaker::User)
        .filter_map(|t| t.audio.clone().map(|a| (t.seq, a)))
        .filter(|(_, a)| (MIN_CLIP_BYTES..=max_bytes).contains(&a.len()))
        .collect()
}

/// Re-transcribes caller turns with the batch STT (more accurate than the
/// realtime stream) and replaces the stored text, keeping the live text in
/// `live_text`. Failures keep the live text.
async fn retranscribe(state: &AppState, call: CallId, clips: Vec<(i32, bytes::Bytes)>) {
    use futures_util::StreamExt;

    let Some(key) = &state.config.sarvam.api_key else {
        return;
    };
    if clips.is_empty() || state.config.provider_mode == common::config::ProviderMode::Fake {
        return;
    }
    let config = providers::sarvam::SarvamBatchSttConfig {
        url: state.config.sarvam.stt_rest_url.clone(),
        model: state.config.sarvam.stt_rest_model.clone(),
        api_key: key.expose().to_owned(),
    };
    let results: Vec<_> = futures_util::stream::iter(clips)
        .map(|(seq, clip)| {
            let config = &config;
            async move {
                let seconds = clip.len() as f64 / f64::from(audio::SAMPLE_RATE);
                let wav = audio::wav::mulaw_wav(&clip);
                let result = providers::sarvam::transcribe_wav(&state.http, config, wav).await;
                (seq, seconds, result)
            }
        })
        .buffer_unordered(RETRANSCRIBE_CONCURRENCY)
        .collect()
        .await;

    let mut billed = 0.0;
    let mut changed = 0;
    for (seq, seconds, result) in results {
        match result {
            Ok(r) => {
                billed += seconds;
                let text = r.transcript.trim();
                if text.is_empty() {
                    continue;
                }
                let updated = sqlx::query(
                    "UPDATE call_turns SET live_text = text, text = $3 \
                     WHERE call_id = $1 AND seq = $2 AND text <> $3",
                )
                .bind(call.0)
                .bind(seq)
                .bind(text)
                .execute(&state.db)
                .await;
                match updated {
                    Ok(done) => changed += done.rows_affected(),
                    Err(e) => {
                        tracing::warn!(%call, seq, error = %e, "could not store re-transcript")
                    }
                }
            }
            Err(e) => {
                tracing::warn!(%call, seq, error = %e, "re-transcription failed; keeping live text")
            }
        }
    }
    let cost = state.assets.pricing.stt_cost(billed);
    if cost > 0.0 {
        record_cost("stt", "stt_retranscribe", cost);
        if let Err(e) = add_cost(state, call, "stt_retranscribe", cost).await {
            tracing::warn!(%call, error = %e, "could not add re-transcription cost");
        }
    }
    tracing::info!(%call, changed, billed_seconds = billed, cost_inr = cost, "caller turns re-transcribed");
}

async fn persist(state: &AppState, slot: &CallSlot, o: &CallOutcome) -> Result<(), AppError> {
    let pricing = &state.assets.pricing;
    let stt_cost = pricing.stt_cost(o.stt_seconds);
    record_cost("stt", "stt", stt_cost);
    let (mut tts_cost, mut llm_cost) = (0.0, 0.0);
    let mut tts_chars = 0u64;
    for (model, chars) in &o.tts_chars {
        tts_chars += chars;
        match pricing.tts_cost(model, *chars) {
            Some(c) => {
                tts_cost += c;
                record_cost("tts", model, c);
            }
            None => tracing::warn!(%model, "no TTS price in pricing.toml"),
        }
    }
    let mut llm_usage = serde_json::Map::new();
    for (model, usage) in &o.llm_usage {
        match pricing.llm_cost(model, *usage) {
            Some(c) => {
                llm_cost += c;
                record_cost("llm", model, c);
            }
            None => tracing::warn!(%model, "no LLM price in pricing.toml"),
        }
        llm_usage.insert(
            model.clone(),
            serde_json::to_value(usage).unwrap_or(Value::Null),
        );
    }

    // The estimated part of `llm` (streams cut short), shown for
    // transparency; it is already included in `llm`.
    let llm_estimated: f64 = o
        .llm_estimated
        .iter()
        .filter_map(|(model, usage)| pricing.llm_cost(model, *usage))
        .sum();

    let cost = stt_cost + tts_cost + llm_cost;
    let breakdown = serde_json::json!({
        "stt": stt_cost,
        "tts": tts_cost,
        "llm": llm_cost,
        "llm_estimated": llm_estimated,
        "tts_cached_chars": o.tts_cached_chars,
    });

    let mut tx = state.db.begin().await?;
    for t in &o.turns {
        sqlx::query(
            "INSERT INTO call_turns (id, tenant_id, call_id, seq, role, text, language, interrupted, started_at, ended_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) ON CONFLICT (call_id, seq) DO NOTHING",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(slot.tenant.0)
        .bind(slot.id.0)
        .bind(t.seq)
        .bind(t.role.as_str())
        .bind(&t.text)
        .bind(&t.language)
        .bind(t.interrupted)
        .bind(t.started_at)
        .bind(t.ended_at)
        .execute(&mut *tx)
        .await?;
    }
    for l in &o.latencies {
        sqlx::query(
            "INSERT INTO turn_latency (tenant_id, call_id, turn_seq, speech_end_ms, stt_final_ms, llm_first_token_ms, \
             tts_first_byte_ms, first_audio_sent_ms, spoken_chars, cap_triggered, llm_model, llm_attempts) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) ON CONFLICT (call_id, turn_seq) DO NOTHING",
        )
        .bind(slot.tenant.0)
        .bind(slot.id.0)
        .bind(l.turn_seq)
        .bind(l.speech_end_ms.map(|v| v as i32))
        .bind(l.stt_final_ms.map(|v| v as i32))
        .bind(l.llm_first_token_ms.map(|v| v as i32))
        .bind(l.tts_first_byte_ms.map(|v| v as i32))
        .bind(l.first_audio_sent_ms.map(|v| v as i32))
        .bind(l.spoken_chars)
        .bind(l.cap_triggered)
        .bind(&l.llm_model)
        .bind(l.llm_attempts)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "UPDATE calls SET \
           status = CASE WHEN status IN ('queued', 'ringing', 'in_progress') THEN 'completed' ELSE status END, \
           ended_at = COALESCE(ended_at, now()), \
           duration_secs = COALESCE(duration_secs, EXTRACT(EPOCH FROM (now() - answered_at))::int), \
           end_reason = COALESCE(end_reason, $2), \
           detected_language = COALESCE($3, detected_language), \
           stt_seconds = $4, tts_chars = $5, llm_usage = $6, \
           cost_inr = $7 + COALESCE((cost_breakdown->>'extraction')::float8, 0) \
             + COALESCE((cost_breakdown->>'stt_retranscribe')::float8, 0) \
             + COALESCE((cost_breakdown->>'tts_greeting')::float8, 0) \
             + COALESCE((cost_breakdown->>'review')::float8, 0), \
           cost_breakdown = cost_breakdown || $8 \
         WHERE id = $1",
    )
    .bind(slot.id.0)
    .bind(&o.end_reason)
    .bind(&o.detected_language)
    .bind(o.stt_seconds)
    .bind(tts_chars as i32)
    .bind(Value::Object(llm_usage))
    .bind(cost)
    .bind(breakdown)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let v2v: Vec<i64> = o
        .latencies
        .iter()
        .filter_map(|l| l.first_audio_sent_ms)
        .collect();
    tracing::info!(
        call = %slot.id,
        turns = o.turns.len(),
        end_reason = %o.end_reason,
        greeting_ms = ?o.greeting_latency_ms,
        v2v_p50_ms = ?percentile(&v2v, 50.0),
        v2v_p95_ms = ?percentile(&v2v, 95.0),
        stt_seconds = o.stt_seconds,
        tts_chars,
        tts_cached_chars = o.tts_cached_chars,
        cost_inr = cost,
        "call summary"
    );
    Ok(())
}

pub(crate) fn record_cost(service: &'static str, model: &str, inr: f64) {
    metrics::counter!(metric_names::COST_PAISE_TOTAL, "service" => service, "model" => model.to_owned())
        .increment((inr * 100.0).round().max(0.0) as u64);
}

/// Runs lead extraction for a finished call and stores the result.
pub async fn extract(state: &AppState, call: CallId) -> Result<(), AppError> {
    let turns: Vec<(String, String)> = sqlx::query_as(
        "SELECT role, text FROM call_turns WHERE call_id = $1 AND role <> 'tool' ORDER BY seq",
    )
    .bind(call.0)
    .fetch_all(&state.db)
    .await?;
    let fields: Vec<LeadField> = sqlx::query_as::<_, (sqlx::types::Json<Vec<LeadField>>,)>(
        "SELECT collect_fields FROM calls WHERE id = $1",
    )
    .bind(call.0)
    .fetch_optional(&state.db)
    .await?
    .map(|(f,)| f.0)
    .unwrap_or_default();
    let followups: Vec<(String,)> =
        sqlx::query_as("SELECT topic FROM call_followups WHERE call_id = $1 ORDER BY created_at")
            .bind(call.0)
            .fetch_all(&state.db)
            .await?;
    let transcript = turns
        .iter()
        .map(|(role, text)| {
            format!(
                "{}: {text}",
                if role == "user" { "Caller" } else { "Agent" }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut user = format!("TRANSCRIPT:\n{transcript}");
    if !followups.is_empty() {
        user.push_str("\n\nDETAILS THE CALLER ASKED TO RECEIVE:\n");
        for (t,) in &followups {
            user.push_str(&format!("- {t}\n"));
        }
    }
    let mut system = format!(
        "You extract structured lead data from a sales phone call transcript. Today is {} (India time); \
         turn relative dates such as \"tomorrow\" or \"next Saturday\" into YYYY-MM-DD. \
         Return only a JSON object with keys name (in English letters as normally written, never spelled out letter by letter), \
         interest_level (high|medium|low|none), budget, \
         preferred_language (BCP-47 such as en-IN, hi-IN, ta-IN: the language the caller spoke or asked for), follow_up_date (YYYY-MM-DD when a date is agreed), objections (array of strings) \
         and summary (2-3 sentences, English). Use null when unknown. Do not invent facts.",
        chrono::Utc::now()
            .with_timezone(&chrono_tz::Asia::Kolkata)
            .format("%A %Y-%m-%d")
    );
    if !fields.is_empty() {
        system.push_str(&agents::custom_instructions(&fields));
    }
    let endpoint = LlmEndpoint {
        url: state.config.llm.primary_url(),
        model: state.config.llm.extract_model.clone(),
    };
    let mut request = ChatRequest {
        messages: vec![
            ChatMessage::text(Role::System, system),
            ChatMessage::text(Role::User, user),
        ],
        tools: Vec::new(),
        // A short JSON object; reasoning tokens would only add cost.
        max_tokens: 600,
        temperature: 0.0,
        disable_reasoning: true,
        json_schema: Some(("lead_fields".to_owned(), extraction_schema(&fields))),
    };

    let mut last_error = String::new();
    for attempt in 0..2 {
        if attempt == 1 {
            // Retry without structured output in case the model rejects it.
            request.json_schema = None;
        }
        match state.providers.llm.complete(&endpoint, &request).await {
            Ok(completion) => {
                if let Some(c) = state
                    .assets
                    .pricing
                    .llm_cost(&endpoint.model, completion.usage)
                {
                    record_cost("llm_extract", &endpoint.model, c);
                    add_cost(state, call, "extraction", c).await?;
                }
                match parse_fields(&completion.content) {
                    Ok(mut lead) => {
                        lead.name = lead
                            .name
                            .map(|n| clean_spoken_name(&n))
                            .filter(|n| !n.is_empty());
                        lead.custom = (!fields.is_empty()).then(|| {
                            let raw = lead.custom.take().map(Value::Object);
                            agents::clean_custom(&fields, raw.as_ref())
                        });
                        sqlx::query("UPDATE calls SET extracted_fields = $2 WHERE id = $1")
                            .bind(call.0)
                            .bind(serde_json::to_value(&lead).unwrap_or(Value::Null))
                            .execute(&state.db)
                            .await?;
                        tracing::info!(%call, "lead fields extracted");
                        return Ok(());
                    }
                    Err(e) => last_error = format!("invalid JSON: {e}"),
                }
            }
            Err(e) => last_error = e.to_string(),
        }
    }
    Err(AppError::Internal(last_error))
}

/// Adds a post-call cost (`extraction`, `stt_retranscribe`) to the call's AI
/// cost and to that part of its breakdown.
pub(crate) async fn add_cost(
    state: &AppState,
    call: CallId,
    part: &'static str,
    inr: f64,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE calls SET cost_inr = cost_inr + $2, \
           cost_breakdown = jsonb_set(cost_breakdown, ARRAY[$3], \
             to_jsonb(COALESCE((cost_breakdown->>$3)::float8, 0) + $2)) \
         WHERE id = $1",
    )
    .bind(call.0)
    .bind(inr)
    .bind(part)
    .execute(&state.db)
    .await?;
    Ok(())
}

/// Reads the phone charge for a finished call from Plivo's call detail
/// record and stores it. The record appears a few seconds after hangup, so
/// this retries for about a minute.
pub async fn fetch_telephony_cost(state: AppState, call: CallId, call_uuid: String) {
    let Some(plivo) = state.plivo.clone() else {
        return;
    };
    for delay in [3u64, 7, 15, 35] {
        tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
        let detail = match plivo.call_detail(&call_uuid).await {
            Ok(Some(d)) => d,
            Ok(None) => continue,
            Err(e) => {
                tracing::debug!(%call, error = %e, "call detail not available yet");
                continue;
            }
        };
        let amount = detail
            .total_amount
            .as_deref()
            .and_then(|a| a.parse::<f64>().ok());
        let Some(raw) = amount else { continue };
        let rates = &state.assets.pricing.telephony;
        let amount = rates.plivo_to_inr(raw);
        let rate = detail
            .total_rate
            .as_deref()
            .and_then(|r| r.parse::<f64>().ok())
            .map(|r| rates.plivo_to_inr(r));
        let result = sqlx::query(
            "UPDATE calls SET telephony_cost_inr = $2, telephony_billed_secs = $3, \
             telephony_rate_inr_per_min = $4 WHERE id = $1",
        )
        .bind(call.0)
        .bind(amount)
        .bind(detail.billed_duration.map(|s| s as i32))
        .bind(rate)
        .execute(&state.db)
        .await;
        match result {
            Ok(_) => {
                record_cost("telephony", "plivo", amount);
                tracing::info!(%call, inr = amount, plivo_amount = raw, currency = %rates.plivo_currency, billed_secs = ?detail.billed_duration, "telephony cost stored");
            }
            Err(e) => tracing::warn!(%call, error = %e, "could not store telephony cost"),
        }
        return;
    }
    tracing::warn!(%call, "Plivo call record had no cost after 60 s");
}

/// Parses model output into [`ExtractedFields`], tolerating code fences
/// and text around the JSON object.
pub fn parse_fields(content: &str) -> Result<ExtractedFields, serde_json::Error> {
    let start = content.find('{').unwrap_or(0);
    let end = content.rfind('}').map_or(content.len(), |i| i + 1);
    serde_json::from_str(&content[start..end.max(start)])
}

/// Saves the call's recording if Plivo's recording callback never arrived:
/// after hangup, looks the recording up by call UUID and stores it.
pub async fn recording_fallback(state: AppState, call: CallId, call_uuid: String) {
    let Some(plivo) = state.plivo.clone() else {
        return;
    };
    for delay in [20u64, 40, 120] {
        tokio::time::sleep(Duration::from_secs(delay)).await;
        let stored: Result<Option<(Option<String>,)>, _> =
            sqlx::query_as("SELECT recording_key FROM calls WHERE id = $1")
                .bind(call.0)
                .fetch_optional(&state.db)
                .await;
        if matches!(stored, Ok(Some((Some(_),)))) {
            return;
        }
        match plivo.recordings_for_call(&call_uuid).await {
            Ok(list) => {
                if let Some(r) = list.into_iter().next() {
                    tracing::info!(%call, "recording callback missing; fetching recording from Plivo");
                    store_recording(
                        state,
                        DEFAULT_TENANT,
                        call,
                        r.recording_url,
                        Some(r.recording_id),
                    )
                    .await;
                    return;
                }
            }
            Err(e) => tracing::warn!(%call, error = %e, "could not list Plivo recordings"),
        }
    }
    tracing::warn!(%call, "Plivo has no recording for this call");
}

/// Downloads a Plivo recording into our storage, then deletes it from Plivo.
pub async fn store_recording(
    state: AppState,
    tenant: TenantId,
    call: CallId,
    record_url: String,
    recording_id: Option<String>,
) {
    let Some(plivo) = state.plivo.clone() else {
        return;
    };
    let mut audio = None;
    for attempt in 0..4u32 {
        match plivo.download(&record_url).await {
            Ok(bytes) if !bytes.is_empty() => {
                audio = Some(bytes);
                break;
            }
            Ok(_) => tracing::warn!(%call, "recording empty; retrying"),
            Err(e) => tracing::warn!(%call, error = %e, "recording download failed; retrying"),
        }
        tokio::time::sleep(Duration::from_secs(2u64.pow(attempt))).await;
    }
    let Some(audio) = audio else {
        tracing::error!(%call, "giving up on recording download");
        return;
    };
    let key = format!("tenants/{tenant}/calls/{call}.mp3");
    if let Err(e) = state.storage.put(&key, audio.to_vec(), "audio/mpeg").await {
        tracing::error!(%call, error = %e, "could not store recording");
        return;
    }
    if let Err(e) = sqlx::query("UPDATE calls SET recording_key = $2 WHERE id = $1")
        .bind(call.0)
        .bind(&key)
        .execute(&state.db)
        .await
    {
        tracing::error!(%call, error = %e, "could not save recording key");
        return;
    }
    if let Some(id) = recording_id {
        if let Err(e) = plivo.delete_recording(&id).await {
            tracing::warn!(%call, error = %e, "could not delete recording from Plivo");
        }
    }
    tracing::info!(%call, %key, bytes = audio.len(), "recording stored");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(detected: Option<&str>, caller: &[(&str, Option<&str>)]) -> CallOutcome {
        let turns = caller
            .iter()
            .enumerate()
            .map(|(i, (text, language))| pipeline::TurnRecord {
                seq: i as i32,
                role: pipeline::Speaker::User,
                text: (*text).to_owned(),
                language: language.map(str::to_owned),
                interrupted: false,
                started_at: chrono::Utc::now(),
                ended_at: None,
                audio: None,
            })
            .collect();
        CallOutcome {
            detected_language: detected.map(str::to_owned),
            turns,
            ..CallOutcome::default()
        }
    }

    #[test]
    fn only_non_english_calls_are_retranscribed() {
        assert!(english_only(&call(
            Some("en-IN"),
            &[("Friday at 2 PM works", Some("en-IN"))]
        )));
        assert!(english_only(&call(None, &[("yes", None)])));
        // The caller switched to Tamil.
        assert!(!english_only(&call(
            Some("ta-IN"),
            &[("hello", Some("en-IN"))]
        )));
        // Code-mixed speech labelled English but written partly in Tamil script.
        assert!(!english_only(&call(
            Some("en-IN"),
            &[("Friday ok, நாளைக்கு வரலாமா", Some("en-IN"))]
        )));
        assert_eq!("non_english".parse(), Ok(Retranscribe::NonEnglish));
        assert_eq!("true".parse(), Ok(Retranscribe::All));
        assert_eq!("off".parse(), Ok(Retranscribe::Off));
    }

    #[test]
    fn parses_fields_with_fences() {
        let raw = "```json\n{\"name\":\"Ravi\",\"interest_level\":\"high\",\"budget\":\"₹20,000/month\",\
                   \"preferred_language\":\"hi-IN\",\"follow_up_date\":null,\"objections\":[\"price\"],\
                   \"summary\":\"Wants ads.\"}\n```";
        let f = parse_fields(raw).unwrap();
        assert_eq!(f.name.as_deref(), Some("Ravi"));
        assert_eq!(f.interest_level, Some(InterestLevel::High));
        assert_eq!(f.objections, vec!["price"]);
        assert!(parse_fields("{\"summary\": 3}").is_err());
    }

    #[test]
    fn schema_lists_every_field() {
        let s = extraction_schema(&[]);
        assert_eq!(s["required"].as_array().map(Vec::len), Some(7));
        assert!(s["properties"].get("custom").is_none());
        let city: LeadField =
            serde_json::from_value(json!({"key": "city", "label": "City"})).unwrap();
        let s = extraction_schema(&[city]);
        assert_eq!(s["required"].as_array().map(Vec::len), Some(8));
        assert_eq!(s["properties"]["custom"]["required"], json!(["city"]));
    }
}
