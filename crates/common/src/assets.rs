//! Static assets loaded once at startup:
//!
//! - `prompts/speaking_rules.txt`: the fixed guardrail block that is always
//!   placed before the per-call system prompt.
//! - `prompts/lines/<lang>.toml`: fixed lines per language (disclosure,
//!   re-prompt, goodbye, "one moment please", ...). `en-IN` is required and
//!   is the fallback for languages without a file.
//! - `config/voices.toml`: language → TTS model override and persona speaker.
//! - `config/pricing.toml`: provider rates in ₹ for per-call cost tracking.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;

/// Language whose fixed lines must exist and are used as the fallback.
pub const FALLBACK_LINES_LANGUAGE: &str = "en-IN";

/// Failure while loading assets.
#[derive(Debug, thiserror::Error)]
pub enum AssetError {
    /// A file could not be read.
    #[error("reading {path}: {source}")]
    Io {
        /// File that failed.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// A TOML file could not be parsed.
    #[error("parsing {path}: {source}")]
    Toml {
        /// File that failed.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: toml::de::Error,
    },
    /// The files parsed but are inconsistent.
    #[error("{0}")]
    Invalid(String),
}

/// All static assets.
#[derive(Debug, Clone)]
pub struct Assets {
    /// Fixed guardrail block placed before every per-call system prompt.
    pub speaking_rules: String,
    /// Reusable personas from `prompts/personas/<name>.txt`: who the agent
    /// is and what the call is for. Picked per call with `persona`.
    pub personas: HashMap<String, String>,
    /// Fixed lines by language code.
    lines: HashMap<String, FixedLines>,
    /// Language → voice mapping.
    pub voices: VoiceMap,
    /// Provider rates.
    pub pricing: Pricing,
    /// Appointment rules (`config/booking.toml`, optional).
    pub booking: BookingRules,
}

/// Opening hours and slot size for appointment booking.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BookingRules {
    /// IANA timezone the hours are in.
    pub timezone: String,
    /// First slot, `HH:MM`.
    pub open: String,
    /// End of the day, `HH:MM`; no slot starts at or after it.
    pub close: String,
    /// Slot length in minutes.
    pub slot_mins: u32,
    /// Open weekdays (`Mon`..`Sun`).
    pub days: Vec<String>,
    /// How far ahead bookings are accepted.
    pub max_days_ahead: u32,
    /// Today's slots closer than this are not offered.
    #[serde(default)]
    pub min_notice_mins: u32,
}

impl Default for BookingRules {
    fn default() -> Self {
        Self {
            timezone: "Asia/Kolkata".into(),
            open: "10:00".into(),
            close: "18:00".into(),
            slot_mins: 30,
            days: ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
                .map(String::from)
                .to_vec(),
            max_days_ahead: 30,
            min_notice_mins: 60,
        }
    }
}

impl Assets {
    /// Loads assets from the prompts and config directories.
    pub fn load(prompts_dir: &Path, config_dir: &Path) -> Result<Self, AssetError> {
        let rules_path = prompts_dir.join("speaking_rules.txt");
        let speaking_rules = read(&rules_path)?.trim().to_owned();
        if speaking_rules.is_empty() {
            return Err(AssetError::Invalid(format!(
                "{} is empty",
                rules_path.display()
            )));
        }

        let mut personas = HashMap::new();
        let personas_dir = prompts_dir.join("personas");
        if let Ok(entries) = fs::read_dir(&personas_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("txt") {
                    continue;
                }
                let Some(name) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned)
                else {
                    continue;
                };
                let text = read(&path)?.trim().to_owned();
                if !text.is_empty() {
                    personas.insert(name, text);
                }
            }
        }

        let lines_dir = prompts_dir.join("lines");
        let mut lines = HashMap::new();
        let entries = fs::read_dir(&lines_dir).map_err(|source| AssetError::Io {
            path: lines_dir.clone(),
            source,
        })?;
        for entry in entries {
            let path = entry
                .map_err(|source| AssetError::Io {
                    path: lines_dir.clone(),
                    source,
                })?
                .path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let Some(lang) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else {
                continue;
            };
            let parsed: FixedLines = parse_toml(&path)?;
            lines.insert(lang, parsed);
        }
        if !lines.contains_key(FALLBACK_LINES_LANGUAGE) {
            return Err(AssetError::Invalid(format!(
                "{}/{FALLBACK_LINES_LANGUAGE}.toml is required",
                lines_dir.display()
            )));
        }

        let voices: VoiceMap = parse_toml(&config_dir.join("voices.toml"))?;
        if !voices.languages.contains_key(&voices.fallback_language) {
            return Err(AssetError::Invalid(format!(
                "voices.toml: fallback_language `{}` has no [languages] entry",
                voices.fallback_language
            )));
        }
        let pricing: Pricing = parse_toml(&config_dir.join("pricing.toml"))?;
        let booking_path = config_dir.join("booking.toml");
        let booking: BookingRules = if booking_path.exists() {
            parse_toml(&booking_path)?
        } else {
            BookingRules::default()
        };

        Ok(Self {
            speaking_rules,
            personas,
            lines,
            voices,
            pricing,
            booking,
        })
    }

    /// Fixed lines for `language`, falling back to `en-IN`.
    pub fn lines(&self, language: &str) -> &FixedLines {
        self.lines
            .get(language)
            .or_else(|| self.lines.get(FALLBACK_LINES_LANGUAGE))
            .unwrap_or(&EMPTY_LINES)
    }

    /// Languages that have their own fixed-lines file.
    pub fn line_languages(&self) -> impl Iterator<Item = &str> {
        self.lines.keys().map(String::as_str)
    }
}

