//! Typed configuration loaded once from environment variables at startup.
//!
//! Every environment variable is read here and nowhere else. Missing or
//! malformed values are collected and reported together, so a bad `.env`
//! fails fast with one readable message. Provider credentials (Plivo,
//! Sarvam) are optional at startup so the server can boot for local work;
//! features that need them return a `provider_not_configured` error.

use std::{fmt, net::SocketAddr, path::PathBuf, str::FromStr};

use chrono_tz::Tz;

use crate::secret::Secret;

/// Deployment environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppEnv {
    /// Developer machine: dev-only routes enabled, relaxed checks.
    Local,
    /// Any deployed environment (the VPS).
    Production,
}

impl FromStr for AppEnv {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "local" | "dev" | "development" => Ok(Self::Local),
            "production" | "prod" | "staging" => Ok(Self::Production),
            other => Err(format!(
                "unknown value `{other}` (expected local or production)"
            )),
        }
    }
}

/// Log output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// One JSON object per line (default; for log shipping).
    Json,
    /// Human-readable, for a local terminal.
    Pretty,
}

impl FromStr for LogFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "json" => Ok(Self::Json),
            "pretty" | "text" => Ok(Self::Pretty),
            other => Err(format!("unknown value `{other}` (expected json or pretty)")),
        }
    }
}

/// Which provider implementations to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderMode {
    /// Sarvam STT, TTS and LLM (production).
    Sarvam,
    /// Deterministic fakes (local pipeline testing without API keys).
    Fake,
}

impl FromStr for ProviderMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "sarvam" => Ok(Self::Sarvam),
            "fake" => Ok(Self::Fake),
            other => Err(format!("unknown value `{other}` (expected sarvam or fake)")),
        }
    }
}

/// Audio codec requested from the TTS WebSocket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtsCodec {
    /// 8 kHz WAV (PCM); we strip the header and μ-law-encode locally.
    Wav,
    /// μ-law straight from the provider (no local encoding).
    Mulaw,
}

impl TtsCodec {
    /// The provider's wire value for this codec.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Mulaw => "mulaw",
        }
    }
}

impl FromStr for TtsCodec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "wav" => Ok(Self::Wav),
            "mulaw" | "ulaw" => Ok(Self::Mulaw),
            other => Err(format!("unknown value `{other}` (expected wav or mulaw)")),
        }
    }
}

/// Which calls get their caller turns re-transcribed after the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retranscribe {
    /// Never.
    Off,
    /// Calls where the caller spoke or asked for a language other than
    /// English. Live STT handles English well; Indian languages and
    /// code-mixed speech are where it loses names and dates.
    NonEnglish,
    /// Every call.
    All,
}

impl FromStr for Retranscribe {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "off" | "false" | "0" | "no" => Ok(Self::Off),
            "non_english" | "non-english" => Ok(Self::NonEnglish),
            "all" | "true" | "1" | "yes" => Ok(Self::All),
            other => Err(format!(
                "unknown value `{other}` (expected non_english, all or off)"
            )),
        }
    }
}

/// Full application configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// `APP_ENV`.
    pub app_env: AppEnv,
    /// `BIND_ADDR`, the socket the HTTP server listens on.
    pub bind_addr: SocketAddr,
    /// `PUBLIC_BASE_URL`, the externally reachable base URL Plivo calls back.
    pub public_base_url: String,
    /// `API_KEY`, the static key required in `X-API-Key`.
    pub api_key: Secret,
    /// `LOG_FORMAT`.
    pub log_format: LogFormat,
    /// `PROVIDER_MODE` (`fake` is allowed only with `APP_ENV=local`).
    pub provider_mode: ProviderMode,
    /// Database settings.
    pub database: DatabaseConfig,
    /// Object storage settings.
    pub s3: S3Config,
    /// Plivo credentials.
    pub plivo: PlivoConfig,
    /// Sarvam STT/TTS settings.
    pub sarvam: SarvamConfig,
    /// LLM settings (Sarvam OpenAI-compatible API).
    pub llm: LlmConfig,
    /// Real-time pipeline tuning and guardrails.
    pub pipeline: PipelineConfig,
    /// Allowed calling window.
    pub call_hours: CallHoursConfig,
    /// Rate limits and shutdown timing.
    pub limits: LimitsConfig,
    /// Locations of on-disk assets.
    pub paths: PathsConfig,
}

