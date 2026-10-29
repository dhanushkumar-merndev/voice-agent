//! The per-call actor: one event loop that owns all call state.
//!
//! States: `Greeting → Listening → Thinking → Speaking → Listening …`, plus
//! `Ending`. Inputs are caller audio, VAD probabilities, STT events, turn
//! events, TTS audio, playout progress, timers and external commands.
//!
//! Turn-taking: Silero VAD marks speech start (2 windows ≥ threshold) and
//! end (`VAD_SILENCE_MS` below threshold). Audio reaches STT only between
//! those marks (plus a 300 ms pre-roll), and the STT final transcript then
//! starts the agent's turn.
//!
//! Barge-in: caller speech for `BARGE_IN_MIN_MS` while the agent speaks
//! clears Plivo's queue, cancels the LLM and TTS (the TTS socket is swapped
//! for a hot spare, since Sarvam has no cancel message), and keeps only the
//! audio actually played in the history.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use audio::{preroll::PreRoll, FRAME_SAMPLES};
use bytes::Bytes;
use chrono::Utc;
use common::{
    assets::{Tone, Voice},
    metric_names,
};
use providers::{
    llm::{ChatMessage, Role},
    stt::{SttCommand, SttEvent, SttOptions, SttSession},
    tts::{TtsCommand, TtsEvent, TtsSession},
    ProviderError,
};
use tokio::{sync::mpsc, time::Instant};
use tokio_util::sync::CancellationToken;

use crate::{
    playout::{PlayedClause, Playout, PlayoutCmd, PlayoutEvent},
    prompt::{self, SystemPromptParts},
    romanized::romanized_language,
    shaper::StopReason,
    turn::{self, LineKind, TurnEvent, TurnInput, TurnSummary},
    types::*,
    vad::{VadOut, VadStream, WINDOW_MS},
};

