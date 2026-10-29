//! Call lifecycle: create → dial → (ring: prewarm) → answer → media stream → finalize.

use std::{
    sync::{atomic::Ordering, Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use audio::frame::Framer;
use axum::extract::ws::{Message, WebSocket};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use common::{
    ids::{CallId, KbId, TenantId, DEFAULT_TENANT},
    mask::mask_phone,
    AppError, ErrorCode,
};
use futures_util::{SinkExt, StreamExt};
use pipeline::{CallControl, CallParams, Deps, MediaIn, MediaOut, Prewarmed, ToolBackend};
use providers::stt::SttOptions;
use serde::Deserialize;
use telephony::{
    client::MakeCall,
    stream::{Inbound, Outbound},
};
use tokio::sync::{mpsc, watch};

use super::{
    booking::{BookOutcome, Booking},
    postcall,
    registry::CallSlot,
    util::{check_call_hours, is_e164, is_language, stream_token},
};
use crate::state::AppState;

/// `POST /v1/calls` body.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateCall {
    /// Destination, E.164.
    pub to: String,
    /// Lead's name.
    pub lead_name: String,
    /// Persona name (`prompts/personas/<name>.txt`): who the agent is and
    /// what the call is for.
    #[serde(default)]
    pub persona: Option<String>,
    /// Extra per-call instructions, added after the persona. Required when
    /// no persona is given.
    #[serde(default)]
    pub system_prompt: String,
    /// Knowledge base to search.
    #[serde(default)]
    pub knowledge_base_id: Option<KbId>,
    /// `auto` or BCP-47.
    #[serde(default)]
    pub language: Option<String>,
    /// TTS speaker override.
    #[serde(default)]
    pub voice: Option<String>,
    /// Live LLM for this call (a model in `config/pricing.toml`, e.g. `gemma4`).
    #[serde(default)]
    pub llm_model: Option<String>,
    /// TTS model override (a model in `config/pricing.toml`, e.g. `bulbul:v3`).
    /// Needs `voice`, since speaker names differ between models.
    #[serde(default)]
    pub tts_model: Option<String>,
    /// Short greeting after the fixed disclosure line (at most 100
    /// characters). The disclosure itself cannot be changed per call.
    #[serde(default)]
    pub greeting: Option<String>,
    /// Call length cap.
    #[serde(default)]
    pub max_duration_secs: Option<u32>,
    /// Lets the agent check and book appointments (`config/booking.toml`).
    #[serde(default)]
    pub booking: bool,
}

/// Validates the request, prepares greeting audio and knowledge, stores
/// the call and registers it. Simulated calls skip Plivo and call hours.
pub async fn create(
    state: &AppState,
    req: CreateCall,
    simulated: bool,
) -> Result<Arc<CallSlot>, AppError> {
    let tenant = DEFAULT_TENANT;
    let cfg = &state.config;
    if state.lifecycle.is_draining() {
        return Err(AppError::new(
            ErrorCode::Unavailable,
            "server is shutting down",
        ));
    }
    if !simulated && !is_e164(&req.to) {
        return Err(AppError::bad_request(
            "`to` must be an E.164 number like +919876543210",
        ));
    }
    if req.lead_name.trim().is_empty() || req.lead_name.chars().count() > 100 {
        return Err(AppError::bad_request(
            "`lead_name` must be 1-100 characters",
        ));
    }
    if req.system_prompt.len() > 20_000 {
        return Err(AppError::bad_request(
            "`system_prompt` must be at most 20000 bytes",
        ));
    }
    let persona = match req.persona.as_deref().map(str::trim) {
        Some(name) => Some(state.assets.personas.get(name).ok_or_else(|| {
            let mut known: Vec<&str> = state.assets.personas.keys().map(String::as_str).collect();
            known.sort_unstable();
            AppError::bad_request(format!(
                "unknown `persona` `{name}`; available: {}",
                known.join(", ")
            ))
        })?),
        None => None,
    };
    if persona.is_none() && req.system_prompt.trim().is_empty() {
        return Err(AppError::bad_request(
            "give a `persona` or a `system_prompt`",
        ));
    }
    if let Some(model) = &req.llm_model {
        if !state.assets.pricing.llm.contains_key(model) {
            let mut known: Vec<&str> = state
                .assets
                .pricing
                .llm
                .keys()
                .map(String::as_str)
                .collect();
            known.sort_unstable();
            return Err(AppError::bad_request(format!(
                "unknown `llm_model` `{model}`; available: {}",
                known.join(", ")
            )));
        }
    }
    if let Some(model) = &req.tts_model {
        if !state.assets.pricing.tts.contains_key(model) {
            let mut known: Vec<&str> = state
                .assets
                .pricing
                .tts
                .keys()
                .map(String::as_str)
                .collect();
            known.sort_unstable();
            return Err(AppError::bad_request(format!(
                "unknown `tts_model` `{model}`; available: {}",
                known.join(", ")
            )));
        }
        if req.voice.as_deref().is_none_or(|v| v.trim().is_empty()) {
            return Err(AppError::bad_request(
                "`tts_model` needs `voice` (speaker names differ between models)",
            ));
        }
    }
    if let Some(g) = &req.greeting {
        check_greeting(g)?;
    }
    if req.booking && state.booking.is_none() {
        return Err(AppError::bad_request(
            "`booking` needs a valid config/booking.toml",
        ));
    }
    // The persona, then this call's extra instructions.
    let system_prompt = [persona.map(String::as_str), Some(req.system_prompt.trim())]
        .into_iter()
        .flatten()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if req
        .greeting
        .as_ref()
        .is_some_and(|g| g.chars().count() > 500)
    {
        return Err(AppError::bad_request(
            "`greeting` must be at most 500 characters",
        ));
    }
    let language = req
        .language
        .clone()
        .unwrap_or_else(|| cfg.pipeline.default_language.clone());
    if !is_language(&language) {
        return Err(AppError::bad_request(
            "`language` must be `auto` or a BCP-47 code like hi-IN",
        ));
    }
    let max_duration = req
        .max_duration_secs
        .unwrap_or(cfg.pipeline.default_max_duration_secs);
    if !(10..=3600).contains(&max_duration) {
        return Err(AppError::bad_request(
            "`max_duration_secs` must be between 10 and 3600",
        ));
    }
    if !simulated {
        check_call_hours(&cfg.call_hours)?;
        if state.plivo.is_none() || cfg.plivo.from_number.is_none() {
            return Err(AppError::new(
                ErrorCode::ProviderNotConfigured,
                "Plivo is not configured (PLIVO_AUTH_ID, PLIVO_AUTH_TOKEN, PLIVO_FROM_NUMBER)",
            ));
        }
    }
    if !state.providers.configured {
        return Err(AppError::new(
            ErrorCode::ProviderNotConfigured,
            "SARVAM_API_KEY is not configured",
        ));
    }
    if state.vad.is_none() {
        return Err(AppError::new(
            ErrorCode::ProviderNotConfigured,
            "VAD model missing: run scripts/download_models.sh",
        ));
    }
    if let Some(kb) = req.knowledge_base_id {
        let exists: Option<(uuid::Uuid,)> =
            sqlx::query_as("SELECT id FROM knowledge_bases WHERE id = $1 AND tenant_id = $2")
                .bind(kb.0)
                .bind(tenant.0)
                .fetch_optional(&state.db)
                .await?;
        if exists.is_none() {
            return Err(AppError::not_found("knowledge base not found"));
        }
    }

    let fixed_language = (language != "auto").then(|| language.clone());
    let initial_language = fixed_language.clone().unwrap_or_else(|| {
        if cfg.pipeline.default_language != "auto" {
            cfg.pipeline.default_language.clone()
        } else {
            state.assets.voices.fallback_language.clone()
        }
    });
    let mut voice = state.assets.voices.resolve(
        &initial_language,
        &cfg.sarvam.tts_model,
        req.voice.as_deref(),
    );
    if let Some(model) = &req.tts_model {
        voice.model = model.clone();
    }
    let lines = state.assets.lines(&initial_language).clone();
    let greeting_body = req
        .greeting
        .clone()
        .filter(|g| !g.trim().is_empty())
        .unwrap_or_else(|| {
            lines
                .default_greeting
                .replace("{name}", req.lead_name.trim())
        });
    // The fixed disclosure always comes first; the request cannot replace it.
    let greeting_text = format!("{} {}", lines.disclosure, greeting_body.trim());

    let call_id = CallId::new();
    let kb_query: String = format!("{} {}", req.lead_name, system_prompt)
        .chars()
        .take(1000)
        .collect();
    // Two clips: the disclosure is the same on every call for this voice (so
    // it is cached once) and always plays in full; the body can be interrupted.
    let (disclosure_audio, greeting_audio, kb_context) = tokio::join!(
        state
            .lines
            .get_or_synthesize(tenant, &voice, &lines.disclosure),
        state
            .lines
            .get_or_synthesize(tenant, &voice, greeting_body.trim()),
        async {
            match req.knowledge_base_id {
                Some(kb) if state.kb.available() => state.kb.search(kb, &kb_query, 3).await.ok(),
                _ => None,
            }
        }
    );
    let cached = |clip: Result<bytes::Bytes, AppError>, what: &str| match clip {
        Ok(a) => Some(a),
        Err(e) => {
            tracing::warn!(call = %call_id, error = %e, "{what} not cached; will synthesize live");
            None
        }
    };
    let disclosure_audio = cached(disclosure_audio, "disclosure");
    let greeting_audio = cached(greeting_audio, "greeting");
    let kb_context = kb_context.filter(|h| !h.is_empty()).map(|hits| {
        hits.iter()
            .map(|h| format!("- {}", h.content.replace('\n', " ")))
            .collect::<Vec<_>>()
            .join("\n")
    });

    sqlx::query(
        "INSERT INTO calls (id, tenant_id, to_number, lead_name, system_prompt, kb_id, language, voice, greeting, max_duration_secs, status) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'queued')",
    )
    .bind(call_id.0)
    .bind(tenant.0)
    .bind(&req.to)
    .bind(req.lead_name.trim())
    .bind(&system_prompt)
    .bind(req.knowledge_base_id.map(|k| k.0))
    .bind(&language)
    .bind(&req.voice)
    .bind(&greeting_text)
    .bind(max_duration as i32)
    .execute(&state.db)
    .await?;

    let slot = Arc::new(CallSlot {
        id: call_id,
        tenant,
        kb_id: req.knowledge_base_id,
        params: CallParams {
            call_id: call_id.to_string(),
            lead_name: req.lead_name.trim().to_owned(),
            system_prompt: system_prompt.clone(),
            has_kb: req.knowledge_base_id.is_some() && state.kb.available(),
            has_booking: req.booking,
            today: match (&state.booking, req.booking) {
                (Some(b), true) => b.calendar_line(chrono::Utc::now()),
                _ => String::new(),
            },
            kb_context,
            fixed_language,
            initial_language,
            voice_override: req.voice.clone(),
            tts_model: req.tts_model.clone(),
            llm_primary: req.llm_model.as_ref().map(|m| providers::llm::LlmEndpoint {
                url: cfg.llm.url_for(m),
                model: m.clone(),
            }),
            greeting_text,
            disclosure_text: lines.disclosure.clone(),
            disclosure_audio,
            greeting_audio,
            max_duration: Duration::from_secs(u64::from(max_duration)),
        },
        max_duration_secs: max_duration,
        simulated,
        created: Instant::now(),
        plivo_uuid: Mutex::new(None),
        request_uuid: Mutex::new(None),
        prewarmed: tokio::sync::Mutex::new(None),
        commands: Mutex::new(None),
        streaming: std::sync::atomic::AtomicBool::new(false),
    });
    state.calls.insert(slot.clone());

    // Cache the fixed lines and common phrases in the background: this
    // call's voice first, then every other language's voice, so a call that
    // switches language still plays them instantly. Only missing audio is
    // synthesized (once, then it is stored).
    {
        let state = state.clone();
        let default_model = cfg.sarvam.tts_model.clone();
        tokio::spawn(async move {
            state
                .lines
                .warm(tenant, &voice, line_texts(&state, &voice.language))
                .await;
            let others: Vec<String> = state
                .assets
                .voices
                .languages
                .keys()
                .filter(|l| **l != voice.language)
                .cloned()
                .collect();
            for language in others {
                let other = state.assets.voices.resolve(&language, &default_model, None);
                let mut texts = line_texts(&state, &language);
                texts.extend(
                    state
                        .assets
                        .voices
                        .intro(&language, &other)
                        .map(str::to_owned),
                );
                state.lines.warm(tenant, &other, texts).await;
            }
        });
    }

    if !simulated {
        if let Err(e) = dial(state, &slot, &req.to).await {
            state.calls.remove(call_id);
            sqlx::query("UPDATE calls SET status = 'failed', end_reason = $2, ended_at = now() WHERE id = $1")
                .bind(call_id.0)
                .bind(format!("dial_failed: {}", e.public_message()))
                .execute(&state.db)
                .await?;
            return Err(e);
        }
    }
    Ok(slot)
}

async fn dial(state: &AppState, slot: &CallSlot, to: &str) -> Result<(), AppError> {
    let (Some(plivo), Some(from)) = (&state.plivo, &state.config.plivo.from_number) else {
        return Err(AppError::new(
            ErrorCode::ProviderNotConfigured,
            "Plivo is not configured",
        ));
    };
    let base = &state.config.public_base_url;
    let id = slot.id;
    let answer = format!("{base}/plivo/answer/{id}");
    let ring = format!("{base}/plivo/ring/{id}");
    let hangup = format!("{base}/plivo/hangup/{id}");
    let resp = plivo
        .make_call(&MakeCall {
            from,
            to,
            answer_url: &answer,
            answer_method: "POST",
            ring_url: &ring,
            ring_method: "POST",
            hangup_url: &hangup,
            hangup_method: "POST",
            time_limit: slot.max_duration_secs + 30,
            ring_timeout: 45,
        })
        .await
        .map_err(|e| {
            tracing::error!(call = %id, error = %e, "plivo make_call failed");
            AppError::new(ErrorCode::ProviderError, "could not place the call")
        })?;
    tracing::info!(call = %id, to = %mask_phone(to), request_uuid = %resp.request_uuid, "call placed");
    slot.set_request_uuid(&resp.request_uuid);
    sqlx::query("UPDATE calls SET started_at = now() WHERE id = $1")
        .bind(id.0)
        .execute(&state.db)
        .await?;
    Ok(())
}

/// Opens STT and two TTS sessions while the phone rings, and touches the
/// LLM endpoint so its TLS connection is warm.
pub async fn prewarm(state: AppState, slot: Arc<CallSlot>) {
    if slot.streaming.load(Ordering::Relaxed) {
        return;
    }
    let started = Instant::now();
    let p = &state.providers;
    let mut voice = state.assets.voices.resolve(
        &slot.params.initial_language,
        &state.config.sarvam.tts_model,
        slot.params.voice_override.as_deref(),
    );
    if let Some(model) = &slot.params.tts_model {
        voice.model = model.clone();
    }
    let (stt, tts_a, tts_b, _) = tokio::join!(
        p.stt.open(SttOptions {
            language: slot.params.fixed_language.clone()
        }),
        p.tts.open(&voice),
        p.tts.open(&voice),
        warm_llm(&state),
    );
    let mut guard = slot.prewarmed.lock().await;
    if slot.streaming.load(Ordering::Relaxed) {
        return; // too late; the stream opened its own
    }
    let stt = stt
        .map_err(|e| tracing::warn!(call = %slot.id, error = %e, "prewarm stt failed"))
        .ok();
    let tts = match (tts_a, tts_b) {
        (Ok(a), b) => Some((a, b.ok())),
        (Err(e), Ok(b)) => {
            tracing::warn!(call = %slot.id, error = %e, "prewarm tts failed");
            Some((b, None))
        }
        (Err(e), Err(_)) => {
            tracing::warn!(call = %slot.id, error = %e, "prewarm tts failed");
            None
        }
    };
    *guard = Some(Prewarmed { stt, tts });
    tracing::info!(call = %slot.id, ms = started.elapsed().as_millis() as u64, "prewarmed");
}

/// Fixed lines and phrases worth caching for `language`. A language without
/// its own lines file gets none: its fallback lines would be in English.
fn line_texts(state: &AppState, language: &str) -> Vec<String> {
    if !state.assets.line_languages().any(|l| l == language) {
        return Vec::new();
    }
    let l = state.assets.lines(language).clone();
    let mut texts = vec![
        l.reprompt,
        l.silence_goodbye,
        l.goodbye,
        l.short_goodbye,
        l.one_moment,
        l.apology,
        l.send_on_whatsapp,
        l.budget_closing,
        l.max_duration_closing,
    ];
    texts.extend(l.checking);
    texts.extend(l.phrases);
    texts.retain(|t| !t.trim().is_empty());
    texts
}

/// Sends a cheap request to the LLM host so a pooled TLS connection is ready.
pub async fn warm_llm(state: &AppState) {
    let url = state.config.llm.primary_url();
    let _ = state
        .http
        .head(&url)
        .timeout(Duration::from_secs(3))
        .send()
        .await;
}

/// Longest greeting a request may add after the fixed disclosure line
/// (about 6 s of speech together with it).
const MAX_GREETING_CHARS: usize = 100;

/// A request's greeting must be short: the disclosure is added by the
/// system, so the greeting only introduces the caller's reason for calling.
fn check_greeting(greeting: &str) -> Result<(), AppError> {
    let chars = greeting.trim().chars().count();
    if chars > MAX_GREETING_CHARS {
        return Err(AppError::bad_request(format!(
            "`greeting` is {chars} characters; keep it under {MAX_GREETING_CHARS} \
             (one short intro and a question). The AI and recording notice is added automatically."
        )));
    }
    Ok(())
}

struct CallTools {
    state: AppState,
    tenant: TenantId,
    call: CallId,
    kb: Option<KbId>,
    booking: Option<Arc<Booking>>,
}

#[async_trait]
impl ToolBackend for CallTools {
    async fn search_knowledge(&self, query: &str) -> Result<Vec<String>, String> {
        let Some(kb) = self.kb else {
            return Err("no knowledge base for this call".into());
        };
        let hits = self
            .state
            .kb
            .search(kb, query, 3)
            .await
            .map_err(|e| e.to_string())?;
        Ok(hits.into_iter().map(|h| h.content).collect())
    }

    async fn send_details(&self, topic: &str) -> Result<(), String> {
        let topic: String = topic.chars().take(500).collect();
        sqlx::query(
            "INSERT INTO call_followups (id, tenant_id, call_id, topic) VALUES ($1, $2, $3, $4)",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(self.tenant.0)
        .bind(self.call.0)
        .bind(topic)
        .execute(&self.state.db)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    async fn check_availability(&self, date: &str, time: Option<&str>) -> Result<String, String> {
        let Some(booking) = &self.booking else {
            return Err("booking is not enabled for this call".into());
        };
        let reply = booking
            .check(&self.state.db, self.tenant, date, time, chrono::Utc::now())
            .await
            .map_err(|e| e.to_string())?;
        tracing::info!(call = %self.call, %date, ?time, %reply, "availability checked");
        Ok(reply)
    }

    async fn book_appointment(&self, date: &str, time: &str, name: &str) -> Result<String, String> {
        let Some(booking) = &self.booking else {
            return Err("booking is not enabled for this call".into());
        };
        let outcome = booking
            .book(
                &self.state.db,
                self.tenant,
                Some(self.call.0),
                (date, time, name),
                chrono::Utc::now(),
            )
            .await
            .map_err(|e| e.to_string())?;
        tracing::info!(call = %self.call, %date, %time, ?outcome, "booking attempted");
        Ok(match outcome {
            BookOutcome::Booked(s) | BookOutcome::Rejected(s) => s,
        })
    }
}

struct PlivoControl {
    state: AppState,
    slot: Arc<CallSlot>,
}

#[async_trait]
impl CallControl for PlivoControl {
    async fn hangup(&self) {
        if self.slot.simulated {
            return;
        }
        let (Some(plivo), Some(uuid)) = (&self.state.plivo, self.slot.plivo_uuid()) else {
            return;
        };
        if let Err(e) = plivo.hangup(&uuid).await {
            tracing::warn!(call = %self.slot.id, error = %e, "plivo hangup failed");
        }
    }
}

/// Runs the media WebSocket for a call until it ends, then finalizes it.
pub async fn run_media(state: AppState, slot: Arc<CallSlot>, socket: WebSocket) {
    if slot.streaming.swap(true, Ordering::SeqCst) {
        tracing::warn!(call = %slot.id, "second media stream rejected");
        return;
    }
    let _guard = state.lifecycle.call_started();
    let Some(vad) = state.vad.clone() else {
        return;
    };
    let prewarmed = slot.prewarmed.lock().await.take().unwrap_or_default();
    let (media_tx, media_rx) = mpsc::channel::<MediaIn>(512);
    let (out_tx, mut out_rx) = mpsc::channel::<MediaOut>(512);
    let (cmd_tx, cmd_rx) = mpsc::channel(4);
    *slot
        .commands
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(cmd_tx);
    let (stream_id_tx, stream_id_rx) = watch::channel::<Option<String>>(None);
    let (mut sink, mut stream) = socket.split();

    // Writer: pipeline output → Plivo JSON.
    let writer = tokio::spawn(async move {
        let mut b64 = String::with_capacity(256);
        while let Some(m) = out_rx.recv().await {
            let text = match m {
                MediaOut::Play(frame) => {
                    b64.clear();
                    STANDARD.encode_string(&frame, &mut b64);
                    Outbound::play_mulaw(&b64).to_json()
                }
                MediaOut::Checkpoint(name) => match stream_id_rx.borrow().clone() {
                    Some(sid) => Outbound::Checkpoint {
                        stream_id: &sid,
                        name: &name,
                    }
                    .to_json(),
                    None => continue,
                },
                MediaOut::Clear => match stream_id_rx.borrow().clone() {
                    Some(sid) => Outbound::ClearAudio { stream_id: &sid }.to_json(),
                    None => continue,
                },
                MediaOut::Close => {
                    let _ = sink.send(Message::Close(None)).await;
                    break;
                }
            };
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    // Reader: Plivo JSON → pipeline input.
    let reader_state = state.clone();
    let reader_slot = slot.clone();
    let reader = tokio::spawn(async move {
        let mut framer = Framer::new(audio::FRAME_SAMPLES);
        let mut frames = Vec::with_capacity(4);
        while let Some(Ok(msg)) = stream.next().await {
            let text = match msg {
                Message::Text(t) => t,
                Message::Close(_) => break,
                _ => continue,
            };
            let Ok(event) = serde_json::from_str::<Inbound>(text.as_str()) else {
                continue;
            };
            match event {
                Inbound::Start { start, .. } => {
                    let _ = stream_id_tx.send(Some(start.stream_id.clone()));
                    if reader_slot.plivo_uuid().is_none() && !reader_slot.simulated {
                        reader_slot.set_plivo_uuid(&start.call_id);
                    }
                    let _ = sqlx::query(
                        "UPDATE calls SET status = 'in_progress', answered_at = COALESCE(answered_at, now()) \
                         WHERE id = $1 AND status IN ('queued', 'ringing', 'in_progress')",
                    )
                    .bind(reader_slot.id.0)
                    .execute(&reader_state.db)
                    .await;
                    if media_tx.send(MediaIn::Start).await.is_err() {
                        break;
                    }
                }
                Inbound::Media { media } => {
                    let Ok(raw) = STANDARD.decode(media.payload.as_bytes()) else {
                        continue;
                    };
                    frames.clear();
                    framer.push(&raw, &mut frames);
                    for f in frames.drain(..) {
                        // Never block on a slow pipeline; dropping a frame beats stalling.
                        let _ = media_tx.try_send(MediaIn::Audio(f));
                    }
                }
                Inbound::PlayedStream { name } => {
                    let _ = media_tx.try_send(MediaIn::Played(name));
                }
                Inbound::Dtmf { dtmf } => {
                    let _ = media_tx.try_send(MediaIn::Dtmf(dtmf.digit));
                }
                Inbound::ClearedAudio {} => {}
            }
        }
    });

    let deps = Deps {
        stt: state.providers.stt.clone(),
        tts: state.providers.tts.clone(),
        llm: state.providers.llm.clone(),
        vad,
        assets: state.assets.clone(),
        lines: state.lines.clone(),
    };
    let tools = Arc::new(CallTools {
        state: state.clone(),
        tenant: slot.tenant,
        call: slot.id,
        kb: slot.kb_id,
        booking: if slot.params.has_booking {
            state.booking.clone()
        } else {
            None
        },
    });
    let control = Arc::new(PlivoControl {
        state: state.clone(),
        slot: slot.clone(),
    });
    let outcome = pipeline::run_call(
        slot.params.clone(),
        state.pipeline.clone(),
        deps,
        tools,
        control,
        prewarmed,
        media_rx,
        out_tx,
        cmd_rx,
        state.lifecycle.shutdown_token(),
    )
    .await;

    reader.abort();
    if tokio::time::timeout(Duration::from_secs(1), writer)
        .await
        .is_err()
    {
        tracing::debug!(call = %slot.id, "writer did not finish in time");
    }
    *slot
        .commands
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    postcall::finalize(&state, &slot, outcome).await;
}

/// The media WebSocket URL Plivo connects to (token included).
pub fn stream_url(state: &AppState, call: CallId) -> String {
    format!(
        "{}/plivo/stream/{call}?t={}",
        state.config.public_ws_base_url(),
        stream_token(state.config.api_key.expose(), call)
    )
}

#[cfg(test)]
mod tests {
    use super::check_greeting;

    #[test]
    fn long_greetings_are_refused() {
        assert!(check_greeting(
            "This is Priya from Star Smile Dental. Shall we book your check-up?"
        )
        .is_ok());
        // Counted in characters, so Indic scripts get the same room as English.
        assert!(check_greeting("ಸ್ಟಾರ್ ಸ್ಮೈಲ್ ಡೆಂಟಲ್ ಕ್ಲಿನಿಕ್‌ನಿಂದ ಕರೆ ಮಾಡುತ್ತಿದ್ದೇನೆ. ನಿಮ್ಮ ಚೆಕ್-ಅಪ್ ಬುಕ್ ಮಾಡೋಣವೇ?").is_ok());
        assert!(check_greeting(&"Hello and welcome, ".repeat(6)).is_err());
    }
}