/// PostgreSQL settings.
#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    /// `DATABASE_URL` (contains a password, hence secret).
    pub url: Secret,
    /// `DATABASE_MAX_CONNECTIONS`.
    pub max_connections: u32,
}

/// S3-compatible storage settings (MinIO locally, Tigris or AWS S3 on the VPS).
#[derive(Debug, Clone)]
pub struct S3Config {
    /// `S3_ENDPOINT`; `None` means the AWS default endpoint for the region.
    pub endpoint: Option<String>,
    /// `S3_BUCKET`.
    pub bucket: String,
    /// `S3_ACCESS_KEY`.
    pub access_key: Secret,
    /// `S3_SECRET_KEY`.
    pub secret_key: Secret,
    /// `S3_REGION`.
    pub region: String,
    /// `S3_FORCE_PATH_STYLE` (needed for MinIO).
    pub force_path_style: bool,
}

/// Plivo credentials. All optional so the server boots without them.
#[derive(Debug, Clone)]
pub struct PlivoConfig {
    /// `PLIVO_AUTH_ID`.
    pub auth_id: Option<String>,
    /// `PLIVO_AUTH_TOKEN`, also the key for webhook signatures.
    pub auth_token: Option<Secret>,
    /// `PLIVO_FROM_NUMBER`, the caller ID for outbound calls.
    pub from_number: Option<String>,
}

/// Sarvam speech settings. The same key is also used by the LLM client.
#[derive(Debug, Clone)]
pub struct SarvamConfig {
    /// `SARVAM_API_KEY`.
    pub api_key: Option<Secret>,
    /// `SARVAM_STT_URL`, the realtime streaming STT WebSocket.
    pub stt_url: String,
    /// `SARVAM_STT_MODEL`.
    pub stt_model: String,
    /// `SARVAM_TTS_URL`, the streaming TTS WebSocket.
    pub tts_url: String,
    /// `SARVAM_TTS_MODEL`, the default TTS model (overridable per language).
    pub tts_model: String,
    /// `SARVAM_TTS_CODEC`.
    pub tts_codec: TtsCodec,
    /// `RETRANSCRIBE_CALLER_TURNS` (`non_english`, `all` or `off`): after the
    /// call, re-transcribe each caller turn with the more accurate batch STT
    /// and store that as the transcript.
    pub retranscribe_caller_turns: Retranscribe,
    /// `SARVAM_STT_REST_URL`, the batch speech-to-text endpoint.
    pub stt_rest_url: String,
    /// `SARVAM_STT_REST_MODEL`, the batch STT model used for re-transcription.
    pub stt_rest_model: String,
}

/// LLM settings.
#[derive(Debug, Clone)]
pub struct LlmConfig {
    /// `LLM_BASE_URL`, e.g. `https://api.sarvam.ai/v1`.
    pub base_url: String,
    /// `LLM_MODEL`, used on live turns.
    pub model: String,
    /// `LLM_FALLBACK_MODEL`, used when the primary misses its deadline or errors.
    pub fallback_model: String,
    /// `LLM_FALLBACK_PATH`, absolute path on the same host as `LLM_BASE_URL`.
    pub fallback_path: String,
    /// `LLM_FIRST_TOKEN_TIMEOUT_MS`.
    pub first_token_timeout_ms: u64,
    /// `LLM_HEDGE_MS`: with no first token after this long, send the same
    /// request again and use whichever answers first (cuts slow outliers);
    /// 0 disables it.
    pub hedge_ms: u64,
    /// `LLM_MAX_TOKENS_LIVE`.
    pub max_tokens_live: u32,
    /// `EXTRACT_LLM_MODEL`, used for post-call extraction.
    pub extract_model: String,
}

impl LlmConfig {
    /// Chat completions URL for the primary model.
    pub fn primary_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    /// Chat completions URL for `model`: Sarvam's own models are on
    /// `base_url` (`/v1`), the open-weight ones (`gemma4`, `mimo-…`) on the
    /// fallback path (`/v2`).
    pub fn url_for(&self, model: &str) -> String {
        if model.starts_with("sarvam-") {
            self.primary_url()
        } else {
            self.fallback_url()
        }
    }