// `Assets::load` guarantees the `en-IN` fallback exists; this keeps
// `Assets::lines` total without `expect`.
static EMPTY_LINES: FixedLines = FixedLines {
    disclosure: String::new(),
    default_greeting: String::new(),
    reprompt: String::new(),
    silence_goodbye: String::new(),
    goodbye: String::new(),
    short_goodbye: String::new(),
    one_moment: String::new(),
    checking: Vec::new(),
    apology: String::new(),
    send_on_whatsapp: String::new(),
    budget_closing: String::new(),
    max_duration_closing: String::new(),
    phrases: Vec::new(),
};

/// Fixed lines spoken by the agent outside the LLM. All are cached as audio.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixedLines {
    /// First line of every call: AI assistant and recording disclosure.
    pub disclosure: String,
    /// Greeting used when the call request has none; `{name}` is the lead's name.
    pub default_greeting: String,
    /// Said once after the caller has been silent for a while.
    pub reprompt: String,
    /// Said before hanging up after a second silence.
    pub silence_goodbye: String,
    /// Normal goodbye (end_call tool).
    pub goodbye: String,
    /// Very short goodbye, used when the normal one would run into the next
    /// paid telephony block. Empty means always use `goodbye`.
    #[serde(default)]
    pub short_goodbye: String,
    /// Filler while the LLM is retried.
    pub one_moment: String,
    /// Fillers while a slow tool runs (availability, booking, knowledge),
    /// used in turn so the call doesn't repeat one. A single string is
    /// accepted too.
    #[serde(default, deserialize_with = "one_or_many")]
    pub checking: Vec<String>,
    /// Said when every LLM attempt failed.
    pub apology: String,
    /// Replaces URLs, emails and long numbers in replies.
    pub send_on_whatsapp: String,
    /// Said when the per-call TTS budget is used up, before hanging up.
    pub budget_closing: String,
    /// Said when the call reaches its maximum duration, before hanging up.
    pub max_duration_closing: String,
    /// Short phrases the LLM often opens a reply with ("Got it."). They are
    /// cached as audio, so a reply that starts with one plays instantly.
    #[serde(default)]
    pub phrases: Vec<String>,
}

/// Reads a string or a list of strings as a list.
fn one_or_many<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(d)? {
        OneOrMany::One(s) => vec![s],
        OneOrMany::Many(v) => v,
    })
}

/// Language → TTS voice mapping from `voices.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoiceMap {
    /// Language used when the caller's language is unknown or unmapped.
    pub fallback_language: String,
    /// Per-language voice entries keyed by BCP-47 code.
    pub languages: HashMap<String, VoiceEntry>,
}

/// One language's voice.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoiceEntry {
    /// TTS model override (e.g. `bulbul:v3` for Malayalam); `None` uses `SARVAM_TTS_MODEL`.
    pub model: Option<String>,
    /// Speaker ID valid for that model.
    pub speaker: String,
    /// Speaking rate, default 1.0.
    pub pace: Option<f32>,
    /// The agent's name while this voice speaks, for a voice that does not
    /// fit the persona's name (a male voice for "Priya"). `None` keeps the
    /// persona's name.
    pub name: Option<String>,
    /// Said once, in this language, when the call switches to this voice
    /// ("I'm Gokul, I'll take it from here"). Only used with `name`.
    pub intro: Option<String>,
    /// Voice changes per tone. A missing tone keeps this speaker and only
    /// adjusts the pace (see [`Tone::pace_factor`]).
    #[serde(default)]
    pub tones: HashMap<Tone, ToneEntry>,
}