/// Pre-roll kept for STT while the gate is closed (frames of 20 ms).
const PREROLL_FRAMES: usize = 15;
/// VAD windows of speech needed to open a caller utterance.
const SPEECH_START_MS: u32 = 64;
/// Eager mode: silence before a stable partial starts a speculative turn.
const EAGER_SILENCE_MS: u32 = 160;
/// Eager mode: a partial must be unchanged for this long.
const EAGER_STABLE: Duration = Duration::from_millis(150);
/// A barge-in that cuts the agent off before it said more than this many
/// characters means the caller had only paused: their earlier words and
/// the next utterance are merged into one turn instead.
const CONTINUATION_MAX_CHARS: usize = 12;
/// Reply clauses up to this long are cached after their first synthesis.
const LEARN_MAX_CHARS: usize = 40;
/// Minimum STT language confidence to switch voices.
const LANGUAGE_CONFIDENCE: f32 = 0.6;
/// Plivo starts billing at answer, a little before our media stream starts.
const ANSWER_TO_STREAM: Duration = Duration::from_secs(1);
/// Caller audio kept per turn for re-transcription after the call. A file
/// STT request takes at most 30 s, so longer turns stop growing here and
/// are skipped by post-call.
const USER_AUDIO_MAX_BYTES: usize = 31 * audio::SAMPLE_RATE as usize;
/// Safety net once the call is ending.
const ENDING_TIMEOUT: Duration = Duration::from_secs(15);
/// Caller speech with no words transcribed yet that still interrupts the
/// agent. The first partial transcript takes about 0.9 s, so cutting off
/// sooner stops the agent mid-sentence for a "yeah" or "hmm".
const BARGE_IN_UNHEARD_MS: u32 = 1200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Greeting,
    Listening,
    Thinking,
    Speaking,
    Ending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    /// The disclosure line: always heard in full.
    Greeting,
    /// The greeting after the disclosure: interruptible, like a reply.
    GreetingBody,
    Turn(u64),
    Line,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum After {
    Listen,
    Hangup(&'static str),
}

#[derive(Debug, Default)]
struct Marks {
    speech_end: Option<Instant>,
    stt_final: Option<Instant>,
    first_token: Option<Instant>,
    tts_first: Option<Instant>,
    first_sent: Option<Instant>,
    model: Option<String>,
}

struct ActiveTurn {
    id: u64,
    cancel: CancellationToken,
    events: mpsc::Receiver<TurnEvent>,
    events_done: bool,
    speculative: bool,
    /// Held because the caller spoke during the disclosure: it is answered
    /// when the greeting body starts (which cuts the body), or once quiet.
    wait_greeting: bool,
    held: Vec<TurnEvent>,
    user_text: String,
    user_committed: bool,
    /// Position of the committed user message in the history.
    user_history_index: Option<usize>,
    user_language: Option<String>,
    clauses: Vec<(u64, String)>,
    summary: Option<Box<TurnSummary>>,
    history_index: Option<usize>,
    started_at: chrono::DateTime<Utc>,
    marks: Marks,
}

/// The two greeting clips, kept until the body has played (or was cut) so
/// the transcript and history can say what the caller actually heard.
struct GreetingParts {
    disclosure_id: u64,
    disclosure: String,
    body_id: u64,
    body: String,
    body_started: bool,
    /// When the cached disclosure finishes playing.
    disclosure_end: Option<Instant>,
    history_index: usize,
    seq: i32,
}

/// Runs one call until it ends and returns the outcome.
#[allow(clippy::too_many_arguments)]
pub async fn run_call(
    params: CallParams,
    settings: Arc<PipelineSettings>,
    deps: Deps,
    tools: Arc<dyn ToolBackend>,
    control: Arc<dyn CallControl>,
    prewarmed: Prewarmed,
    media_in: mpsc::Receiver<MediaIn>,
    media_out: mpsc::Sender<MediaOut>,
    commands: mpsc::Receiver<CallCommand>,
    shutdown: CancellationToken,
) -> CallOutcome {
    let (playout, playout_events) = Playout::spawn(media_out.clone(), settings.playout_lead);
    let mut voice = deps.assets.voices.resolve(
        &params.initial_language,
        &settings.tts_default_model,
        params.voice_override.as_deref(),
    );
    if let Some(model) = &params.tts_model {
        voice.model = model.clone();
    }
    let (spare_tx, spare_rx) = mpsc::channel(4);
    let (stt_tx, stt_rx) = mpsc::channel(2);
    let agent_name = deps
        .assets
        .voices
        .agent_name(&params.initial_language, &voice)
        .map(str::to_owned);
    let system = prompt::system_message(&SystemPromptParts {
        speaking_rules: &deps.assets.speaking_rules,
        system_prompt: &params.system_prompt,
        lead_name: &params.lead_name,
        kb_context: params.kb_context.as_deref(),
        has_kb: params.has_kb,
        has_booking: params.has_booking,
        today: &params.today,
        language: params.fixed_language.as_deref(),
        agent_name: agent_name.as_deref(),
    });
    let call_deadline = Instant::now() + params.max_duration;
    let mut actor = Actor {
        vad: deps.vad.open(),
        language: params.initial_language.clone(),
        tone: Tone::Neutral,
        prompt_language: params.fixed_language.clone(),
        agent_name,
        language_confirmed: params.fixed_language.is_some(),
        lang_candidate: None,
        p: params,
        s: settings,
        deps,
        tools,
        control,
        media_out,
        playout,
        playout_events,
        stt: None,
        stt_reopen_tx: stt_tx,
        stt_reopen_rx: stt_rx,
        stt_reopening: false,
        stt_bytes: 0,
        stt_billed: 0.0,
        user_audio: Vec::new(),
        tts: None,
        tts_spare: None,
        spare_tx,
        spare_rx,
        spare_opening: 0,
        tts_backlog: Vec::new(),
        voice,
        phase: Phase::Greeting,
        stream_started: Instant::now(),
        greeting_end: None,
        greeting_parts: None,
        user_speaking: false,
        speech_run_ms: 0,
        silence_run_ms: 0,
        utterance_ms: 0,
        last_voiced: None,
        gate_open: false,
        preroll: PreRoll::new(PREROLL_FRAMES),
        pending_user: String::new(),
        speech_end_at: None,
        stt_final_at: None,
        last_partial: None,
        next_clause: 1,
        next_turn: 1,
        clause_owner: HashMap::new(),
        started_clauses: HashSet::new(),
        tts_outstanding: HashSet::new(),
        learning: HashMap::new(),
        playout_idle: true,
        after: After::Listen,
        turn: None,
        history: Vec::new(),
        system,
        silence_deadline: None,
        ack_deadline: None,
        checkings: 0,
        silence_strikes: 0,
        call_deadline,
        hard_deadline: None,
        tts_chars_total: 0,
        seq: 0,
        outcome: CallOutcome::default(),
        done: false,
    };
    actor.run(prewarmed, media_in, commands, shutdown).await;
    actor.outcome
}

struct Actor {
    p: CallParams,
    s: Arc<PipelineSettings>,
    deps: Deps,
    tools: Arc<dyn ToolBackend>,
    control: Arc<dyn CallControl>,
    media_out: mpsc::Sender<MediaOut>,
    playout: Playout,
    playout_events: mpsc::Receiver<PlayoutEvent>,
    vad: VadStream,

    stt: Option<SttSession>,
    stt_reopen_tx: mpsc::Sender<Result<SttSession, ProviderError>>,
    stt_reopen_rx: mpsc::Receiver<Result<SttSession, ProviderError>>,
    stt_reopening: bool,
    stt_bytes: usize,
    stt_billed: f64,
    /// Caller audio sent to STT since the last committed caller turn.
    user_audio: Vec<u8>,

    tts: Option<TtsSession>,
    tts_spare: Option<TtsSession>,
    spare_tx: mpsc::Sender<Result<TtsSession, ProviderError>>,
    spare_rx: mpsc::Receiver<Result<TtsSession, ProviderError>>,
    spare_opening: usize,
    tts_backlog: Vec<(u64, String)>,
    voice: Voice,
    /// Tone of the current reply's voice; calls start neutral.
    tone: Tone,
    /// Language named in the system prompt.
    prompt_language: Option<String>,
    /// The agent's name for the current voice (`None`: the persona's name).
    agent_name: Option<String>,
    language: String,
    language_confirmed: bool,
    lang_candidate: Option<(String, u8)>,

    phase: Phase,
    stream_started: Instant,
    /// When the cached greeting finishes playing.
    greeting_end: Option<Instant>,
    greeting_parts: Option<GreetingParts>,

    user_speaking: bool,
    speech_run_ms: u32,
    silence_run_ms: u32,
    utterance_ms: u32,
    last_voiced: Option<Instant>,
    gate_open: bool,
    preroll: PreRoll,
    pending_user: String,
    speech_end_at: Option<Instant>,
    stt_final_at: Option<Instant>,
    last_partial: Option<(String, Instant)>,

    next_clause: u64,
    next_turn: u64,
    clause_owner: HashMap<u64, Owner>,
    started_clauses: HashSet<u64>,
    tts_outstanding: HashSet<u64>,
    /// Short reply clauses being synthesized, kept to offer to the cache.
    learning: HashMap<u64, (Voice, String, Vec<u8>)>,
    playout_idle: bool,
    after: After,
    turn: Option<ActiveTurn>,
    history: Vec<ChatMessage>,
    system: String,

    silence_deadline: Option<Instant>,
    /// Answer the held "yes" / "hmm" in `pending_user` at this time unless
    /// the caller goes on (see `ack_wait`).
    ack_deadline: Option<Instant>,
    /// Checking fillers played so far, to take the next variant.
    checkings: usize,
    silence_strikes: u8,
    call_deadline: Instant,
    hard_deadline: Option<Instant>,

    tts_chars_total: usize,
    seq: i32,
    outcome: CallOutcome,
    done: bool,
}

enum Ev {
    Shutdown,
    Media(Option<MediaIn>),
    Command(Option<CallCommand>),
    Vad(Option<VadOut>),
    Stt(Option<SttEvent>),
    SttReopened(Option<Result<SttSession, ProviderError>>),
    Tts(Option<TtsEvent>),
    Spare(Option<Result<TtsSession, ProviderError>>),
    Turn(Option<TurnEvent>),
    Playout(Option<PlayoutEvent>),
    Timer,
}

async fn recv_opt<T>(rx: Option<&mut mpsc::Receiver<T>>) -> Option<T> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

fn ms_between(later: Instant, earlier: Instant) -> i64 {
    later.saturating_duration_since(earlier).as_millis() as i64
}

/// Words that only acknowledge, hesitate or check the line ("yeah", "so",
/// "haan", "hello"), in the supported languages.
const BACKCHANNEL_WORDS: &[&str] = &[
    "yeah",
    "yes",
    "yep",
    "ya",
    "yah",
    "ok",
    "okay",
    "okey",
    "k",
    "hmm",
    "hm",
    "mm",
    "mhm",
    "mmhmm",
    "uh",
    "huh",
    "so",
    "um",
    "umm",
    "ah",
    "aah",
    "oh",
    "uhhuh",
    "right",
    "sure",
    "fine",
    "alright",
    "great",
    "good",
    "cool",
    "hello",
    "hi",
    "hey",
    "haan",
    "han",
    "ha",
    "haa",
    "haanji",
    "ji",
    "achha",
    "acha",
    "accha",
    "theek",
    "thik",
    "hai",
    "houdu",
    "sari",
    "seri",
    "aama",
    "aamaa",
    "avunu",
    "sare",
    "hum",
    "हाँ",
    "हां",
    "हा",
    "जी",
    "अच्छा",
    "ठीक",
    "है",
    "ओके",
    "हम्म",
    "हेलो",
    "हैलो",
    "howdu",
    "aitu",
    "aaytu",
    "seringa",
    "aamanga",
    "alage",
    "aunu",
    "sarenandi",
    "hoon",
    "hya",
    "accha",
    "thik",
    "ಹೌದು",
    "ಹೌದ್",
    "ಸರಿ",
    "ಓಕೆ",
    "ಹಲೋ",
    "ಹಾಂ",
    "ಹಾ",
    "ಹೂಂ",
    "ಹೂ",
    "ಹ್ಞೂ",
    "ಆಯ್ತು",
    "ಓ",
    "ஆமா",
    "ஆமாம்",
    "ஆமாங்க",
    "சரி",
    "சரிங்க",
    "ஓகே",
    "ஹலோ",
    "ம்",
    "ம்ம்",
    "ஆ",
    "ஓ",
    "అవును",
    "సరే",
    "సరేనండి",
    "అలాగే",
    "ఓకే",
    "హలో",
    "హా",
    "ఊ",
    "ఆ",
    "ശരി",
    "അതെ",
    "ഓക്കേ",
    "ഹലോ",
    "ഉം",
    "ആ",
    "হ্যাঁ",
    "হ্যা",
    "আচ্ছা",
    "ঠিক",
    "আছে",
    "ওকে",
    "হ্যালো",
    "হুম",
];

/// Most words a backchannel can have ("yeah okay sure").
const BACKCHANNEL_MAX_WORDS: usize = 3;

/// `true` when `text` is only an acknowledgement or "hello": it should not
/// interrupt the agent, and on its own it is not a turn during the greeting.
fn is_backchannel(text: &str) -> bool {
    let norm = normalize(text);
    let words: Vec<&str> = norm.split_whitespace().collect();
    !words.is_empty()
        && words.len() <= BACKCHANNEL_MAX_WORDS
        && words
            .iter()
            .all(|w| BACKCHANNEL_WORDS.iter().any(|b| normalize(b) == *w))
}

fn normalize(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

impl Actor {
    async fn run(
        &mut self,
        prewarmed: Prewarmed,
        mut media_in: mpsc::Receiver<MediaIn>,
        mut commands: mpsc::Receiver<CallCommand>,
        shutdown: CancellationToken,
    ) {
        if !self.start(prewarmed, &mut media_in).await {
            self.finish().await;
            return;
        }
        let mut commands_open = true;
        let mut shutdown_seen = false;

        while !self.done {
            let deadline = [
                self.silence_deadline,
                self.ack_deadline,
                Some(self.call_deadline),
                self.hard_deadline,
            ]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(self.call_deadline);

            let ev = tokio::select! {
                () = shutdown.cancelled(), if !shutdown_seen => Ev::Shutdown,
                m = media_in.recv() => Ev::Media(m),
                c = commands.recv(), if commands_open => Ev::Command(c),
                v = self.vad.results.recv() => Ev::Vad(v),
                s = recv_opt(self.stt.as_mut().map(|s| &mut s.events)) => Ev::Stt(s),
                r = self.stt_reopen_rx.recv() => Ev::SttReopened(r),
                t = recv_opt(self.tts.as_mut().map(|t| &mut t.events)) => Ev::Tts(t),
                r = self.spare_rx.recv() => Ev::Spare(r),
                e = recv_opt(self.turn.as_mut().filter(|t| !t.events_done).map(|t| &mut t.events)) => Ev::Turn(e),
                p = self.playout_events.recv() => Ev::Playout(p),
                () = tokio::time::sleep_until(deadline) => Ev::Timer,
            };

            match ev {
                Ev::Shutdown => {
                    shutdown_seen = true;
                    let line = self
                        .deps
                        .assets
                        .lines(&self.language)
                        .max_duration_closing
                        .clone();
                    self.end_with_line(line, "server_shutdown").await;
                }
                Ev::Media(None) => {
                    if self.outcome.end_reason.is_empty() {
                        self.outcome.end_reason = "stream_closed".to_owned();
                    }
                    break;
                }
                Ev::Media(Some(m)) => self.on_media(m),
                Ev::Command(None) => commands_open = false,
                Ev::Command(Some(CallCommand::Hangup)) => self.hangup("api_hangup").await,
                Ev::Vad(Some(v)) => self.on_vad(v).await,
                Ev::Vad(None) => {}
                Ev::Stt(Some(e)) => self.on_stt(e).await,
                Ev::Stt(None) => {
                    tracing::warn!(call = %self.p.call_id, "stt session closed; reopening");
                    self.stt = None;
                    self.reopen_stt();
                }
                Ev::SttReopened(Some(Ok(session))) => {
                    self.stt_reopening = false;
                    self.stt = Some(session);
                    if self.gate_open {
                        self.stt_send(SttCommand::SpeechStart);
                    }
                }
                Ev::SttReopened(Some(Err(e))) => {
                    self.stt_reopening = false;
                    tracing::error!(call = %self.p.call_id, error = %e, "stt reopen failed");
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    self.reopen_stt();
                }
                Ev::SttReopened(None) => {}
                Ev::Tts(Some(e)) => self.on_tts(e).await,
                Ev::Tts(None) => {
                    tracing::warn!(call = %self.p.call_id, "tts session closed");
                    self.abandon_tts_outstanding().await;
                    self.tts = None;
                    self.promote_spare().await;
                }
                Ev::Spare(Some(r)) => self.on_spare(r).await,
                Ev::Spare(None) => {}
                Ev::Turn(Some(e)) => self.on_turn_event(e).await,
                Ev::Turn(None) => {
                    if let Some(t) = self.turn.as_mut() {
                        t.events_done = true;
                        if t.summary.is_none() && !t.speculative {
                            // The task ended without a summary (cancelled or crashed).
                            t.summary = Some(Box::default());
                        }
                    }
                    self.check_quiet().await;
                }
                Ev::Playout(Some(e)) => self.on_playout(e).await,
                Ev::Playout(None) => break,
                Ev::Timer => self.on_timer().await,
            }
        }
        self.finish().await;
    }

    // ------------------------------------------------------------ start ----

    /// Waits for the stream start, plays the greeting and makes sure STT
    /// and TTS are connected. Returns `false` if the call cannot proceed.
    async fn start(
        &mut self,
        prewarmed: Prewarmed,
        media_in: &mut mpsc::Receiver<MediaIn>,
    ) -> bool {
        match tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match media_in.recv().await {
                    Some(MediaIn::Start) => return true,
                    Some(_) => continue,
                    None => return false,
                }
            }
        })
        .await
        {
            Ok(true) => {}
            _ => {
                self.outcome.end_reason = "no_stream_start".to_owned();
                return false;
            }
        }
        self.stream_started = Instant::now();
        self.align_call_deadline();
        self.phase = Phase::Greeting;
        self.after = After::Listen;

        let greeting = self.p.greeting_text.clone();
        // The disclosure plays in full; the body after it is interruptible.
        let disclosure = self.p.disclosure_text.trim().to_owned();
        let body = match greeting.strip_prefix(disclosure.as_str()) {
            Some(rest) if !disclosure.is_empty() => rest.trim().to_owned(),
            _ => String::new(),
        };
        let first = if body.is_empty() {
            greeting.clone()
        } else {
            disclosure
        };
        let first_audio = if body.is_empty() {
            self.p
                .greeting_audio
                .clone()
                .or_else(|| self.p.disclosure_audio.clone())
        } else {
            self.p.disclosure_audio.clone()
        }
        .or_else(|| self.deps.lines.get(&self.voice, &first));
        let body_audio = if body.is_empty() {
            None
        } else {
            self.p
                .greeting_audio
                .clone()
                .or_else(|| self.deps.lines.get(&self.voice, &body))
        };
        let clip_end = |audio: &Bytes| Duration::from_millis(audio::mulaw_duration_ms(audio.len()));
        let (mut first_id, mut body_id) = (None, None);
        let mut disclosure_end = None;
        if let Some(audio) = first_audio.clone() {
            let end = self.stream_started + clip_end(&audio);
            disclosure_end = Some(end);
            self.greeting_end = Some(end + body_audio.as_ref().map_or(Duration::ZERO, clip_end));
            first_id = Some(self.play_clip(audio, Owner::Greeting).await);
            if let Some(audio) = body_audio.clone() {
                body_id = Some(self.play_clip(audio, Owner::GreetingBody).await);
            }
        }

        // Connect whatever was not prewarmed (the greeting is already playing).
        let Prewarmed { stt, tts } = prewarmed;
        let stt_fut = async {
            match stt {
                Some(s) => Ok(s),
                None => self.deps.stt.open(self.stt_options()).await,
            }
        };
        let tts_fut = async {
            match tts {
                Some(pair) => Ok(pair),
                None => {
                    let (a, b) = tokio::join!(
                        self.deps.tts.open(&self.voice),
                        self.deps.tts.open(&self.voice)
                    );
                    a.map(|a| (a, b.ok()))
                }
            }
        };
        let (stt, tts) = tokio::join!(stt_fut, tts_fut);
        match stt {
            Ok(s) => self.stt = Some(s),
            Err(e) => {
                tracing::error!(call = %self.p.call_id, error = %e, "stt unavailable");
                self.reopen_stt();
            }
        }
        match tts {
            Ok((active, spare)) => {
                self.tts = Some(active);
                self.tts_spare = spare;
                if self.tts_spare.is_none() {
                    self.open_spare();
                }
            }
            Err(e) => {
                tracing::error!(call = %self.p.call_id, error = %e, "tts unavailable");
                if first_audio.is_none() {
                    self.outcome.end_reason = "tts_unavailable".to_owned();
                    self.control.hangup().await;
                    return false;
                }
                self.open_spare();
            }
        }
        let first_id = match first_id {
            Some(id) => id,
            None => self.speak_text(first.clone(), Owner::Greeting).await,
        };
        let body_id = match (body_id, body_audio) {
            (Some(id), _) => Some(id),
            _ if body.is_empty() => None,
            (None, Some(audio)) => Some(self.play_clip(audio, Owner::GreetingBody).await),
            (None, None) => Some(self.speak_text(body.clone(), Owner::GreetingBody).await),
        };
        let history_index = self.history.len();
        self.history.push(ChatMessage::text(
            Role::Assistant,
            tagged(Some(Tone::Neutral), &greeting),
        ));
        self.record(Speaker::Assistant, greeting, false, Utc::now());
        if let Some(body_id) = body_id {
            self.greeting_parts = Some(GreetingParts {
                disclosure_id: first_id,
                disclosure: first,
                body_id,
                body,
                body_started: false,
                disclosure_end,
                history_index,
                seq: self.seq,
            });
        }
        true
    }

    /// The disclosure is still playing (the greeting body has not started).
    fn in_disclosure(&self) -> bool {
        self.phase == Phase::Greeting
            && self
                .greeting_parts
                .as_ref()
                .is_some_and(|g| !g.body_started)
    }

    /// The greeting body was cut off: the transcript and history keep only
    /// what was played, so the LLM knows its question may not have been heard.
    fn note_greeting_cut(&mut self, report: &[PlayedClause]) {
        let Some(g) = self.greeting_parts.take() else {
            return;
        };
        let clauses = [
            (g.disclosure_id, g.disclosure.clone()),
            (g.body_id, g.body.clone()),
        ];
        let played = played_text(&clauses, report, &self.started_clauses);
        if played == format!("{} {}", g.disclosure, g.body) {
            return;
        }
        if self.greeting_end.is_some() {
            self.greeting_end = g.disclosure_end;
        }
        if let Some(m) = self.history.get_mut(g.history_index) {
            m.content = Some(tagged(Some(Tone::Neutral), &played));
        }
        if let Some(r) = self.outcome.turns.iter_mut().find(|r| r.seq == g.seq) {
            r.text = played;
            r.interrupted = true;
        }
    }

    /// Answers what the caller said during the disclosure: stops the
    /// greeting body and lets the held turn speak.
    async fn release_greeting_turn(&mut self) {
        if !self.turn.as_ref().is_some_and(|t| t.wait_greeting) {
            return;
        }
        if !self.playout_idle {
            self.stop_audio().await;
            self.clause_owner.clear();
            self.started_clauses.clear();
        }
        let Some(t) = self.turn.as_mut() else { return };
        t.speculative = false;
        t.wait_greeting = false;
        let held = std::mem::take(&mut t.held);
        self.pending_user.clear();
        self.phase = Phase::Thinking;
        self.after = After::Listen;
        for e in held {
            Box::pin(self.on_turn_event(e)).await;
        }
    }

    fn stt_options(&self) -> SttOptions {
        SttOptions {
            language: self.p.fixed_language.clone(),
        }
    }

    fn reopen_stt(&mut self) {
        if self.stt_reopening {
            return;
        }
        self.stt_reopening = true;
        let stt = self.deps.stt.clone();
        let options = self.stt_options();
        let tx = self.stt_reopen_tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(stt.open(options).await).await;
        });
    }

    // ------------------------------------------------------------ input ----

    fn on_media(&mut self, m: MediaIn) {
        match m {
            MediaIn::Audio(frame) => {
                self.vad.push(frame.clone());
                if self.gate_open {
                    self.stt_audio(frame);
                } else {
                    self.preroll.push(frame);
                }
            }
            MediaIn::Played(name) => tracing::trace!(checkpoint = %name, "played"),
            MediaIn::Dtmf(digit) => tracing::debug!(%digit, "dtmf"),
            MediaIn::Start => {}
        }
    }

    fn stt_send(&mut self, cmd: SttCommand) {
        if let Some(stt) = &self.stt {
            if stt.commands.try_send(cmd).is_err() {
                tracing::warn!(call = %self.p.call_id, "stt queue full; dropping");
            }
        }
    }

    fn stt_audio(&mut self, frame: Bytes) {
        self.stt_bytes += frame.len();
        if self.user_audio.len() < USER_AUDIO_MAX_BYTES {
            self.user_audio.extend_from_slice(&frame);
        }
        self.stt_send(SttCommand::Audio(frame));
    }

    async fn on_vad(&mut self, v: VadOut) {
        let th = self.s.vad_threshold;
        let low = (th - 0.15).max(0.05);
        if v.prob >= th {
            self.speech_run_ms += WINDOW_MS;
            self.silence_run_ms = 0;
            self.last_voiced = Some(Instant::from_std(v.at));
            if self.user_speaking {
                self.utterance_ms += WINDOW_MS;
            }
        } else if v.prob < low {
            self.silence_run_ms += WINDOW_MS;
            self.speech_run_ms = 0;
        } else if self.user_speaking {
            // Between thresholds: keep the current state.
            self.last_voiced = Some(Instant::from_std(v.at));
            self.utterance_ms += WINDOW_MS;
        }

        if !self.user_speaking && self.speech_run_ms >= SPEECH_START_MS {
            self.on_speech_start().await;
        }
        if !self.user_speaking {
            return;
        }
        let barge_in_ms = self.s.barge_in_min.as_millis() as u32;
        if self.phase == Phase::Speaking && self.utterance_ms >= barge_in_ms {
            // "Yeah", "okay": keep talking. Without a transcript yet, wait
            // for one before cutting the agent off.
            let hold = match &self.last_partial {
                Some((partial, _)) => is_backchannel(partial),
                None => self.utterance_ms < BARGE_IN_UNHEARD_MS.max(barge_in_ms),
            };
            if !hold {
                self.barge_in().await;
            }
        }
        if self.s.eager_llm
            && self.silence_run_ms >= EAGER_SILENCE_MS
            && self.turn.is_none()
            && self.phase != Phase::Ending
        {
            if let Some((partial, at)) = self.last_partial.clone() {
                // A lone "yes" may be held (see `ack_wait`): no speculation.
                let ack = is_backchannel(&join_text(&self.pending_user, &partial));
                if !partial.is_empty() && !ack && at.elapsed() >= EAGER_STABLE {
                    let text = join_text(&self.pending_user, &partial);
                    self.speech_end_at = self.last_voiced;
                    self.start_turn(text, true).await;
                }
            }
        }
        if self.silence_run_ms >= self.s.vad_silence.as_millis() as u32 {
            self.on_speech_end();
        }
    }

    async fn on_speech_start(&mut self) {
        self.user_speaking = true;
        self.utterance_ms = self.speech_run_ms;
        self.silence_strikes = 0;
        self.silence_deadline = None;
        // The caller goes on after "yes": the next final joins onto it.
        self.ack_deadline = None;
        self.last_partial = None;
        self.stt_send(SttCommand::SpeechStart);
        let frames: Vec<Bytes> = self.preroll.drain().collect();
        for f in frames {
            self.stt_audio(f);
        }
        self.gate_open = true;

        // The caller kept talking: a turn that has not spoken yet is stale.
        // While ending, the turn that said goodbye must finish.
        let stale = self.phase != Phase::Ending
            && self
                .turn
                .as_ref()
                .is_some_and(|t| t.speculative || (t.clauses.is_empty() && !t.user_committed));
        if stale {
            if let Some(t) = self.turn.take() {
                t.cancel.cancel();
                if !t.speculative {
                    self.pending_user = t.user_text;
                }
                if self.phase == Phase::Thinking {
                    self.phase = Phase::Listening;
                }
            }
        }
    }

    fn on_speech_end(&mut self) {
        self.user_speaking = false;
        self.utterance_ms = 0;
        self.speech_end_at = self.last_voiced.or(Some(Instant::now()));
        self.stt_send(SttCommand::SpeechEnd);
        self.gate_open = false;
        if self.phase == Phase::Listening && self.turn.is_none() {
            // If no transcript arrives (noise), the silence timer still runs.
            self.silence_deadline = Some(Instant::now() + self.s.silence_reprompt);
        }
    }

    async fn on_stt(&mut self, e: SttEvent) {
        match e {
            SttEvent::Partial { text, .. } => {
                if !text.is_empty() && self.last_partial.as_ref().map(|(t, _)| t) != Some(&text) {
                    let interrupts = self.phase == Phase::Speaking
                        && self.user_speaking
                        && text.split_whitespace().count() >= 2
                        && !is_backchannel(&text);
                    self.last_partial = Some((text, Instant::now()));
                    if interrupts {
                        self.barge_in().await;
                    }
                }
            }
            SttEvent::Final {
                text,
                language,
                language_confidence,
                ..
            } => {
                self.stt_final_at = Some(Instant::now());
                if self.phase == Phase::Ending {
                    // Goodbye said ("okay, okay"): don't start a new turn.
                    self.pending_user.clear();
                    self.drop_unused_audio();
                    return;
                }
                self.detect_language(language.as_deref(), language_confidence)
                    .await;
                if text.trim().is_empty() {
                    if !self.user_speaking && self.pending_user.is_empty() {
                        self.drop_unused_audio();
                    }
                    return;
                }
                let combined = join_text(&self.pending_user, &text);
                if self.in_disclosure() {
                    // The disclosure must be heard: "hello?" is ignored, and
                    // anything else is answered when the greeting body starts.
                    if is_backchannel(&combined) {
                        self.pending_user.clear();
                        self.drop_unused_audio();
                        return;
                    }
                    self.pending_user = combined.clone();
                    if self.user_speaking {
                        return;
                    }
                    let same = self.turn.as_ref().is_some_and(|t| {
                        t.speculative && normalize(&t.user_text) == normalize(&combined)
                    });
                    if !same {
                        if let Some(t) = self.turn.take() {
                            t.cancel.cancel();
                        }
                        self.start_turn(combined, true).await;
                    }
                    if let Some(t) = self.turn.as_mut() {
                        t.wait_greeting = true;
                        t.marks.stt_final = self.stt_final_at;
                    }
                    return;
                }
                if let Some(t) = self.turn.as_mut() {
                    if t.speculative {
                        if normalize(&t.user_text) == normalize(&combined) {
                            t.speculative = false;
                            t.marks.stt_final = self.stt_final_at;
                            self.pending_user.clear();
                            self.phase = Phase::Thinking;
                            let held = std::mem::take(&mut t.held);
                            for e in held {
                                Box::pin(self.on_turn_event(e)).await;
                            }
                        } else if let Some(t) = self.turn.take() {
                            t.cancel.cancel();
                            self.start_turn(combined, false).await;
                        }
                        return;
                    }
                }
                if self.user_speaking {
                    self.pending_user = combined;
                    return;
                }
                let greeting_body = self.phase == Phase::Speaking
                    && self.greeting_parts.as_ref().is_some_and(|g| g.body_started);
                if greeting_body
                    || (self.turn.is_some()
                        && matches!(self.phase, Phase::Speaking | Phase::Thinking))
                {
                    // Backchannel ("haan", "ok") while the agent talks: ignore.
                    if is_backchannel(&combined) {
                        self.pending_user.clear();
                        self.drop_unused_audio();
                        return;
                    }
                    self.pending_user.clear();
                    self.barge_in().await;
                    // Non-empty when the caller had only paused (see `resume_user`).
                    let combined = join_text(&std::mem::take(&mut self.pending_user), &combined);
                    self.start_turn(combined, false).await;
                    return;
                }
                // "Hello?" while the greeting plays: they will hear the question.
                if self.phase == Phase::Greeting && is_backchannel(&combined) {
                    self.pending_user.clear();
                    self.drop_unused_audio();
                    return;
                }
                // "Yes..." / "So..." after a question is often the start of
                // the answer: give the caller a moment to go on.
                if !self.s.ack_wait.is_zero() && is_backchannel(&combined) {
                    if let Some(t) = self.turn.take() {
                        t.cancel.cancel();
                    }
                    self.pending_user = combined;
                    self.ack_deadline = Some(Instant::now() + self.s.ack_wait);
                    self.silence_deadline = None;
                    return;
                }
                self.start_turn(combined, false).await;
            }
            SttEvent::SessionEnd { billed_secs } => self.stt_billed += billed_secs,
            SttEvent::Error {
                code,
                message,
                fatal,
            } => {
                tracing::warn!(call = %self.p.call_id, %code, %message, fatal, "stt error");
                if fatal {
                    self.stt = None;
                    self.reopen_stt();
                }
            }
        }
    }

    async fn detect_language(&mut self, language: Option<&str>, confidence: Option<f32>) {
        if self.p.fixed_language.is_some() {
            return;
        }
        let (Some(lang), Some(conf)) = (language, confidence) else {
            return;
        };
        if conf < LANGUAGE_CONFIDENCE {
            return;
        }
        // TTS and voices.toml use od-IN for Odia; realtime STT reports or-IN.
        let lang = if lang == "or-IN" { "od-IN" } else { lang };
        if lang == self.language {
            self.language_confirmed = true;
            self.lang_candidate = None;
            self.outcome.detected_language = Some(lang.to_owned());
            return;
        }
        let count = match &self.lang_candidate {
            Some((l, n)) if l == lang => n + 1,
            _ => 1,
        };
        self.lang_candidate = Some((lang.to_owned(), count));
        let needed = if self.language_confirmed { 2 } else { 1 };
        if count >= needed {
            self.switch_language(lang.to_owned()).await;
        }
    }

    /// The model switched language (the caller asked, or spoke it): change
    /// the voice before this turn's reply, and pin speech-to-text to the
    /// language. Auto-detection can mistake code-mixed speech for English
    /// written in English letters, so the model's switch is the one that
    /// holds; it can switch again if the caller asks.
    async fn switch_on_request(&mut self, lang: String) {
        self.switch_pinned(lang, "tool").await;
    }

    /// Switches to `lang` and pins speech-to-text to it; `via` labels the metric.
    async fn switch_pinned(&mut self, lang: String, via: &'static str) {
        if !self.deps.assets.voices.languages.contains_key(&lang) {
            tracing::warn!(call = %self.p.call_id, %lang, "no voice for requested language");
            return;
        }
        metrics::counter!("voice_language_switches_total", "via" => via).increment(1);
        if self.p.fixed_language.as_deref() != Some(lang.as_str()) {
            self.p.fixed_language = Some(lang.clone());
            // The new session replaces the current one when it opens.
            self.reopen_stt();
        }
        if lang != self.language {
            self.switch_language(lang).await;
        }
    }

    async fn switch_language(&mut self, lang: String) {
        tracing::info!(call = %self.p.call_id, from = %self.language, to = %lang, "switching language");
        self.language = lang.clone();
        self.language_confirmed = true;
        self.lang_candidate = None;
        self.outcome.detected_language = Some(lang.clone());
        let voice = self.voice_for(&lang, self.tone);
        if voice.model == self.voice.model {
            self.configure_tts(voice).await;
        } else {
            // Different model (e.g. Malayalam on bulbul:v3): new sockets.
            self.voice = voice;
            self.abandon_tts_outstanding().await;
            for s in [self.tts.take(), self.tts_spare.take()]
                .into_iter()
                .flatten()
            {
                let _ = s.commands.send(TtsCommand::Close).await;
            }
            self.open_spare();
            self.open_spare();
        }
        let name = self
            .deps
            .assets
            .voices
            .agent_name(&lang, &self.voice)
            .map(str::to_owned);
        let changed = name != self.agent_name;
        self.agent_name = name;
        self.prompt_language = Some(lang.clone());
        self.rebuild_system();
        if changed {
            // A voice that doesn't fit the persona's name: it says its own
            // name once, before the reply.
            let intro = self
                .deps
                .assets
                .voices
                .intro(&lang, &self.voice)
                .map(str::to_owned);
            if let Some(intro) = intro {
                self.say_intro(intro).await;
            }
        }
    }

    /// Plays the new voice's introduction as part of the current reply (or
    /// on its own between turns), so the transcript and history keep it.
    async fn say_intro(&mut self, text: String) {
        tracing::info!(call = %self.p.call_id, "new voice introduces itself");
        match self.turn.as_ref().map(|t| t.id) {
            Some(turn_id) => {
                let id = self.speak_line(text.clone(), Owner::Turn(turn_id)).await;
                if let Some(t) = self.turn.as_mut() {
                    t.clauses.push((id, text));
                }
            }
            None => {
                self.speak_line(text.clone(), Owner::Line).await;
                self.history
                    .push(ChatMessage::text(Role::Assistant, text.clone()));
                self.record(Speaker::Assistant, text, false, Utc::now());
            }
        }
    }

    /// Rebuilds the system message from the call's current language and name.
    fn rebuild_system(&mut self) {
        self.system = prompt::system_message(&SystemPromptParts {
            speaking_rules: &self.deps.assets.speaking_rules,
            system_prompt: &self.p.system_prompt,
            lead_name: &self.p.lead_name,
            kb_context: self.p.kb_context.as_deref(),
            has_kb: self.p.has_kb,
            has_booking: self.p.has_booking,
            today: &self.p.today,
            language: self.prompt_language.as_deref(),
            agent_name: self.agent_name.as_deref(),
        });
    }

    /// The voice for `lang` in `tone`. The per-call voice applies to the
    /// call's first language (and to every language when the call chose a
    /// TTS model, since v3 voices speak every language).
    fn voice_for(&self, lang: &str, tone: Tone) -> Voice {
        let voices = &self.deps.assets.voices;
        let default_model = &self.s.tts_default_model;
        match &self.p.tts_model {
            Some(model) => {
                // Tone speakers belong to the default model: keep the speaker, change the pace.
                let mut voice =
                    voices.resolve(lang, default_model, self.p.voice_override.as_deref());
                voice.model = model.clone();
                voice.pace *= tone.pace_factor();
                voice
            }
            None => {
                let speaker = self
                    .p
                    .voice_override
                    .as_deref()
                    .filter(|_| lang == self.p.initial_language);
                voices.resolve_tone(lang, default_model, speaker, tone)
            }
        }
    }

    /// Sends `voice` (same model) to the open TTS sockets and makes it current.
    async fn configure_tts(&mut self, voice: Voice) {
        for s in [self.tts.as_ref(), self.tts_spare.as_ref()]
            .into_iter()
            .flatten()
        {
            let _ = s.commands.send(TtsCommand::Configure(voice.clone())).await;
        }
        self.voice = voice;
    }

    /// The reply's tone: switch speaker and pace before its first clause.
    async fn set_tone(&mut self, tone: Tone) {
        if tone == self.tone {
            return;
        }
        self.tone = tone;
        metrics::counter!("voice_tone_changes_total", "tone" => tone.as_str()).increment(1);
        let voice = self.voice_for(&self.language.clone(), tone);
        if voice == self.voice || voice.model != self.voice.model {
            return;
        }
        tracing::debug!(call = %self.p.call_id, tone = tone.as_str(), speaker = %voice.speaker, "tone");
        self.configure_tts(voice).await;
    }

    // ------------------------------------------------------------ turns ----

    /// Starts the LLM turn for `user_text`. Text that is an Indian language
    /// in English letters switches the call to that language first, so the
    /// reply comes in its own script (models otherwise answer it in English).
    async fn start_turn(&mut self, user_text: String, speculative: bool) {
        if let Some(lang) = romanized_language(&user_text).filter(|l| *l != self.language) {
            tracing::info!(call = %self.p.call_id, %lang, "caller wrote the language in English letters");
            self.switch_pinned(lang.to_owned(), "romanized").await;
        }
        self.start_turn_now(user_text, speculative);
    }

    fn start_turn_now(&mut self, user_text: String, speculative: bool) {
        let mut messages = prompt::assemble(&self.system, &self.history);
        // No billing-block hint for the LLM: told "N seconds left, wrap up if
        // done", it ended live conversations mid-question. The block is used
        // only for the max-duration closing and the short goodbye.
        messages.push(ChatMessage::text(Role::User, user_text.clone()));
        let input = TurnInput {
            messages,
            tools: prompt::tools(self.p.has_kb, self.p.has_booking),
            max_turn_chars: self.s.max_spoken_chars_per_turn,
            max_turn_sentences: self.s.max_sentences_per_turn,
            budget_chars: self
                .s
                .max_tts_chars_per_call
                .saturating_sub(self.tts_chars_total),
            primary: self
                .p
                .llm_primary
                .clone()
                .unwrap_or_else(|| self.s.llm_primary.clone()),
            fallback: self.s.llm_fallback.clone(),
            first_token_timeout: self.s.llm_first_token_timeout,
            hedge_after: self.s.llm_hedge_after,
            max_tokens: self.s.llm_max_tokens,
        };
        let cancel = CancellationToken::new();
        let events = turn::spawn(
            input,
            self.deps.llm.clone(),
            self.tools.clone(),
            cancel.clone(),
        );
        let id = self.next_turn;
        self.next_turn += 1;
        self.turn = Some(ActiveTurn {
            id,
            cancel,
            events,
            events_done: false,
            speculative,
            wait_greeting: false,
            held: Vec::new(),
            user_text,
            user_committed: false,
            user_history_index: None,
            user_language: Some(self.language.clone()),
            clauses: Vec::new(),
            summary: None,
            history_index: None,
            started_at: Utc::now(),
            marks: Marks {
                speech_end: self.speech_end_at.or(self.stt_final_at),
                stt_final: if speculative { None } else { self.stt_final_at },
                ..Marks::default()
            },
        });
        if !speculative {
            self.pending_user.clear();
            self.phase = Phase::Thinking;
            self.after = After::Listen;
        }
        self.silence_deadline = None;
    }

    fn commit_user(&mut self) {
        let Some(t) = self.turn.as_mut() else { return };
        if t.user_committed {
            return;
        }
        t.user_committed = true;
        t.user_history_index = Some(self.history.len());
        let text = t.user_text.clone();
        let language = t.user_language.clone();
        let at = t.started_at;
        // The caller cannot have spoken since this turn started: speech
        // before commit makes the turn stale and merges it into the next.
        let audio = std::mem::take(&mut self.user_audio);
        self.history
            .push(ChatMessage::text(Role::User, text.clone()));
        self.seq += 1;
        self.outcome.turns.push(TurnRecord {
            seq: self.seq,
            role: Speaker::User,
            text,
            language,
            interrupted: false,
            started_at: at,
            ended_at: Some(at),
            audio: (!audio.is_empty()).then(|| Bytes::from(audio)),
        });
    }

    /// Forgets caller audio that belongs to no turn (an ignored "yeah", noise).
    fn drop_unused_audio(&mut self) {
        if self.turn.as_ref().is_none_or(|t| t.user_committed) {
            self.user_audio.clear();
        }
    }

    async fn on_turn_event(&mut self, e: TurnEvent) {
        let Some(t) = self.turn.as_mut() else { return };
        if t.speculative {
            t.held.push(e);
            return;
        }
        let turn_id = t.id;
        match e {
            TurnEvent::FirstToken { model } => {
                t.marks.first_token = Some(Instant::now());
                t.marks.model = Some(model);
            }
            TurnEvent::Clause(text) => {
                self.commit_user();
                // Models often answer "talk in Telugu" in Telugu without
                // calling switch_language: follow the reply's script so the
                // voice speaks it.
                if let Some(lang) = reply_language(&text, &self.language) {
                    tracing::info!(call = %self.p.call_id, %lang, "reply script changed the language");
                    self.switch_pinned(lang.to_owned(), "script").await;
                }
                let id = match self.deps.lines.get(&self.voice, &text) {
                    Some(audio) => {
                        // A common phrase: no TTS round trip, nothing billed.
                        if let Some(t) = self.turn.as_mut() {
                            t.marks.tts_first.get_or_insert_with(Instant::now);
                        }
                        let chars = text.chars().count() as u64;
                        self.outcome.tts_cached_chars += chars;
                        metrics::counter!("voice_tts_cache_hits_total").increment(1);
                        metrics::counter!("voice_tts_cached_chars_total").increment(chars);
                        self.play_clip(audio, Owner::Turn(turn_id)).await
                    }
                    None => {
                        let id = self.speak_text(text.clone(), Owner::Turn(turn_id)).await;
                        if self.learnable(&text) {
                            self.learning
                                .insert(id, (self.voice.clone(), text.clone(), Vec::new()));
                        }
                        id
                    }
                };
                if let Some(t) = self.turn.as_mut() {
                    t.clauses.push((id, text));
                }
            }
            TurnEvent::SwitchLanguage(lang) => self.switch_on_request(lang).await,
            TurnEvent::Tone(tone) => self.set_tone(tone).await,
            TurnEvent::Line(kind) => {
                self.commit_user();
                let lines = self.deps.assets.lines(&self.language);
                let text = match kind {
                    LineKind::OneMoment => lines.one_moment.clone(),
                    LineKind::Apology => lines.apology.clone(),
                    LineKind::SendOnWhatsapp => lines.send_on_whatsapp.clone(),
                    LineKind::Checking => {
                        let n = lines.checking.len().max(1);
                        let text = lines.checking.get(self.checkings % n).cloned();
                        self.checkings += 1;
                        text.unwrap_or_default()
                    }
                };
                if text.trim().is_empty() {
                    return;
                }
                let id = self.speak_line(text.clone(), Owner::Turn(turn_id)).await;
                if let Some(t) = self.turn.as_mut() {
                    t.clauses.push((id, text));
                }
            }
            TurnEvent::Finished(summary) => {
                self.commit_user();
                for (model, usage) in &summary.usage {
                    self.add_llm_usage(model, *usage);
                }
                self.history.extend(summary.tool_messages.iter().cloned());
                for m in summary
                    .tool_messages
                    .iter()
                    .filter(|m| m.role == Role::Tool)
                {
                    let text: String = m
                        .content
                        .clone()
                        .unwrap_or_default()
                        .chars()
                        .take(500)
                        .collect();
                    self.record(Speaker::Tool, text, false, Utc::now());
                }
                let spoken = self
                    .turn
                    .as_ref()
                    .map(|t| {
                        t.clauses
                            .iter()
                            .map(|(_, c)| c.as_str())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                let index = (!spoken.is_empty()).then(|| {
                    self.history.push(ChatMessage::text(
                        Role::Assistant,
                        tagged(summary.tone, &spoken),
                    ));
                    self.history.len() - 1
                });
                let lines = self.deps.assets.lines(&self.language).clone();
                if summary.stop == Some(StopReason::CallBudget) {
                    self.speak_line(lines.budget_closing, Owner::Line).await;
                    self.begin_ending("tts_budget");
                } else if summary.end_call.is_some() {
                    if spoken.is_empty() {
                        let line = self.goodbye_for_block(lines.goodbye, &lines.short_goodbye);
                        self.speak_line(line, Owner::Line).await;
                    }
                    self.begin_ending("agent_end_call");
                }
                if let Some(t) = self.turn.as_mut() {
                    t.history_index = index;
                    t.summary = Some(summary);
                }
                self.check_quiet().await;
            }
        }
    }

    fn add_llm_usage(&mut self, model: &str, usage: common::assets::TokenUsage) {
        self.outcome
            .llm_usage
            .entry(model.to_owned())
            .or_default()
            .add(usage);
        for (kind, n) in [
            ("prompt", usage.prompt_tokens),
            ("cached", usage.cached_prompt_tokens),
            ("completion", usage.completion_tokens),
        ] {
            metrics::counter!(metric_names::LLM_TOKENS_TOTAL, "model" => model.to_owned(), "kind" => kind)
                .increment(n);
        }
    }

    // ------------------------------------------------------------ speech ----

    async fn play_clip(&mut self, audio: Bytes, owner: Owner) -> u64 {
        let id = self.next_clause;
        self.next_clause += 1;
        self.clause_owner.insert(id, owner);
        self.playout_idle = false;
        self.playout.play_clip(id, audio).await;
        id
    }

    /// Speaks a fixed line: cached audio if available, live TTS otherwise.
    async fn speak_line(&mut self, text: String, owner: Owner) -> u64 {
        match self.deps.lines.get(&self.voice, &text) {
            Some(audio) => self.play_clip(audio, owner).await,
            None => self.speak_text(text, owner).await,
        }
    }

    /// Sends text to TTS as one clause.
    async fn speak_text(&mut self, text: String, owner: Owner) -> u64 {
        let id = self.next_clause;
        self.next_clause += 1;
        self.clause_owner.insert(id, owner);
        self.playout_idle = false;
        self.playout.send(PlayoutCmd::Register(id)).await;
        let chars = text.chars().count();
        self.tts_chars_total += chars;
        *self
            .outcome
            .tts_chars
            .entry(self.voice.model.clone())
            .or_default() += chars as u64;
        metrics::counter!(metric_names::TTS_CHARS_TOTAL, "model" => self.voice.model.clone())
            .increment(chars as u64);
        self.tts_outstanding.insert(id);
        match &self.tts {
            Some(tts) => {
                if tts
                    .commands
                    .send(TtsCommand::Speak { id, text })
                    .await
                    .is_err()
                {
                    self.tts_backlog.push((id, String::new()));
                }
            }
            None => self.tts_backlog.push((id, text)),
        }
        id
    }

    async fn on_tts(&mut self, e: TtsEvent) {
        match e {
            TtsEvent::Audio { id, data } => {
                let Some(owner) = self.clause_owner.get(&id).copied() else {
                    return;
                };
                if let (Owner::Turn(tid), Some(t)) = (owner, self.turn.as_mut()) {
                    if t.id == tid && t.marks.tts_first.is_none() {
                        t.marks.tts_first = Some(Instant::now());
                    }
                }
                if let Some((_, _, buf)) = self.learning.get_mut(&id) {
                    buf.extend_from_slice(&data);
                }
                self.playout.send(PlayoutCmd::Audio(id, data)).await;
            }
            TtsEvent::Done { id } => {
                if self.tts_outstanding.remove(&id) {
                    self.playout.send(PlayoutCmd::End(id)).await;
                }
                if let Some((voice, text, audio)) = self.learning.remove(&id) {
                    if !audio.is_empty() {
                        self.deps.lines.learn(&voice, &text, Bytes::from(audio));
                    }
                }
                self.check_quiet().await;
            }
            TtsEvent::Error { message, fatal } => {
                tracing::warn!(call = %self.p.call_id, %message, fatal, "tts error");
                if fatal {
                    self.abandon_tts_outstanding().await;
                    if let Some(s) = self.tts.take() {
                        let _ = s.commands.send(TtsCommand::Close).await;
                    }
                    self.promote_spare().await;
                }
            }
        }
    }

    /// Whether a reply clause is worth caching: short, generic phrasing. Text
    /// with digits or the lead's name is call-specific, so it is skipped.
    fn learnable(&self, text: &str) -> bool {
        let name = self.p.lead_name.trim().to_lowercase();
        text.chars().count() <= LEARN_MAX_CHARS
            && !text.chars().any(|c| c.is_numeric())
            && (name.is_empty() || !text.to_lowercase().contains(&name))
    }

    /// Ends every clause still waiting for TTS (their audio will not come).
    async fn abandon_tts_outstanding(&mut self) {
        self.learning.clear();
        let ids: Vec<u64> = self.tts_outstanding.drain().collect();
        for id in ids {
            self.playout.send(PlayoutCmd::End(id)).await;
        }
        self.tts_backlog.clear();
        self.check_quiet().await;
    }

    fn open_spare(&mut self) {
        self.spare_opening += 1;
        let tts = self.deps.tts.clone();
        let voice = self.voice.clone();
        let tx = self.spare_tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(tts.open(&voice).await).await;
        });
    }

    async fn on_spare(&mut self, r: Result<TtsSession, ProviderError>) {
        self.spare_opening = self.spare_opening.saturating_sub(1);
        match r {
            Ok(session) if session.model != self.voice.model => {
                let _ = session.commands.send(TtsCommand::Close).await;
            }
            Ok(session) => {
                // Language may have changed while it was opening.
                let _ = session
                    .commands
                    .send(TtsCommand::Configure(self.voice.clone()))
                    .await;
                if self.tts.is_none() {
                    self.tts = Some(session);
                    self.flush_backlog().await;
                } else if self.tts_spare.is_none() {
                    self.tts_spare = Some(session);
                } else {
                    let _ = session.commands.send(TtsCommand::Close).await;
                }
            }
            Err(e) => {
                tracing::warn!(call = %self.p.call_id, error = %e, "tts open failed");
                if self.tts.is_none() && self.spare_opening == 0 && !self.done {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    self.open_spare();
                }
            }
        }
    }

    async fn promote_spare(&mut self) {
        if let Some(spare) = self.tts_spare.take() {
            self.tts = Some(spare);
            self.flush_backlog().await;
        } else if self.spare_opening == 0 {
            self.open_spare();
        }
        if self.tts_spare.is_none() {
            self.open_spare();
        }
    }

    async fn flush_backlog(&mut self) {
        let backlog = std::mem::take(&mut self.tts_backlog);
        let Some(tts) = &self.tts else {
            self.tts_backlog = backlog;
            return;
        };
        for (id, text) in backlog {
            if text.is_empty() {
                continue;
            }
            let _ = tts.commands.send(TtsCommand::Speak { id, text }).await;
        }
    }

    async fn on_playout(&mut self, e: PlayoutEvent) {
        match e {
            PlayoutEvent::Started(id) => {
                self.started_clauses.insert(id);
                match self.clause_owner.get(&id).copied() {
                    Some(Owner::Greeting) => {
                        if self.outcome.greeting_latency_ms.is_none() {
                            let ms = ms_between(Instant::now(), self.stream_started);
                            self.outcome.greeting_latency_ms = Some(ms);
                            metrics::histogram!(metric_names::GREETING_LATENCY_MS)
                                .record(ms as f64);
                        }
                    }
                    Some(Owner::Turn(tid)) => {
                        if let Some(t) = self.turn.as_mut().filter(|t| t.id == tid) {
                            if t.marks.first_sent.is_none() {
                                t.marks.first_sent = Some(Instant::now());
                            }
                            if self.phase != Phase::Ending {
                                self.phase = Phase::Speaking;
                            }
                        }
                    }
                    Some(Owner::GreetingBody) => {
                        if let Some(g) = self.greeting_parts.as_mut() {
                            g.body_started = true;
                        }
                        if self.phase == Phase::Greeting {
                            self.phase = Phase::Speaking;
                        }
                        self.release_greeting_turn().await;
                    }
                    Some(Owner::Line) if self.phase != Phase::Ending => {
                        self.phase = Phase::Speaking;
                    }
                    Some(Owner::Line) | None => {}
                }
            }
            PlayoutEvent::Gap(id) => {
                metrics::counter!("voice_playout_gaps_total").increment(1);
                tracing::info!(call = %self.p.call_id, clause = id, "audio gap: speech ran out mid-clause");
            }
            PlayoutEvent::Drained => {
                self.playout_idle = true;
                if self.greeting_parts.as_ref().is_some_and(|g| g.body_started) {
                    // The greeting body played in full.
                    self.greeting_parts = None;
                }
                self.check_quiet().await;
            }
        }
    }

    /// Called whenever speech might have finished: completes the turn and
    /// moves on (listen, or hang up) once nothing is left to say.
    async fn check_quiet(&mut self) {
        if !self.playout_idle || !self.tts_outstanding.is_empty() {
            return;
        }
        if self.turn.as_ref().is_some_and(|t| t.wait_greeting) {
            // The greeting ended without a body clip starting.
            Box::pin(self.release_greeting_turn()).await;
            return;
        }
        if let Some(t) = &self.turn {
            if t.speculative || t.summary.is_none() {
                return;
            }
        }
        if let Some(t) = self.turn.take() {
            self.finalize_turn(t, None);
        }
        match self.after.clone() {
            After::Hangup(reason) => self.hangup(reason).await,
            After::Listen => {
                if matches!(
                    self.phase,
                    Phase::Greeting | Phase::Thinking | Phase::Speaking
                ) {
                    self.phase = Phase::Listening;
                    if !self.user_speaking {
                        self.silence_deadline = Some(Instant::now() + self.s.silence_reprompt);
                    }
                }
            }
        }
    }

    /// Records the agent's side of a turn. `played` overrides the text
    /// with what was actually heard (barge-in).
    fn finalize_turn(&mut self, t: ActiveTurn, played: Option<String>) {
        if !t.user_committed {
            return;
        }
        let summary = t.summary.unwrap_or_default();
        let interrupted = played.is_some();
        let spoken: String = t
            .clauses
            .iter()
            .map(|(_, c)| c.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let text = played.unwrap_or_else(|| spoken.clone());
        if interrupted {
            let content = tagged(summary.tone, &text);
            match t.history_index {
                Some(i) if i < self.history.len() => {
                    if text.is_empty() {
                        self.history.remove(i);
                    } else {
                        self.history[i].content = Some(content);
                    }
                }
                _ if !text.is_empty() => self
                    .history
                    .push(ChatMessage::text(Role::Assistant, content)),
                _ => {}
            }
        }
        if !text.is_empty() || interrupted {
            self.record(Speaker::Assistant, text.clone(), interrupted, t.started_at);
        }

        let spoken_chars = spoken.chars().count();
        let cap = summary.stop == Some(StopReason::TurnCap);
        metrics::histogram!(metric_names::SPOKEN_CHARS_PER_TURN).record(spoken_chars as f64);
        if cap {
            metrics::counter!(metric_names::SPOKEN_CAP_TRIGGERED_TOTAL).increment(1);
        }
        let m = &t.marks;
        // Speech during the greeting waits for it (the disclosure must be
        // heard); measure from the greeting's end so latency stays honest.
        let base = match (m.speech_end, self.greeting_end) {
            (Some(end), Some(g)) => Some(end.max(g)),
            (end, _) => end,
        };
        let offset = |x: Option<Instant>| match (x, base) {
            (Some(x), Some(b)) => Some(ms_between(x, b)),
            _ => None,
        };
        let latency = TurnLatency {
            turn_seq: self.seq,
            speech_end_ms: base.map(|b| ms_between(b, self.stream_started)),
            stt_final_ms: offset(m.stt_final),
            llm_first_token_ms: offset(m.first_token),
            tts_first_byte_ms: offset(m.tts_first),
            first_audio_sent_ms: offset(m.first_sent),
            spoken_chars: spoken_chars as i32,
            cap_triggered: cap,
            llm_model: m.model.clone().or(summary.model.clone()),
            llm_attempts: summary.attempts.max(1),
        };
        let model = latency.llm_model.clone().unwrap_or_default();
        for (stage, value, provider) in [
            (
                "stt_final",
                latency.stt_final_ms,
                self.deps.stt.name().to_owned(),
            ),
            ("llm_first_token", latency.llm_first_token_ms, model.clone()),
            (
                "tts_first_audio",
                latency.tts_first_byte_ms,
                self.deps.tts.name().to_owned(),
            ),
            (
                "first_audio_sent",
                latency.first_audio_sent_ms,
                "pipeline".to_owned(),
            ),
        ] {
            if let Some(v) = value {
                metrics::histogram!(metric_names::TURN_STAGE_MS, "stage" => stage, "provider" => provider)
                    .record(v as f64);
            }
        }
        tracing::info!(
            call = %self.p.call_id,
            turn = latency.turn_seq,
            stt_final_ms = ?latency.stt_final_ms,
            llm_first_token_ms = ?latency.llm_first_token_ms,
            tts_first_byte_ms = ?latency.tts_first_byte_ms,
            voice_to_voice_ms = ?latency.first_audio_sent_ms,
            spoken_chars,
            interrupted,
            model = %model,
            "turn complete"
        );
        self.outcome.latencies.push(latency);
    }

    fn record(
        &mut self,
        role: Speaker,
        text: String,
        interrupted: bool,
        started_at: chrono::DateTime<Utc>,
    ) {
        self.seq += 1;
        tracing::info!(call = %self.p.call_id, seq = self.seq, role = role.as_str(), %text, interrupted, "transcript");
        self.outcome.turns.push(TurnRecord {
            seq: self.seq,
            role,
            text,
            language: Some(self.language.clone()),
            interrupted,
            started_at,
            ended_at: Some(Utc::now()),
            audio: None,
        });
    }

    /// Caller interrupted: stop audio, cancel generation, keep what was heard.
    async fn barge_in(&mut self) {
        if matches!(self.phase, Phase::Greeting | Phase::Ending) {
            return;
        }
        metrics::counter!("voice_barge_ins_total").increment(1);
        let report = self.stop_audio().await;
        if let Some(t) = self.turn.take() {
            t.cancel.cancel();
            let played = played_text(&t.clauses, &report, &self.started_clauses);
            let ran_tools = t
                .summary
                .as_ref()
                .is_some_and(|s| !s.tool_messages.is_empty());
            if t.user_committed
                && !ran_tools
                && played.trim_end_matches('…').trim().chars().count() < CONTINUATION_MAX_CHARS
            {
                self.resume_user(t);
            } else {
                self.finalize_turn(t, Some(played));
            }
        }
        self.clause_owner.clear();
        self.started_clauses.clear();
        self.after = After::Listen;
        self.phase = Phase::Listening;
        self.silence_deadline = None;
    }

    /// Stops everything playing or being synthesized and reports how much
    /// of each clause was heard.
    async fn stop_audio(&mut self) -> Vec<PlayedClause> {
        let report = self.playout.clear().await;
        self.playout_idle = true;
        if !self.tts_outstanding.is_empty() {
            // Sarvam TTS has no cancel: drop the busy socket, use the spare.
            self.tts_outstanding.clear();
            self.tts_backlog.clear();
            self.learning.clear();
            if let Some(s) = self.tts.take() {
                let _ = s.commands.send(TtsCommand::Close).await;
            }
            self.promote_spare().await;
        }
        self.note_greeting_cut(&report);
        report
    }

    /// The caller kept talking right after the agent started answering: take
    /// the user message back out of the history and transcript, and hold its
    /// text so the next final transcript is joined onto it.
    fn resume_user(&mut self, t: ActiveTurn) {
        if let Some(i) = t.history_index.filter(|&i| i < self.history.len()) {
            self.history.remove(i);
        }
        if let Some(i) = t.user_history_index.filter(|&i| i < self.history.len()) {
            self.history.remove(i);
        }
        if self
            .outcome
            .turns
            .last()
            .is_some_and(|r| r.seq == self.seq && r.role == Speaker::User)
        {
            if let Some(audio) = self.outcome.turns.pop().and_then(|r| r.audio) {
                // Its audio goes back in front of what the caller says next.
                let mut merged = audio.to_vec();
                merged.extend_from_slice(&self.user_audio);
                merged.truncate(USER_AUDIO_MAX_BYTES);
                self.user_audio = merged;
            }
            self.seq -= 1;
        }
        metrics::counter!("voice_turn_continuations_total").increment(1);
        tracing::debug!(call = %self.p.call_id, "caller continued; merging utterances");
        self.pending_user = join_text(&t.user_text, &self.pending_user);
    }

    // ----------------------------------------------------------- billing ----

    /// Time left in the current paid telephony block, if billing-aware.
    fn pulse_remaining(&self) -> Option<Duration> {
        let pulse = self.s.billing_pulse;
        if pulse.is_zero() {
            return None;
        }
        let elapsed = self.stream_started.elapsed() + ANSWER_TO_STREAM;
        let into = Duration::from_millis((elapsed.as_millis() % pulse.as_millis()) as u64);
        Some(pulse - into)
    }

    /// Moves the max-duration closing so the closing line ends inside a
    /// paid block: it starts `pulse_wrap_up` before a block boundary.
    fn align_call_deadline(&mut self) {
        let pulse = self.s.billing_pulse;
        if pulse.is_zero() || self.p.max_duration < pulse {
            return;
        }
        let blocks = (self.p.max_duration.as_millis() / pulse.as_millis()) as u32;
        let end = (pulse * blocks)
            .saturating_sub(self.s.pulse_wrap_up)
            .saturating_sub(ANSWER_TO_STREAM);
        self.call_deadline = self.stream_started + end.min(self.p.max_duration);
    }

    /// The normal goodbye, or the short one when the normal one would
    /// spill into the next paid block.
    fn goodbye_for_block(&self, normal: String, short: &str) -> String {
        let Some(left) = self.pulse_remaining() else {
            return normal;
        };
        if short.trim().is_empty() {
            return normal;
        }
        let spoken = |text: &str| match self.deps.lines.get(&self.voice, text) {
            Some(audio) => Duration::from_millis(audio::mulaw_duration_ms(audio.len()) as u64),
            None => Duration::from_millis(text.chars().count() as u64 * 70),
        };
        let margin = Duration::from_millis(700);
        if spoken(&normal) + margin > left && spoken(short) + margin <= left {
            metrics::counter!("voice_short_goodbyes_total").increment(1);
            short.to_owned()
        } else {
            normal
        }
    }

    // ------------------------------------------------------------ ending ----

    fn begin_ending(&mut self, reason: &'static str) {
        self.phase = Phase::Ending;
        self.after = After::Hangup(reason);
        self.silence_deadline = None;
        self.hard_deadline = Some(Instant::now() + ENDING_TIMEOUT);
    }

    /// Interrupts whatever is happening, says `line`, then hangs up.
    async fn end_with_line(&mut self, line: String, reason: &'static str) {
        if self.phase == Phase::Ending {
            return;
        }
        if self.turn.is_some() || !self.playout_idle {
            let phase = self.phase;
            self.phase = Phase::Speaking;
            self.barge_in().await;
            self.phase = phase;
        }
        self.begin_ending(reason);
        self.speak_line(line, Owner::Line).await;
    }

    async fn on_timer(&mut self) {
        let now = Instant::now();
        if self.hard_deadline.is_some_and(|d| now >= d) {
            self.hangup("ending_timeout").await;
            return;
        }
        if now >= self.call_deadline && self.phase != Phase::Ending {
            let line = self
                .deps
                .assets
                .lines(&self.language)
                .max_duration_closing
                .clone();
            self.end_with_line(line, "max_duration").await;
            return;
        }
        if self.ack_deadline.is_some_and(|d| now >= d) {
            self.ack_deadline = None;
            let ready = matches!(self.phase, Phase::Listening | Phase::Speaking)
                && !self.user_speaking
                && self.turn.is_none()
                && !self.pending_user.is_empty();
            if ready {
                let text = std::mem::take(&mut self.pending_user);
                // Latency counts from the end of the deliberate wait.
                self.speech_end_at = Some(now);
                self.stt_final_at = Some(now);
                self.start_turn(text, false).await;
            }
            return;
        }
        if self.silence_deadline.is_some_and(|d| now >= d) {
            self.silence_deadline = None;
            if self.phase != Phase::Listening || self.user_speaking || self.turn.is_some() {
                return;
            }
            self.silence_strikes += 1;
            let lines = self.deps.assets.lines(&self.language).clone();
            if self.silence_strikes == 1 {
                self.after = After::Listen;
                self.phase = Phase::Speaking;
                self.speak_line(lines.reprompt, Owner::Line).await;
            } else {
                self.begin_ending("silence");
                let line = self.goodbye_for_block(lines.silence_goodbye, &lines.short_goodbye);
                self.speak_line(line, Owner::Line).await;
            }
        }
    }

    async fn hangup(&mut self, reason: &'static str) {
        if self.done {
            return;
        }
        if self.outcome.end_reason.is_empty() {
            self.outcome.end_reason = reason.to_owned();
        }
        self.control.hangup().await;
        let _ = self.media_out.send(MediaOut::Close).await;
        self.done = true;
    }

    async fn finish(&mut self) {
        if let Some(t) = self.turn.take() {
            t.cancel.cancel();
            if !t.clauses.is_empty() {
                let report: Vec<PlayedClause> = Vec::new();
                let played = played_text(&t.clauses, &report, &self.started_clauses);
                self.finalize_turn(t, Some(played));
            }
        }
        for s in [self.tts.take(), self.tts_spare.take()]
            .into_iter()
            .flatten()
        {
            let _ = s.commands.send(TtsCommand::Close).await;
        }
        if let Some(stt) = self.stt.take() {
            if let Some(billed) = stt.close(Duration::from_millis(1500)).await {
                self.stt_billed += billed;
            }
        }
        let streamed = self.stt_bytes as f64 / (FRAME_SAMPLES as f64 * 50.0);
        // A language switch replaces the session, and a replaced session
        // never reports its billed seconds, so never count less than was sent.
        self.outcome.stt_seconds = self.stt_billed.max(streamed);
        metrics::counter!(metric_names::STT_BILLED_MS_TOTAL)
            .increment((self.outcome.stt_seconds * 1000.0) as u64);
        if self.outcome.end_reason.is_empty() {
            self.outcome.end_reason = "completed".to_owned();
        }
    }
}

