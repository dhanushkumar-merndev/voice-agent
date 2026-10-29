//! Shared application state handed to every handler.

use std::sync::Arc;

use common::{assets::Assets, storage::Storage, Config};
use governor::{clock::DefaultClock, state::InMemoryState, state::NotKeyed, RateLimiter};
use kb::KbService;
use metrics_exporter_prometheus::PrometheusHandle;
use pipeline::{vad::VadPool, PipelineSettings};
use providers::{llm::LlmProvider, stt::SttProvider, tts::TtsProvider};
use sqlx::PgPool;
use telephony::PlivoClient;

use crate::{
    lifecycle::Lifecycle,
    services::{booking::Booking, lines::LineCache, registry::CallRegistry},
};

/// Direct (unkeyed) rate limiter.
pub type Limiter = RateLimiter<NotKeyed, InMemoryState, DefaultClock>;

/// STT, TTS and LLM implementations chosen by `PROVIDER_MODE`.
#[derive(Clone)]
pub struct Providers {
    /// Speech-to-text.
    pub stt: Arc<dyn SttProvider>,
    /// Text-to-speech.
    pub tts: Arc<dyn TtsProvider>,
    /// Chat completions.
    pub llm: Arc<dyn LlmProvider>,
    /// `false` when credentials are missing (calls are refused).
    pub configured: bool,
}

/// Application state. Cheap to clone: every field is a handle.
#[derive(Clone)]
pub struct AppState {
    /// Loaded configuration.
    pub config: Arc<Config>,
    /// Prompts, fixed lines, voice map and pricing.
    pub assets: Arc<Assets>,
    /// Database pool.
    pub db: PgPool,
    /// Object storage.
    pub storage: Storage,
    /// Renders `/metrics`.
    pub metrics: PrometheusHandle,
    /// Draining flag and active-call accounting.
    pub lifecycle: Lifecycle,
    /// Shared HTTP client (pooled, keep-alive).
    pub http: reqwest::Client,
    /// Plivo client, when credentials are configured.
    pub plivo: Option<PlivoClient>,
    /// Speech and LLM providers.
    pub providers: Providers,
    /// VAD worker pool, when the model is available.
    pub vad: Option<Arc<VadPool>>,
    /// Knowledge base.
    pub kb: KbService,
    /// Calls in flight.
    pub calls: Arc<CallRegistry>,
    /// Cached audio for greetings and fixed lines.
    pub lines: Arc<LineCache>,
    /// Pipeline tunables.
    pub pipeline: Arc<PipelineSettings>,
    /// Rate limit for `POST /v1/calls`.
    pub call_limiter: Arc<Limiter>,
    /// Appointment slots (`config/booking.toml`); `None` if it is invalid.
    pub booking: Option<Arc<Booking>>,
}
