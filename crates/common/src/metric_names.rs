//! Prometheus metric names shared by every crate, plus their help texts.
//!
//! Names ending in `_ms` get the latency histogram buckets configured in
//! the server.

use metrics::{describe_counter, describe_gauge, describe_histogram, Unit};

/// Counter: HTTP requests by `method`, `route`, `status`.
pub const HTTP_REQUESTS_TOTAL: &str = "http_requests_total";
/// Histogram: HTTP request latency by `method`, `route`.
pub const HTTP_REQUEST_DURATION_MS: &str = "http_request_duration_ms";
/// Gauge: calls with an open media stream.
pub const ACTIVE_CALLS: &str = "voice_active_calls";
/// Counter: finished calls by `outcome`.
pub const CALLS_TOTAL: &str = "voice_calls_total";
/// Histogram: per-turn stage offsets from caller speech end, by `stage` and `provider`.
/// Stages: `stt_final`, `llm_first_token`, `tts_first_audio`, `first_audio_sent`.
pub const TURN_STAGE_MS: &str = "voice_turn_stage_ms";
/// Histogram: stream start to first greeting audio frame sent.
pub const GREETING_LATENCY_MS: &str = "voice_greeting_latency_ms";
/// Histogram: characters spoken per assistant turn.
pub const SPOKEN_CHARS_PER_TURN: &str = "voice_spoken_chars_per_turn";
/// Counter: turns cut by the spoken-character cap.
pub const SPOKEN_CAP_TRIGGERED_TOTAL: &str = "voice_spoken_cap_triggered_total";
/// Counter: LLM retries by `model`.
pub const LLM_RETRIES_TOTAL: &str = "llm_retries_total";
/// Counter: turns answered by the fallback model.
pub const LLM_FALLBACKS_TOTAL: &str = "llm_fallbacks_total";
/// Counter: Sarvam errors by `service` (stt/tts/llm) and `code`.
pub const SARVAM_ERRORS_TOTAL: &str = "sarvam_errors_total";
/// Counter: billed STT audio, in milliseconds.
pub const STT_BILLED_MS_TOTAL: &str = "voice_stt_billed_ms_total";
/// Counter: TTS characters by `model`.
pub const TTS_CHARS_TOTAL: &str = "voice_tts_chars_total";
/// Counter: LLM tokens by `model` and `kind` (prompt/cached/completion).
pub const LLM_TOKENS_TOTAL: &str = "voice_llm_tokens_total";
/// Counter: estimated spend in paise by `service` and `model`.
pub const COST_PAISE_TOTAL: &str = "voice_cost_paise_total";

/// Registers help texts and units with the installed recorder.
pub fn describe_all() {
    describe_counter!(
        HTTP_REQUESTS_TOTAL,
        "HTTP requests by method, route and status"
    );
    describe_histogram!(
        HTTP_REQUEST_DURATION_MS,
        Unit::Milliseconds,
        "HTTP request latency"
    );
    describe_gauge!(ACTIVE_CALLS, "Calls with an open media stream");
    describe_counter!(CALLS_TOTAL, "Finished calls by outcome");
    describe_histogram!(
        TURN_STAGE_MS,
        Unit::Milliseconds,
        "Per-turn stage offset from caller speech end, by stage and provider"
    );
    describe_histogram!(
        GREETING_LATENCY_MS,
        Unit::Milliseconds,
        "Media stream start to first greeting audio sent"
    );
    describe_histogram!(
        SPOKEN_CHARS_PER_TURN,
        "Characters spoken per assistant turn"
    );
    describe_counter!(
        SPOKEN_CAP_TRIGGERED_TOTAL,
        "Assistant turns cut by MAX_SPOKEN_CHARS_PER_TURN"
    );
    describe_counter!(LLM_RETRIES_TOTAL, "LLM retries by model");
    describe_counter!(
        LLM_FALLBACKS_TOTAL,
        "Turns answered by the fallback LLM model"
    );
    describe_counter!(SARVAM_ERRORS_TOTAL, "Sarvam errors by service and code");
    describe_counter!(STT_BILLED_MS_TOTAL, Unit::Milliseconds, "Billed STT audio");
    describe_counter!(TTS_CHARS_TOTAL, "TTS characters by model");
    describe_counter!(LLM_TOKENS_TOTAL, "LLM tokens by model and kind");
    describe_counter!(
        COST_PAISE_TOTAL,
        "Estimated spend in paise by service and model"
    );
}
