//! Builds the application state from configuration.
//!
//! Missing optional pieces (Plivo or Sarvam keys, ONNX models) are logged
//! and the server still starts; the features that need them return
//! `provider_not_configured` until they are set up.

use std::{num::NonZeroU32, sync::Arc, time::Duration};

use common::{
    assets::Assets,
    config::{Config, ProviderMode},
    storage::Storage,
};
use governor::{Quota, RateLimiter};
use kb::{Embedder, KbService};
use metrics_exporter_prometheus::PrometheusHandle;
use pipeline::{
    vad::{EnergyFactory, SileroFactory, VadPool},
    PipelineSettings,
};
use providers::{
    fake::{FakeLlm, FakeStt, FakeTts},
    llm::LlmEndpoint,
    openai::OpenAiCompatLlm,
    sarvam::{SarvamStt, SarvamSttConfig, SarvamTts, SarvamTtsConfig},
};
use sqlx::PgPool;
use telephony::PlivoClient;

use crate::{
    lifecycle::Lifecycle,
    services::{lines::LineCache, registry::CallRegistry},
    state::{AppState, Providers},
};

/// Shared HTTP client: rustls, HTTP/2 where offered, pooled keep-alive connections.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .use_rustls_tls()
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(32)
        .tcp_keepalive(Duration::from_secs(30))
        .tcp_nodelay(true)
        .connect_timeout(Duration::from_secs(5))
        .http2_keep_alive_interval(Duration::from_secs(20))
        .http2_keep_alive_while_idle(true)
        .build()
        .unwrap_or_default()
}

fn providers(config: &Config, http: &reqwest::Client) -> Providers {
    let ms = Duration::from_millis;
    let f = config.fake;
    let fake = || Providers {
        stt: Arc::new(FakeStt {
            final_delay: ms(f.stt_final_ms),
            ..FakeStt::default()
        }),
        tts: Arc::new(FakeTts {
            first_byte_delay: ms(f.tts_first_byte_ms),
            audio_per_char: ms(f.tts_ms_per_char),
            ..FakeTts::default()
        }),
        llm: Arc::new(FakeLlm {
            first_token_delay: ms(f.llm_first_token_ms),
            ..FakeLlm::default()
        }),
        configured: config.provider_mode == ProviderMode::Fake,
    };
    match (config.provider_mode, &config.sarvam.api_key) {
        (ProviderMode::Fake, _) => {
            tracing::warn!("PROVIDER_MODE=fake: using fake STT, TTS and LLM");
            fake()
        }
        (ProviderMode::Sarvam, None) => fake(),
        (ProviderMode::Sarvam, Some(key)) => Providers {
            stt: Arc::new(SarvamStt::new(SarvamSttConfig {
                url: config.sarvam.stt_url.clone(),
                model: config.sarvam.stt_model.clone(),
                api_key: key.expose().to_owned(),
            })),
            tts: Arc::new(SarvamTts::new(SarvamTtsConfig {
                url: config.sarvam.tts_url.clone(),
                api_key: key.expose().to_owned(),
                codec: config.sarvam.tts_codec,
            })),
            llm: Arc::new(OpenAiCompatLlm::new(http.clone(), key.expose())),
            configured: true,
        },
    }
}