    /// Chat completions URL for the fallback model: `fallback_path` on the
    /// same scheme and host as `base_url`.
    pub fn fallback_url(&self) -> String {
        format!("{}{}", url_origin(&self.base_url), self.fallback_path)
    }
}

/// Real-time pipeline tuning and response-length guardrails.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// `DEFAULT_LANGUAGE`: `auto` or a BCP-47 code.
    pub default_language: String,
    /// `VAD_SILENCE_MS`: silence that ends a caller turn.
    pub vad_silence_ms: u32,
    /// `VAD_THRESHOLD`: speech probability threshold (0..1).
    pub vad_threshold: f32,
    /// `BARGE_IN_MIN_MS`: caller speech needed to interrupt the agent.
    pub barge_in_min_ms: u32,
    /// `EAGER_LLM`: start the LLM on a stable partial transcript.
    pub eager_llm: bool,
    /// `SILENCE_REPROMPT_SECS`: caller silence before a re-prompt.
    pub silence_reprompt_secs: u32,
    /// `ACK_WAIT_MS`: extra silence to wait for when the caller said only
    /// "yes", "so" or "hmm", which often comes before the real answer
    /// (0 = answer at once).
    pub ack_wait_ms: u32,
    /// `DEFAULT_MAX_DURATION_SECS`: call length cap when the request omits it.
    pub default_max_duration_secs: u32,
    /// `MAX_SPOKEN_CHARS_PER_TURN`.
    pub max_spoken_chars_per_turn: u32,
    /// `TELEPHONY_BILLING_PULSE_SECS`: Plivo bills calls in blocks of this
    /// many seconds (30 observed); the agent wraps up inside paid blocks.
    /// 0 disables billing-aware ending.
    pub billing_pulse_secs: u32,
    /// `PULSE_WRAP_UP_SECS`: seconds kept free at the end of a block for
    /// the goodbye.
    pub pulse_wrap_up_secs: u32,
    /// `MAX_SENTENCES_PER_TURN`: sentences per reply before it is cut
    /// (1–2 word acknowledgements don't count); 0 disables the limit.
    pub max_sentences_per_turn: u32,
    /// `MAX_TTS_CHARS_PER_CALL`.
    pub max_tts_chars_per_call: u32,
    /// `PLAYOUT_LEAD_MS`: audio sent ahead of real time to Plivo. More
    /// absorbs network jitter (choppy voice); barge-in still clears it.
    pub playout_lead_ms: u32,
}

/// Window in which outbound calls may be placed.
#[derive(Debug, Clone)]
pub struct CallHoursConfig {
    /// `CALL_HOURS_ENFORCED`.
    pub enforced: bool,
    /// `CALL_HOURS_START`, first allowed hour (inclusive, 0-23).
    pub start_hour: u32,
    /// `CALL_HOURS_END`, end hour (exclusive, 1-24).
    pub end_hour: u32,
    /// `CALL_HOURS_TZ`.
    pub timezone: Tz,
}

/// Rate limits and shutdown timing.
#[derive(Debug, Clone)]
pub struct LimitsConfig {
    /// `CALLS_RATE_LIMIT_PER_MIN`: `POST /v1/calls` requests per minute.
    pub calls_per_minute: u32,
    /// `SHUTDOWN_GRACE_SECS`: how long active calls may run after SIGTERM.
    pub shutdown_grace_secs: u64,
}

/// Locations of on-disk assets.
#[derive(Debug, Clone)]
pub struct PathsConfig {
    /// `PROMPTS_DIR`: holds `speaking_rules.txt` and `lines/*.toml`.
    pub prompts_dir: PathBuf,
    /// `CONFIG_DIR`: holds `voices.toml` and `pricing.toml`.
    pub config_dir: PathBuf,
    /// `MODELS_DIR`: ONNX models.
    pub models_dir: PathBuf,
    /// `VAD_THREADS`: VAD worker threads.
    pub vad_threads: usize,
    /// `EMBED_THREADS`: ONNX intra-op threads for embeddings.
    pub embed_threads: usize,
}