fn join_text(a: &str, b: &str) -> String {
    match (a.trim(), b.trim()) {
        ("", b) => b.to_owned(),
        (a, "") => a.to_owned(),
        (a, b) => format!("{a} {b}"),
    }
}

/// What the caller heard of a turn's clauses, given the playout report.
fn played_text(
    clauses: &[(u64, String)],
    report: &[PlayedClause],
    started: &HashSet<u64>,
) -> String {
    let mut parts = Vec::new();
    for (id, text) in clauses {
        let fraction = match report.iter().find(|r| r.id == *id) {
            Some(r) => r.fraction,
            None if started.contains(id) => 1.0,
            None => break,
        };
        if fraction >= 0.98 {
            parts.push(text.clone());
            continue;
        }
        if fraction > 0.15 {
            let total = text.chars().count();
            let keep = ((total as f32) * fraction) as usize;
            let prefix: String = text.chars().take(keep).collect();
            let cut = prefix
                .rfind(char::is_whitespace)
                .map_or(prefix.as_str(), |i| &prefix[..i]);
            if !cut.trim().is_empty() {
                parts.push(format!("{}…", cut.trim_end()));
            }
        }
        break;
    }
    parts.join(" ")
}

/// The language a reply's Indian script implies, when it differs from
/// `current`. Latin text never switches (it may be romanized); Devanagari
/// keeps Marathi if the call is in Marathi.
fn reply_language(text: &str, current: &str) -> Option<&'static str> {
    let lang = text.chars().find_map(|c| match c {
        '\u{0900}'..='\u{097F}' if current.starts_with("mr") => Some("mr-IN"),
        '\u{0900}'..='\u{097F}' => Some("hi-IN"),
        '\u{0980}'..='\u{09FF}' => Some("bn-IN"),
        '\u{0A00}'..='\u{0A7F}' => Some("pa-IN"),
        '\u{0A80}'..='\u{0AFF}' => Some("gu-IN"),
        '\u{0B00}'..='\u{0B7F}' => Some("od-IN"),
        '\u{0B80}'..='\u{0BFF}' => Some("ta-IN"),
        '\u{0C00}'..='\u{0C7F}' => Some("te-IN"),
        '\u{0C80}'..='\u{0CFF}' => Some("kn-IN"),
        '\u{0D00}'..='\u{0D7F}' => Some("ml-IN"),
        _ => None,
    })?;
    (lang != current).then_some(lang)
}

