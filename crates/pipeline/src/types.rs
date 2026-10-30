//! Inputs, outputs and integration traits of the pipeline.

use std::{collections::HashMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use common::assets::{Assets, TokenUsage, Voice};
use providers::{
    llm::{LlmEndpoint, LlmProvider},
    stt::{SttProvider, SttSession},
    tts::{TtsProvider, TtsSession},
};

use crate::vad::VadPool;

/// Media from the telephony side (decoded by the WebSocket layer).
#[derive(Debug, Clone)]
pub enum MediaIn {
    /// The stream started.
    Start,
    /// 20 ms of 8 kHz μ-law caller audio.
    Audio(Bytes),
    /// Playback reached a checkpoint we set.
    Played(String),
    /// The caller pressed a key.
    Dtmf(String),
}

/// Media to the telephony side.
#[derive(Debug, Clone)]
pub enum MediaOut {
    /// 20 ms (or less) of 8 kHz μ-law audio to play.
    Play(Bytes),
    /// Mark the playback position.
    Checkpoint(String),
    /// Drop all queued audio.
    Clear,
    /// Close the stream.
    Close,
}

/// External control of a running call.
#[derive(Debug, Clone)]
pub enum CallCommand {
    /// End the call now (API hangup).
    Hangup,
}

/// Everything about one call that the pipeline needs.
#[derive(Debug, Clone)]
pub struct CallParams {
    /// Call ID (for logs).
    pub call_id: String,
    /// Lead's name.
    pub lead_name: String,
    /// Per-call system prompt (placed after the fixed speaking rules).
    pub system_prompt: String,
    /// Knowledge retrieved at call start, if any.
    pub kb_context: Option<String>,
    /// `true` when the call has a knowledge base (enables `search_knowledge`).
    pub has_kb: bool,
    /// `true` when the agent can check and book appointments.
    pub has_booking: bool,
    /// Today's date in the business's timezone, e.g. "Wednesday, 2026-10-28".
    pub today: String,
    /// `None` = auto-detect; otherwise a BCP-47 code.
    pub fixed_language: Option<String>,
    /// Language of the greeting and fixed lines until detection says otherwise.
    pub initial_language: String,
    /// Per-call speaker override.
    pub voice_override: Option<String>,
    /// Per-call live LLM (replaces `LLM_MODEL` for this call).
    pub llm_primary: Option<LlmEndpoint>,
    /// Per-call TTS model override (e.g. `bulbul:v3`); used with
    /// `voice_override`, whose speaker then stays across language switches.
    pub tts_model: Option<String>,
    /// Full greeting text (disclosure first), for the transcript and history.
    pub greeting_text: String,
    /// The fixed disclosure line that starts `greeting_text`. It always
    /// plays in full; the rest of the greeting can be interrupted. Empty:
    /// the whole greeting is one uninterruptible clip.
    pub disclosure_text: String,
    /// Pre-synthesized disclosure audio (μ-law), if cached.
    pub disclosure_audio: Option<Bytes>,
    /// Pre-synthesized audio (μ-law) of the greeting after the disclosure
    /// (the whole greeting when `disclosure_text` is empty), if cached.
    pub greeting_audio: Option<Bytes>,
    /// Maximum call duration.
    pub max_duration: Duration,
}

/// Tunables shared by all calls.
#[derive(Debug, Clone)]
pub struct PipelineSettings {
    /// Silence that ends a caller turn.
    pub vad_silence: Duration,
    /// Speech probability threshold.
    pub vad_threshold: f32,
    /// Caller speech needed to interrupt the agent.
    pub barge_in_min: Duration,
    /// Speech probability that counts towards an interruption.
    pub barge_in_threshold: f32,
    /// Clear caller speech that interrupts the agent.
    pub barge_in_talk: Duration,
    /// Start the LLM on a stable partial transcript.
    pub eager_llm: bool,
    /// Caller silence before a re-prompt.
    pub silence_reprompt: Duration,
    /// Extra wait before answering a caller who said only "yes" or "hmm".
    pub ack_wait: Duration,
    /// Play a cached "one sec" if the reply has not started this long after
    /// the caller stopped talking (zero = off).
    pub late_reply: Duration,
    /// Per-turn spoken character cap.
    pub max_spoken_chars_per_turn: usize,
    /// Per-turn sentence cap (0 = none).
    pub max_sentences_per_turn: usize,
    /// Per-call TTS character budget.
    pub max_tts_chars_per_call: usize,
    /// Primary live-call LLM.
    pub llm_primary: LlmEndpoint,
    /// Fallback live-call LLM.
    pub llm_fallback: LlmEndpoint,
    /// Deadline for the first token before falling back.
    pub llm_first_token_timeout: Duration,
    /// Re-send a request with no first token after this long (`None`: off).
    pub llm_hedge_after: Option<Duration>,
    /// `max_tokens` on live turns.
    pub llm_max_tokens: u32,
    /// Default TTS model (voices.toml may override per language).
    pub tts_default_model: String,
    /// Audio sent ahead of real time to absorb network jitter.
    pub playout_lead: Duration,
    /// Telephony billing block (zero = not billing-aware).
    pub billing_pulse: Duration,
    /// Time kept free at the end of a block for the goodbye.
    pub pulse_wrap_up: Duration,
}

/// Tools the agent can use, bound server-side to this call. The model
/// never sees or chooses IDs.
#[async_trait]
pub trait ToolBackend: Send + Sync {
    /// Top knowledge-base chunks for `query`.
    async fn search_knowledge(&self, query: &str) -> Result<Vec<String>, String>;
    /// Records a follow-up request ("send details about X").
    async fn send_details(&self, topic: &str) -> Result<(), String>;
    /// Free appointment times on `date` (`YYYY-MM-DD`), summarized for the
    /// model; with `time` (`HH:MM`), whether that time is free.
    async fn check_availability(&self, _date: &str, _time: Option<&str>) -> Result<String, String> {
        Err("booking is not enabled for this call".into())
    }
    /// Books `time` (`HH:MM`) on `date` for `name`; returns a confirmation
    /// or why it could not be booked (with alternatives).
    async fn book_appointment(
        &self,
        _date: &str,
        _time: &str,
        _name: &str,
    ) -> Result<String, String> {
        Err("booking is not enabled for this call".into())
    }
}

/// Telephony control for the running call.
#[async_trait]
pub trait CallControl: Send + Sync {
    /// Hangs up the phone call.
    async fn hangup(&self);
}

/// Cached audio for fixed lines (greeting, re-prompt, goodbye, ...).
pub trait LineAudio: Send + Sync {
    /// μ-law audio for `text` in `voice`, if cached in memory.
    fn get(&self, voice: &Voice, text: &str) -> Option<Bytes>;

    /// Offers freshly synthesized audio of a short reply clause for caching,
    /// so the next time it is said it plays without TTS. Default: ignored.
    fn learn(&self, _voice: &Voice, _text: &str, _audio: Bytes) {}
}

/// Providers and shared services used by every call.
#[derive(Clone)]
pub struct Deps {
    /// Speech-to-text.
    pub stt: Arc<dyn SttProvider>,
    /// Text-to-speech.
    pub tts: Arc<dyn TtsProvider>,
    /// Chat completions.
    pub llm: Arc<dyn LlmProvider>,
    /// Voice activity detection.
    pub vad: Arc<VadPool>,
    /// Prompts, lines, voices.
    pub assets: Arc<Assets>,
    /// Fixed-line audio cache.
    pub lines: Arc<dyn LineAudio>,
}

/// Connections opened before the call was answered.
#[derive(Debug, Default)]
pub struct Prewarmed {
    /// STT session.
    pub stt: Option<SttSession>,
    /// Active and spare TTS sessions for the initial voice.
    pub tts: Option<(TtsSession, Option<TtsSession>)>,
}

/// Who wrote a transcript turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speaker {
    /// The caller.
    User,
    /// The agent.
    Assistant,
    /// A tool call result.
    Tool,
}