/// How a reply should sound. The LLM picks one per reply; calls start neutral.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tone {
    /// Calm, friendly default.
    #[default]
    Neutral,
    /// Warm and upbeat: the caller gave information or agreed.
    Happy,
    /// Calm and firm: off-topic or unwanted questions, complaints.
    Serious,
}

impl Tone {
    /// Parses a reply tag's word ("happy"), ignoring case and spaces.
    pub fn from_tag(tag: &str) -> Option<Self> {
        match tag.trim().to_ascii_lowercase().as_str() {
            "neutral" => Some(Self::Neutral),
            "happy" => Some(Self::Happy),
            "serious" => Some(Self::Serious),
            _ => None,
        }
    }

    /// Lowercase name, as used in tags and metrics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Neutral => "neutral",
            Self::Happy => "happy",
            Self::Serious => "serious",
        }
    }

    /// Pace multiplier when `voices.toml` gives no pace for the tone.
    pub fn pace_factor(self) -> f32 {
        match self {
            Self::Neutral => 1.0,
            Self::Happy => 1.05,
            Self::Serious => 0.95,
        }
    }
}

/// One tone's voice within a language.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToneEntry {
    /// Speaker for this tone (same model as the language); `None` keeps the language's speaker.
    pub speaker: Option<String>,
    /// Speaking rate for this tone; `None` scales the language's pace by [`Tone::pace_factor`].
    pub pace: Option<f32>,
}

/// A fully resolved TTS voice.
#[derive(Debug, Clone, PartialEq)]
pub struct Voice {
    /// BCP-47 language code sent to TTS.
    pub language: String,
    /// TTS model.
    pub model: String,
    /// Speaker ID.
    pub speaker: String,
    /// Speaking rate.
    pub pace: f32,
}

impl VoiceMap {
    /// The agent's name for `voice` speaking `language`: the language's
    /// `name`, if `voice` is that language's own speaker (any style).
    pub fn agent_name(&self, language: &str, voice: &Voice) -> Option<&str> {
        self.own_entry(language, voice)?.name.as_deref()
    }

    /// The line that introduces the agent's name for `voice` (see `intro`).
    pub fn intro(&self, language: &str, voice: &Voice) -> Option<&str> {
        let entry = self.own_entry(language, voice)?;
        entry.name.as_ref().and(entry.intro.as_deref())
    }

    fn own_entry(&self, language: &str, voice: &Voice) -> Option<&VoiceEntry> {
        let entry = self.languages.get(language)?;
        let person = |s: &str| s.split('_').next().unwrap_or_default().to_owned();
        (person(&entry.speaker) == person(&voice.speaker)).then_some(entry)
    }

    /// Resolves the neutral voice for `language`. Unmapped languages use the
    /// fallback language's voice. `speaker_override` (the per-call `voice`
    /// field) replaces the speaker but keeps the language's model.
    pub fn resolve(
        &self,
        language: &str,
        default_model: &str,
        speaker_override: Option<&str>,
    ) -> Voice {
        self.resolve_tone(language, default_model, speaker_override, Tone::Neutral)
    }

    /// Like [`resolve`](Self::resolve), for one tone. A speaker override
    /// other than the language's own speaker keeps that speaker for every
    /// tone, so only the pace changes.
    pub fn resolve_tone(
        &self,
        language: &str,
        default_model: &str,
        speaker_override: Option<&str>,
        tone: Tone,
    ) -> Voice {
        let (language, entry) = match self.languages.get(language) {
            Some(entry) => (language, Some(entry)),
            None => (
                self.fallback_language.as_str(),
                self.languages.get(&self.fallback_language),
            ),
        };
        let model = entry
            .and_then(|e| e.model.clone())
            .unwrap_or_else(|| default_model.to_owned());
        let base_pace = entry.and_then(|e| e.pace).unwrap_or(1.0);
        let tone_entry = entry.and_then(|e| e.tones.get(&tone));
        let own_speaker = entry.map(|e| e.speaker.as_str());
        let speaker = match speaker_override.filter(|s| Some(*s) != own_speaker) {
            Some(s) => s.to_owned(),
            None => tone_entry
                .and_then(|t| t.speaker.clone())
                .or_else(|| own_speaker.map(str::to_owned))
                .unwrap_or_default(),
        };
        Voice {
            language: language.to_owned(),
            model,
            speaker,
            pace: tone_entry
                .and_then(|t| t.pace)
                .unwrap_or(base_pace * tone.pace_factor()),
        }
    }
}