/// Pipeline tunables from config.
pub fn pipeline_settings(config: &Config) -> PipelineSettings {
    let p = &config.pipeline;
    PipelineSettings {
        vad_silence: Duration::from_millis(u64::from(p.vad_silence_ms)),
        vad_threshold: p.vad_threshold,
        barge_in_min: Duration::from_millis(u64::from(p.barge_in_min_ms)),
        barge_in_threshold: p.barge_in_vad_threshold,
        barge_in_talk: Duration::from_millis(u64::from(p.barge_in_talk_ms)),
        eager_llm: p.eager_llm,
        silence_reprompt: Duration::from_secs(u64::from(p.silence_reprompt_secs)),
        ack_wait: Duration::from_millis(u64::from(p.ack_wait_ms)),
        late_reply: Duration::from_millis(u64::from(p.late_reply_ms)),
        max_spoken_chars_per_turn: p.max_spoken_chars_per_turn as usize,
        max_sentences_per_turn: p.max_sentences_per_turn as usize,
        max_tts_chars_per_call: p.max_tts_chars_per_call as usize,
        llm_primary: LlmEndpoint {
            url: config.llm.primary_url(),
            model: config.llm.model.clone(),
        },
        llm_fallback: LlmEndpoint {
            url: config.llm.fallback_url(),
            model: config.llm.fallback_model.clone(),
        },
        llm_first_token_timeout: Duration::from_millis(config.llm.first_token_timeout_ms),
        llm_hedge_after: (config.llm.hedge_ms > 0)
            .then(|| Duration::from_millis(config.llm.hedge_ms)),
        llm_max_tokens: config.llm.max_tokens_live,
        tts_default_model: config.sarvam.tts_model.clone(),
        playout_lead: Duration::from_millis(u64::from(p.playout_lead_ms)),
        billing_pulse: Duration::from_secs(u64::from(p.billing_pulse_secs)),
        pulse_wrap_up: Duration::from_secs(u64::from(p.pulse_wrap_up_secs)),
    }
}

fn vad_pool(config: &Config) -> Option<Arc<VadPool>> {
    let path = config.paths.models_dir.join("silero_vad.onnx");
    let threads = config.paths.vad_threads;
    if path.exists() {
        match VadPool::new(&SileroFactory::new(&path), threads) {
            Ok(pool) => return Some(Arc::new(pool)),
            Err(e) => tracing::error!(error = %e, "could not load Silero VAD"),
        }
    } else {
        tracing::warn!(path = %path.display(), "Silero VAD model missing; run scripts/download_models.sh");
    }
    if config.provider_mode == ProviderMode::Fake {
        tracing::warn!("using the energy VAD (fake mode)");
        return VadPool::new(&EnergyFactory, threads).ok().map(Arc::new);
    }
    None
}

async fn embedder(config: &Config) -> Option<Arc<Embedder>> {
    let dir = config.paths.models_dir.join("bge-m3");
    if !dir.join("model_quantized.onnx").exists() {
        tracing::warn!(path = %dir.display(), "bge-m3 model missing; knowledge base disabled");
        return None;
    }
    let threads = config.paths.embed_threads;
    match tokio::task::spawn_blocking(move || Embedder::load(&dir, threads)).await {
        Ok(Ok(e)) => Some(Arc::new(e)),
        Ok(Err(e)) => {
            tracing::error!(error = %e, "could not load embedding model");
            None
        }
        Err(e) => {
            tracing::error!(error = %e, "embedding model loader panicked");
            None
        }
    }
}

/// Builds the full application state.
pub async fn build_state(
    config: Config,
    assets: Assets,
    db: PgPool,
    metrics: PrometheusHandle,
) -> AppState {
    let http = http_client();
    let storage = Storage::new(&config.s3);
    let plivo = match (&config.plivo.auth_id, &config.plivo.auth_token) {
        (Some(id), Some(token)) => Some(PlivoClient::new(
            http.clone(),
            id,
            token.expose(),
            "https://api.plivo.com",
        )),
        _ => None,
    };
    let providers = providers(&config, &http);
    let vad = vad_pool(&config);
    let kb = KbService::new(db.clone(), embedder(&config).await);
    let lines = Arc::new(LineCache::new(
        db.clone(),
        storage.clone(),
        providers.tts.clone(),
        assets.pricing.clone(),
    ));
    let per_minute = NonZeroU32::new(config.limits.calls_per_minute).unwrap_or(NonZeroU32::MIN);
    let pipeline = Arc::new(pipeline_settings(&config));
    let booking = match crate::services::booking::Booking::new(&assets.booking) {
        Ok(b) => Some(Arc::new(b)),
        Err(e) => {
            tracing::warn!(error = %e, "booking disabled");
            None
        }
    };

    AppState {
        config: Arc::new(config),
        assets: Arc::new(assets),
        db,
        storage,
        metrics,
        lifecycle: Lifecycle::default(),
        http,
        plivo,
        providers,
        vad,
        kb,
        calls: Arc::new(CallRegistry::default()),
        lines,
        pipeline,
        call_limiter: Arc::new(RateLimiter::direct(Quota::per_minute(per_minute))),
        booking,
    }
}