/// A reply as the model wrote it, with its tone tag, so the history keeps
/// showing the model the format it must follow.
fn tagged(tone: Option<Tone>, text: &str) -> String {
    match tone {
        Some(tone) => format!("[{}] {text}", tone.as_str()),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_script_picks_the_language() {
        assert_eq!(reply_language("సరే, ఏ రోజు?", "en-IN"), Some("te-IN"));
        assert_eq!(reply_language("சரி", "ta-IN"), None);
        assert_eq!(reply_language("Sure, which day?", "ta-IN"), None);
        assert_eq!(reply_language("ठीक है", "mr-IN"), None);
        assert_eq!(reply_language("ठीक है", "en-IN"), Some("hi-IN"));
        assert_eq!(reply_language("Okay, ಸರಿ", "en-IN"), Some("kn-IN"));
    }

    #[test]
    fn backchannels_are_recognized_in_several_languages() {
        for t in [
            "Yeah",
            "Okay.",
            "yeah okay sure",
            "Hello?",
            "haan ji",
            "ಹೌದು",
            "சரி",
            "हाँ जी",
            "ಹಾಂ",
            "சரிங்க",
            "అలాగే",
            "ঠিক আছে",
            "ഉം",
        ] {
            assert!(is_backchannel(t), "{t}");
        }
        for t in [
            "No",
            "wait wait",
            "Tomorrow",
            "yeah but what about Friday",
            "",
        ] {
            assert!(!is_backchannel(t), "{t}");
        }
    }

    #[test]
    fn played_text_keeps_heard_prefix() {
        let clauses = vec![
            (1, "Sure, we help clinics.".to_owned()),
            (2, "Which matters most to you right now?".to_owned()),
            (3, "Never started.".to_owned()),
        ];
        let started: HashSet<u64> = [1, 2].into_iter().collect();
        let report = vec![PlayedClause {
            id: 2,
            fraction: 0.5,
        }];
        let heard = played_text(&clauses, &report, &started);
        assert!(heard.starts_with("Sure, we help clinics. Which matters"));
        assert!(heard.ends_with('…'));
        assert!(!heard.contains("Never"));
    }

    #[test]
    fn normalize_ignores_case_and_punctuation() {
        assert_eq!(normalize("Hello, World!"), normalize("hello world"));
        assert_eq!(join_text("", " hi "), "hi");
        assert_eq!(join_text("I want", "a demo"), "I want a demo");
    }
}