impl Speaker {
    /// Database value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}

/// One transcript entry.
#[derive(Debug, Clone)]
pub struct TurnRecord {
    /// Order within the call.
    pub seq: i32,
    /// Author.
    pub role: Speaker,
    /// Text (for interrupted agent turns: what was actually played).
    pub text: String,
    /// Language, when known.
    pub language: Option<String>,
    /// Agent turn cut by barge-in.
    pub interrupted: bool,
    /// Start time.
    pub started_at: DateTime<Utc>,
    /// End time.
    pub ended_at: Option<DateTime<Utc>>,
    /// Caller turns: the 8 kHz μ-law audio sent to STT for this turn, for
    /// re-transcription after the call. `None` for agent and tool turns.
    pub audio: Option<Bytes>,
}

/// Latency marks for one agent turn, in milliseconds.
#[derive(Debug, Clone, Default)]
pub struct TurnLatency {
    /// Sequence of the agent turn.
    pub turn_seq: i32,
    /// When the caller stopped speaking, ms since the stream started.
    pub speech_end_ms: Option<i64>,
    /// STT final, offset from speech end.
    pub stt_final_ms: Option<i64>,
    /// First LLM token, offset from speech end.
    pub llm_first_token_ms: Option<i64>,
    /// First TTS audio byte, offset from speech end.
    pub tts_first_byte_ms: Option<i64>,
    /// First audio frame sent to telephony, offset from speech end.
    pub first_audio_sent_ms: Option<i64>,
    /// Characters spoken.
    pub spoken_chars: i32,
    /// Spoken cap triggered.
    pub cap_triggered: bool,
    /// Model that answered.
    pub llm_model: Option<String>,
    /// LLM attempts made (1 = no retry).
    pub llm_attempts: i16,
}

/// Result of a finished call.
#[derive(Debug, Clone, Default)]
pub struct CallOutcome {
    /// Transcript.
    pub turns: Vec<TurnRecord>,
    /// Per-turn latency.
    pub latencies: Vec<TurnLatency>,
    /// Language detected by STT.
    pub detected_language: Option<String>,
    /// Billed STT seconds (from the provider when available).
    pub stt_seconds: f64,
    /// TTS characters by model.
    pub tts_chars: HashMap<String, u64>,
    /// LLM token usage by model (reported plus estimated).
    pub llm_usage: HashMap<String, TokenUsage>,
    /// The part of `llm_usage` that was estimated: streams dropped before
    /// the provider reported their usage (barge-in, hedging, guardrails).
    pub llm_estimated: HashMap<String, TokenUsage>,
    /// Why the call ended.
    pub end_reason: String,
    /// Stream start → first greeting audio sent.
    pub greeting_latency_ms: Option<i64>,
    /// Reply characters played from cached audio instead of TTS (not billed).
    pub tts_cached_chars: u64,
}