/// Provider rates from `pricing.toml`, in ₹ excluding GST.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pricing {
    /// Speech-to-text rate.
    pub stt: SttRate,
    /// TTS rates by model.
    pub tts: HashMap<String, TtsRate>,
    /// LLM rates by model.
    pub llm: HashMap<String, LlmRate>,
    /// Phone line costs. Per-call charges come from Plivo's call records.
    #[serde(default)]
    pub telephony: TelephonyRate,
}

/// Fixed phone line costs.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelephonyRate {
    /// Monthly rent of the caller ID number(s), ₹ excluding GST.
    #[serde(default)]
    pub number_rental_inr_per_month: f64,
    /// GST added on top of every rate in this file, in percent.
    #[serde(default)]
    pub gst_percent: f64,
    /// Currency of the amounts in Plivo's call records: `USD` or `INR`.
    #[serde(default = "default_plivo_currency")]
    pub plivo_currency: String,
    /// ₹ per US dollar, used when `plivo_currency` is `USD`.
    #[serde(default)]
    pub usd_to_inr: f64,
}

impl Default for TelephonyRate {
    fn default() -> Self {
        Self {
            number_rental_inr_per_month: 0.0,
            gst_percent: 0.0,
            plivo_currency: default_plivo_currency(),
            usd_to_inr: 0.0,
        }
    }
}

fn default_plivo_currency() -> String {
    "USD".to_owned()
}

impl TelephonyRate {
    /// Converts an amount from a Plivo call record to ₹.
    pub fn plivo_to_inr(&self, amount: f64) -> f64 {
        if self.plivo_currency.eq_ignore_ascii_case("INR") {
            amount
        } else {
            amount * self.usd_to_inr
        }
    }
}

/// Speech-to-text rate.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SttRate {
    /// ₹ per minute of billed audio (billed per second).
    pub inr_per_minute: f64,
}

/// TTS rate for one model.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TtsRate {
    /// ₹ per 1,000 characters.
    pub inr_per_1k_chars: f64,
}

/// LLM rate for one model, ₹ per million tokens.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmRate {
    /// Uncached input tokens.
    pub input_per_1m: f64,
    /// Cached input tokens.
    pub cached_input_per_1m: f64,
    /// Output tokens (including any reasoning tokens).
    pub output_per_1m: f64,
}

/// Token counts reported by one or more LLM responses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, Deserialize)]
pub struct TokenUsage {
    /// Prompt tokens, including cached ones.
    pub prompt_tokens: u64,
    /// The cached subset of `prompt_tokens`.
    pub cached_prompt_tokens: u64,
    /// Completion tokens.
    pub completion_tokens: u64,
}

impl TokenUsage {
    /// Adds another usage report to this one.
    pub fn add(&mut self, other: TokenUsage) {
        self.prompt_tokens += other.prompt_tokens;
        self.cached_prompt_tokens += other.cached_prompt_tokens;
        self.completion_tokens += other.completion_tokens;
    }
}

impl Pricing {
    /// Cost of `seconds` of billed STT audio.
    pub fn stt_cost(&self, seconds: f64) -> f64 {
        seconds / 60.0 * self.stt.inr_per_minute
    }

    /// Cost of `chars` TTS characters on `model`; `None` if the model has no rate.
    pub fn tts_cost(&self, model: &str, chars: u64) -> Option<f64> {
        self.tts
            .get(model)
            .map(|r| chars as f64 / 1000.0 * r.inr_per_1k_chars)
    }

    /// Cost of `usage` on `model`; `None` if the model has no rate.
    pub fn llm_cost(&self, model: &str, usage: TokenUsage) -> Option<f64> {
        let rate = self.llm.get(model)?;
        let cached = usage.cached_prompt_tokens.min(usage.prompt_tokens);
        let uncached = usage.prompt_tokens - cached;
        Some(
            (uncached as f64 * rate.input_per_1m
                + cached as f64 * rate.cached_input_per_1m
                + usage.completion_tokens as f64 * rate.output_per_1m)
                / 1_000_000.0,
        )
    }
}

fn read(path: &Path) -> Result<String, AssetError> {
    fs::read_to_string(path).map_err(|source| AssetError::Io {
        path: path.to_owned(),
        source,
    })
}