/// All configuration problems found while loading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub Vec<String>);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "invalid configuration:")?;
        for problem in &self.0 {
            writeln!(f, "  - {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// Loads configuration from the process environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Loads configuration from an arbitrary key lookup (used by tests).
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let mut env = Env {
            lookup: &lookup,
            errors: Vec::new(),
        };

        let config = Self {
            app_env: env.parse_or("APP_ENV", AppEnv::Local),
            bind_addr: env.parse_or("BIND_ADDR", SocketAddr::from(([0, 0, 0, 0], 8080))),
            public_base_url: env
                .required("PUBLIC_BASE_URL")
                .trim_end_matches('/')
                .to_owned(),
            api_key: Secret::new(env.required("API_KEY")),
            log_format: env.parse_or("LOG_FORMAT", LogFormat::Json),
            provider_mode: env.parse_or("PROVIDER_MODE", ProviderMode::Sarvam),
            database: DatabaseConfig {
                url: Secret::new(env.required("DATABASE_URL")),
                max_connections: env.parse_or("DATABASE_MAX_CONNECTIONS", 20),
            },
            s3: S3Config {
                endpoint: env.optional("S3_ENDPOINT"),
                bucket: env.required("S3_BUCKET"),
                access_key: Secret::new(env.required("S3_ACCESS_KEY")),
                secret_key: Secret::new(env.required("S3_SECRET_KEY")),
                region: env.string_or("S3_REGION", "ap-south-1"),
                force_path_style: env.flag_or("S3_FORCE_PATH_STYLE", true),
            },
            plivo: PlivoConfig {
                auth_id: env.optional("PLIVO_AUTH_ID"),
                auth_token: env.optional("PLIVO_AUTH_TOKEN").map(Secret::new),
                from_number: env.optional("PLIVO_FROM_NUMBER"),
            },
            sarvam: SarvamConfig {
                api_key: env.optional("SARVAM_API_KEY").map(Secret::new),
                stt_url: env.string_or(
                    "SARVAM_STT_URL",
                    "wss://api.sarvam.ai/speech-to-text-realtime/ws",
                ),
                stt_model: env.string_or("SARVAM_STT_MODEL", "saaras:v4"),
                tts_url: env.string_or("SARVAM_TTS_URL", "wss://api.sarvam.ai/text-to-speech/ws"),
                tts_model: env.string_or("SARVAM_TTS_MODEL", "bulbul:v4-flash"),
                tts_codec: env.parse_or("SARVAM_TTS_CODEC", TtsCodec::Wav),
                retranscribe_caller_turns: env
                    .parse_or("RETRANSCRIBE_CALLER_TURNS", Retranscribe::NonEnglish),
                stt_rest_url: env.string_or(
                    "SARVAM_STT_REST_URL",
                    "https://api.sarvam.ai/speech-to-text",
                ),
                stt_rest_model: env.string_or("SARVAM_STT_REST_MODEL", "saaras:v4"),
            },
            llm: LlmConfig {
                base_url: env
                    .string_or("LLM_BASE_URL", "https://api.sarvam.ai/v1")
                    .trim_end_matches('/')
                    .to_owned(),
                model: env.string_or("LLM_MODEL", "sarvam-105b-conversations"),
                fallback_model: env.string_or("LLM_FALLBACK_MODEL", "gemma4"),
                fallback_path: env.string_or("LLM_FALLBACK_PATH", "/v2/chat/completions"),
                first_token_timeout_ms: env.parse_or("LLM_FIRST_TOKEN_TIMEOUT_MS", 1500),
                hedge_ms: env.parse_or("LLM_HEDGE_MS", 600),
                max_tokens_live: env.parse_or("LLM_MAX_TOKENS_LIVE", 80),
                extract_model: env.string_or("EXTRACT_LLM_MODEL", "sarvam-105b"),
            },
            pipeline: PipelineConfig {
                default_language: env.string_or("DEFAULT_LANGUAGE", "auto"),
                vad_silence_ms: env.parse_or("VAD_SILENCE_MS", 450),
                vad_threshold: env.parse_or("VAD_THRESHOLD", 0.5),
                barge_in_min_ms: env.parse_or("BARGE_IN_MIN_MS", 300),
                eager_llm: env.flag_or("EAGER_LLM", false),
                silence_reprompt_secs: env.parse_or("SILENCE_REPROMPT_SECS", 7),
                ack_wait_ms: env.parse_or("ACK_WAIT_MS", 2000),
                default_max_duration_secs: env.parse_or("DEFAULT_MAX_DURATION_SECS", 300),
                max_spoken_chars_per_turn: env.parse_or("MAX_SPOKEN_CHARS_PER_TURN", 160),
                max_sentences_per_turn: env.parse_or("MAX_SENTENCES_PER_TURN", 3),
                billing_pulse_secs: env.parse_or("TELEPHONY_BILLING_PULSE_SECS", 30),
                pulse_wrap_up_secs: env.parse_or("PULSE_WRAP_UP_SECS", 5),
                playout_lead_ms: env.parse_or("PLAYOUT_LEAD_MS", 400),
                max_tts_chars_per_call: env.parse_or("MAX_TTS_CHARS_PER_CALL", 4000),
            },
            call_hours: CallHoursConfig {
                enforced: env.flag_or("CALL_HOURS_ENFORCED", true),
                start_hour: env.parse_or("CALL_HOURS_START", 9),
                end_hour: env.parse_or("CALL_HOURS_END", 21),
                timezone: env.parse_or("CALL_HOURS_TZ", chrono_tz::Asia::Kolkata),
            },
            limits: LimitsConfig {
                calls_per_minute: env.parse_or("CALLS_RATE_LIMIT_PER_MIN", 30),
                shutdown_grace_secs: env.parse_or("SHUTDOWN_GRACE_SECS", 120),
            },
            paths: PathsConfig {
                prompts_dir: PathBuf::from(env.string_or("PROMPTS_DIR", "prompts")),
                config_dir: PathBuf::from(env.string_or("CONFIG_DIR", "config")),
                models_dir: PathBuf::from(env.string_or("MODELS_DIR", "models")),
                vad_threads: env.parse_or("VAD_THREADS", default_threads(4)),
                embed_threads: env.parse_or("EMBED_THREADS", default_threads(2)),
            },
        };

        let mut errors = env.errors;
        errors.extend(config.validate());
        if errors.is_empty() {
            Ok(config)
        } else {
            Err(ConfigError(errors))
        }
    }

    /// `true` when running on a developer machine.
    pub fn is_local(&self) -> bool {
        self.app_env == AppEnv::Local
    }

    /// WebSocket base URL derived from `PUBLIC_BASE_URL` (`https` → `wss`).
    pub fn public_ws_base_url(&self) -> String {
        if let Some(rest) = self.public_base_url.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = self.public_base_url.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            self.public_base_url.clone()
        }
    }

    /// Cross-field and range checks. Returns a list of problems.
    fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let p = &self.pipeline;

        if !(self.public_base_url.starts_with("http://")
            || self.public_base_url.starts_with("https://"))
        {
            problems.push("PUBLIC_BASE_URL must start with http:// or https://".to_owned());
        }
        if p.vad_threshold.is_nan() || p.vad_threshold <= 0.0 || p.vad_threshold >= 1.0 {
            problems.push("VAD_THRESHOLD must be between 0 and 1 (exclusive)".to_owned());
        }
        if !(100..=3000).contains(&p.vad_silence_ms) {
            problems.push("VAD_SILENCE_MS must be between 100 and 3000".to_owned());
        }
        if p.ack_wait_ms > 6000 {
            problems.push("ACK_WAIT_MS must be at most 6000".to_owned());
        }
        if !(40..=2000).contains(&p.playout_lead_ms) {
            problems.push("PLAYOUT_LEAD_MS must be between 40 and 2000".to_owned());
        }
        if p.default_language != "auto" && !looks_like_bcp47(&p.default_language) {
            problems.push("DEFAULT_LANGUAGE must be `auto` or a BCP-47 code like hi-IN".to_owned());
        }
        if p.max_spoken_chars_per_turn == 0 || p.max_tts_chars_per_call == 0 {
            problems.push(
                "MAX_SPOKEN_CHARS_PER_TURN and MAX_TTS_CHARS_PER_CALL must be > 0".to_owned(),
            );
        }
        if !(10..=3600).contains(&p.default_max_duration_secs) {
            problems.push("DEFAULT_MAX_DURATION_SECS must be between 10 and 3600".to_owned());
        }
        let h = &self.call_hours;
        if h.start_hour > 23 || h.end_hour > 24 || h.start_hour >= h.end_hour {
            problems.push("CALL_HOURS_START/END must satisfy 0 <= start < end <= 24".to_owned());
        }
        if !self.llm.fallback_path.starts_with('/') {
            problems.push("LLM_FALLBACK_PATH must start with `/`".to_owned());
        }
        if self.llm.max_tokens_live == 0 {
            problems.push("LLM_MAX_TOKENS_LIVE must be > 0".to_owned());
        }
        if self.limits.calls_per_minute == 0 {
            problems.push("CALLS_RATE_LIMIT_PER_MIN must be > 0".to_owned());
        }
        if self.app_env == AppEnv::Production {
            if self.api_key.expose() == "change-me" || self.api_key.expose().len() < 16 {
                problems.push(
                    "API_KEY must be at least 16 characters and not `change-me` in production"
                        .to_owned(),
                );
            }
            if !self.public_base_url.starts_with("https://") {
                problems.push("PUBLIC_BASE_URL must use https:// in production".to_owned());
            }
            if self.provider_mode == ProviderMode::Fake {
                problems.push("PROVIDER_MODE=fake is only allowed with APP_ENV=local".to_owned());
            }
        }
        problems
    }
}