/// Background housekeeping: stale call slots, LLM connection keep-alive,
/// cached phrase audio and missing phone costs or recordings.
pub fn spawn_housekeeping(state: &AppState) {
    let s = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            let removed = s.calls.sweep(Duration::from_secs(15 * 60));
            if removed > 0 {
                tracing::info!(removed, "swept stale call slots");
            }
            close_abandoned_calls(&s).await;
        }
    });
    if state.plivo.is_some() {
        let s = state.clone();
        tokio::spawn(backfill_telephony_costs(s));
    }
    {
        let s = state.clone();
        let warm = s.providers.configured && s.config.provider_mode == ProviderMode::Sarvam;
        tokio::spawn(async move {
            s.lines.preload(common::ids::DEFAULT_TENANT).await;
            // After preload, so audio stored on earlier runs is not redone.
            if warm {
                crate::services::calls::warm_all_voices(&s).await;
            }
        });
    }
    if state.providers.configured && state.config.provider_mode == ProviderMode::Sarvam {
        let s = state.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(45));
            loop {
                tick.tick().await;
                crate::services::calls::warm_llm(&s).await;
            }
        });
    }
}

/// Fetches the Plivo charge and recording for recent answered calls that
/// have none, e.g. because the server restarted before Plivo published them.
/// Marks calls still "live" in the database long after any call could last
/// (the cap is one hour) as failed: their server stopped mid-call. Safe with
/// several servers on one database, since no live call is that old.
async fn close_abandoned_calls(state: &AppState) {
    let result = sqlx::query(
        "UPDATE calls SET status = 'failed', end_reason = COALESCE(end_reason, 'abandoned'), \
           ended_at = COALESCE(ended_at, now()) \
         WHERE status IN ('queued', 'ringing', 'in_progress') AND created_at < now() - interval '2 hours'",
    )
    .execute(&state.db)
    .await;
    match result {
        Ok(done) if done.rows_affected() > 0 => {
            tracing::info!(closed = done.rows_affected(), "closed abandoned calls");
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "could not close abandoned calls"),
    }
}

async fn backfill_telephony_costs(state: AppState) {
    let rows: Result<Vec<(uuid::Uuid, String)>, _> = sqlx::query_as(
        "SELECT id, plivo_call_uuid FROM calls WHERE telephony_cost_inr IS NULL \
         AND answered_at IS NOT NULL AND ended_at IS NOT NULL AND plivo_call_uuid IS NOT NULL \
         AND created_at > now() - interval '7 days'",
    )
    .fetch_all(&state.db)
    .await;
    match rows {
        Ok(rows) => {
            for (id, uuid) in rows {
                crate::services::postcall::fetch_telephony_cost(
                    state.clone(),
                    common::ids::CallId(id),
                    uuid,
                )
                .await;
            }
        }
        Err(e) => tracing::warn!(error = %e, "could not look up calls missing a phone cost"),
    }
    let rows: Result<Vec<(uuid::Uuid, String)>, _> = sqlx::query_as(
        "SELECT id, plivo_call_uuid FROM calls WHERE recording_key IS NULL \
         AND answered_at IS NOT NULL AND ended_at IS NOT NULL AND plivo_call_uuid IS NOT NULL \
         AND created_at > now() - interval '7 days'",
    )
    .fetch_all(&state.db)
    .await;
    match rows {
        Ok(rows) => {
            for (id, uuid) in rows {
                tokio::spawn(crate::services::postcall::recording_fallback(
                    state.clone(),
                    common::ids::CallId(id),
                    uuid,
                ));
            }
        }
        Err(e) => tracing::warn!(error = %e, "could not look up calls missing a recording"),
    }
}