fn parse_toml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, AssetError> {
    toml::from_str(&read(path)?).map_err(|source| AssetError::Toml {
        path: path.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_assets() -> Assets {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        Assets::load(&root.join("prompts"), &root.join("config")).unwrap()
    }

    #[test]
    fn repo_assets_load() {
        let assets = repo_assets();
        assert!(assets.speaking_rules.contains("SPEAKING RULES"));
        assert!(!assets.lines("en-IN").disclosure.is_empty());
        assert!(!assets.lines("hi-IN").disclosure.is_empty());
        // Unknown language falls back to English lines.
        assert_eq!(
            assets.lines("xx-IN").disclosure,
            assets.lines("en-IN").disclosure
        );
    }

    #[test]
    fn voice_resolution() {
        let assets = repo_assets();
        let v = assets.voices.resolve("hi-IN", "bulbul:v4-flash", None);
        assert_eq!(v.model, "bulbul:v4-flash");
        assert_eq!(v.speaker, "aparna_hi_customer");

        // Malayalam has no v4 Flash personas: model override to v3.
        let ml = assets.voices.resolve("ml-IN", "bulbul:v4-flash", None);
        assert_eq!(ml.model, "bulbul:v3");

        // Unmapped language uses the fallback language's voice.
        let unknown = assets.voices.resolve("sa-IN", "bulbul:v4-flash", None);
        assert_eq!(unknown.language, assets.voices.fallback_language);

        let custom = assets
            .voices
            .resolve("hi-IN", "bulbul:v4-flash", Some("ritu_hi_customer"));
        assert_eq!(custom.speaker, "ritu_hi_customer");
    }

    #[test]
    fn tones_change_speaker_or_pace() {
        let voices: VoiceMap = toml::from_str(
            r#"
            fallback_language = "en-IN"
            [languages."en-IN"]
            speaker = "simran_en_conversation"
            tones.happy = { speaker = "simran_en_customer", pace = 1.1 }
            [languages."ta-IN"]
            speaker = "gokul_ta_narration"
            "#,
        )
        .unwrap();
        let m = "bulbul:v4-flash";
        let happy = voices.resolve_tone("en-IN", m, None, Tone::Happy);
        assert_eq!(happy.speaker, "simran_en_customer");
        assert!((happy.pace - 1.1).abs() < 1e-6);
        // No serious entry: same speaker, slower.
        let serious = voices.resolve_tone("en-IN", m, None, Tone::Serious);
        assert_eq!(serious.speaker, "simran_en_conversation");
        assert!(serious.pace < 1.0);

        // One voice in Tamil: tones only change the pace.
        let ta = voices.resolve_tone("ta-IN", m, None, Tone::Happy);
        assert_eq!(ta.speaker, "gokul_ta_narration");
        assert!(ta.pace > 1.0);

        // A per-call voice keeps its speaker; naming the language's own voice does not.
        let custom = voices.resolve_tone("en-IN", m, Some("ishita_en_sales"), Tone::Happy);
        assert_eq!(custom.speaker, "ishita_en_sales");
        let own = voices.resolve_tone("en-IN", m, Some("simran_en_conversation"), Tone::Happy);
        assert_eq!(own.speaker, "simran_en_customer");

        assert_eq!(Tone::from_tag(" Serious "), Some(Tone::Serious));

        // Male voices carry their own name; any style of that voice keeps it.
        let repo = repo_assets().voices;
        let ta = repo.resolve("ta-IN", m, None);
        assert_eq!(repo.agent_name("ta-IN", &ta), Some("Gokul"));
        assert!(repo
            .intro("ta-IN", &ta)
            .is_some_and(|i| i.contains("கோகுல்")));
        let en = repo.resolve("en-IN", m, None);
        assert_eq!(repo.agent_name("en-IN", &en), None);
        let other = Voice {
            speaker: "aravind_ta_ads".into(),
            ..ta
        };
        assert_eq!(repo.agent_name("ta-IN", &other), None);
        assert_eq!(Tone::from_tag("angry"), None);
    }

    #[test]
    fn pricing_math() {
        let p = repo_assets().pricing;
        assert!((p.stt_cost(60.0) - 0.49).abs() < 1e-9);
        assert!((p.tts_cost("bulbul:v4-flash", 1000).unwrap() - 1.0).abs() < 1e-9);
        assert!((p.tts_cost("bulbul:v3", 500).unwrap() - 1.5).abs() < 1e-9);
        let usage = TokenUsage {
            prompt_tokens: 1_000_000,
            cached_prompt_tokens: 400_000,
            completion_tokens: 100_000,
        };
        // 600k * 15 + 400k * 5 + 100k * 60 = 9 + 2 + 6 = ₹17
        let cost = p.llm_cost("sarvam-105b-conversations", usage).unwrap();
        assert!((cost - 17.0).abs() < 1e-9);
        assert!(p.llm_cost("unknown-model", usage).is_none());
    }
}