fn default_threads(max: usize) -> usize {
    std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .clamp(1, max)
}

/// Returns `scheme://host[:port]` of a URL (everything before the path).
fn url_origin(url: &str) -> &str {
    let after_scheme = url.find("://").map_or(0, |i| i + 3);
    match url[after_scheme..].find('/') {
        Some(slash) => &url[..after_scheme + slash],
        None => url,
    }
}

/// Loose shape check for codes like `hi-IN` or `kok-IN`.
fn looks_like_bcp47(code: &str) -> bool {
    let mut parts = code.split('-');
    let lang_ok = parts
        .next()
        .is_some_and(|l| (2..=3).contains(&l.len()) && l.chars().all(|c| c.is_ascii_lowercase()));
    let region_ok = parts
        .next()
        .is_some_and(|r| r.len() == 2 && r.chars().all(|c| c.is_ascii_uppercase()));
    lang_ok && region_ok && parts.next().is_none()
}

/// Reads variables through a lookup and records problems instead of failing
/// on the first one.
struct Env<'a> {
    lookup: &'a dyn Fn(&str) -> Option<String>,
    errors: Vec<String>,
}

impl Env<'_> {
    fn optional(&self, key: &str) -> Option<String> {
        (self.lookup)(key)
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    }

    fn required(&mut self, key: &str) -> String {
        match self.optional(key) {
            Some(v) => v,
            None => {
                self.errors.push(format!("{key} is required"));
                String::new()
            }
        }
    }

    fn string_or(&self, key: &str, default: &str) -> String {
        self.optional(key).unwrap_or_else(|| default.to_owned())
    }

    fn parse_or<T>(&mut self, key: &str, default: T) -> T
    where
        T: FromStr,
        T::Err: fmt::Display,
    {
        match self.optional(key) {
            None => default,
            Some(raw) => match raw.parse::<T>() {
                Ok(v) => v,
                Err(e) => {
                    self.errors
                        .push(format!("{key}: invalid value `{raw}`: {e}"));
                    default
                }
            },
        }
    }

    fn flag_or(&mut self, key: &str, default: bool) -> bool {
        match self.optional(key).map(|v| v.to_ascii_lowercase()) {
            None => default,
            Some(v) => match v.as_str() {
                "1" | "true" | "yes" | "on" => true,
                "0" | "false" | "no" | "off" => false,
                _ => {
                    self.errors
                        .push(format!("{key}: expected true or false, got `{v}`"));
                    default
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn base_env() -> HashMap<&'static str, &'static str> {
        HashMap::from([
            ("PUBLIC_BASE_URL", "https://voice.example.com/"),
            ("API_KEY", "change-me"),
            (
                "DATABASE_URL",
                "postgres://voice:voice@localhost:5432/voice",
            ),
            ("S3_BUCKET", "voice"),
            ("S3_ACCESS_KEY", "minio"),
            ("S3_SECRET_KEY", "minio123"),
        ])
    }

    fn load(env: &HashMap<&'static str, &'static str>) -> Result<Config, ConfigError> {
        Config::from_lookup(|k| env.get(k).map(|v| (*v).to_owned()))
    }

    #[test]
    fn defaults_apply() {
        let cfg = load(&base_env()).unwrap();
        assert_eq!(cfg.app_env, AppEnv::Local);
        assert_eq!(cfg.public_base_url, "https://voice.example.com");
        assert_eq!(cfg.pipeline.vad_silence_ms, 450);
        assert_eq!(cfg.pipeline.playout_lead_ms, 400);
        assert_eq!(cfg.llm.hedge_ms, 600);
        assert_eq!(cfg.llm.max_tokens_live, 80);
        assert_eq!(cfg.pipeline.max_spoken_chars_per_turn, 160);
        assert_eq!(cfg.pipeline.max_sentences_per_turn, 3);
        assert_eq!(cfg.sarvam.stt_model, "saaras:v4");
        assert_eq!(cfg.sarvam.tts_codec, TtsCodec::Wav);
        assert_eq!(
            cfg.sarvam.retranscribe_caller_turns,
            Retranscribe::NonEnglish
        );
        assert_eq!(cfg.sarvam.stt_rest_model, "saaras:v4");
        assert_eq!(cfg.call_hours.timezone, chrono_tz::Asia::Kolkata);
        assert!(cfg.plivo.auth_token.is_none());
    }

    #[test]
    fn missing_required_values_are_all_reported() {
        let err = load(&HashMap::new()).unwrap_err();
        for key in ["PUBLIC_BASE_URL", "API_KEY", "DATABASE_URL", "S3_BUCKET"] {
            assert!(
                err.0.iter().any(|e| e.contains(key)),
                "missing {key} in {err}"
            );
        }
    }

    #[test]
    fn invalid_values_are_reported_without_echoing_secrets() {
        let mut env = base_env();
        env.insert("VAD_THRESHOLD", "1.5");
        env.insert("EAGER_LLM", "maybe");
        env.insert("VAD_SILENCE_MS", "fast");
        let err = load(&env).unwrap_err().to_string();
        assert!(err.contains("VAD_THRESHOLD"));
        assert!(err.contains("EAGER_LLM"));
        assert!(err.contains("VAD_SILENCE_MS"));
        assert!(!err.contains("minio123"));
    }

    #[test]
    fn production_rejects_default_api_key_and_http() {
        let mut env = base_env();
        env.insert("APP_ENV", "production");
        env.insert("PUBLIC_BASE_URL", "http://voice.example.com");
        let err = load(&env).unwrap_err().to_string();
        assert!(err.contains("API_KEY"));
        assert!(err.contains("https://"));
    }

    #[test]
    fn llm_urls() {
        let cfg = load(&base_env()).unwrap();
        assert_eq!(
            cfg.llm.primary_url(),
            "https://api.sarvam.ai/v1/chat/completions"
        );
        assert_eq!(
            cfg.llm.fallback_url(),
            "https://api.sarvam.ai/v2/chat/completions"
        );
    }

    #[test]
    fn ws_base_url() {
        let cfg = load(&base_env()).unwrap();
        assert_eq!(cfg.public_ws_base_url(), "wss://voice.example.com");
    }

    #[test]
    fn origin_and_bcp47_helpers() {
        assert_eq!(
            url_origin("https://api.sarvam.ai/v1"),
            "https://api.sarvam.ai"
        );
        assert_eq!(url_origin("http://localhost:8080"), "http://localhost:8080");
        assert!(looks_like_bcp47("hi-IN"));
        assert!(looks_like_bcp47("kok-IN"));
        assert!(!looks_like_bcp47("hindi"));
        assert!(!looks_like_bcp47("hi-in"));
    }
}
