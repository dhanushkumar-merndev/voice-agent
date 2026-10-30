//! End-to-end pipeline tests with fake STT/LLM/TTS and an energy VAD.

#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::{
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use async_trait::async_trait;
use bytes::Bytes;
use common::assets::{Assets, Voice};
use pipeline::{
    vad::{EnergyFactory, VadPool},
    CallCommand, CallControl, CallOutcome, CallParams, Deps, LineAudio, MediaIn, MediaOut,
    PipelineSettings, Prewarmed, Speaker, ToolBackend,
};
use providers::{
    fake::{FakeLlm, FakeReply, FakeStt, FakeTts},
    llm::LlmEndpoint,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct NoLines;
impl LineAudio for NoLines {
    fn get(&self, _voice: &Voice, _text: &str) -> Option<Bytes> {
        None
    }
}

/// An in-memory phrase cache that learns, like the server's `LineCache`.
#[derive(Default)]
struct MemoryLines(Mutex<std::collections::HashMap<String, Bytes>>);
impl MemoryLines {
    fn has(&self, text: &str) -> bool {
        self.0.lock().unwrap().contains_key(text)
    }
}
impl LineAudio for MemoryLines {
    fn get(&self, _voice: &Voice, text: &str) -> Option<Bytes> {
        self.0.lock().unwrap().get(text).cloned()
    }
    fn learn(&self, _voice: &Voice, text: &str, audio: Bytes) {
        self.0.lock().unwrap().insert(text.to_owned(), audio);
    }
}

#[derive(Default)]
struct Tools {
    followups: Mutex<Vec<String>>,
}
#[async_trait]
impl ToolBackend for Tools {
    async fn search_knowledge(&self, _query: &str) -> Result<Vec<String>, String> {
        Ok(vec!["Ads start at 10,000 rupees a month.".into()])
    }
    async fn send_details(&self, topic: &str) -> Result<(), String> {
        self.followups.lock().unwrap().push(topic.to_owned());
        Ok(())
    }
}

#[derive(Default)]
struct Control {
    hangups: AtomicUsize,
}
#[async_trait]
impl CallControl for Control {
    async fn hangup(&self) {
        self.hangups.fetch_add(1, Ordering::SeqCst);
    }
}

fn assets() -> Arc<Assets> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    Arc::new(Assets::load(&root.join("prompts"), &root.join("config")).unwrap())
}

fn settings() -> PipelineSettings {
    PipelineSettings {
        vad_silence: Duration::from_millis(400),
        vad_threshold: 0.5,
        barge_in_min: Duration::from_millis(120),
        barge_in_threshold: 0.5,
        barge_in_talk: Duration::from_millis(300),
        eager_llm: false,
        silence_reprompt: Duration::from_secs(30),
        ack_wait: Duration::ZERO,
        late_reply: Duration::ZERO,
        max_spoken_chars_per_turn: 250,
        max_sentences_per_turn: 0,
        max_tts_chars_per_call: 4000,
        llm_primary: LlmEndpoint {
            url: "http://primary".into(),
            model: "sarvam-105b-conversations".into(),
        },
        llm_fallback: LlmEndpoint {
            url: "http://fallback".into(),
            model: "gemma4".into(),
        },
        llm_first_token_timeout: Duration::from_millis(300),
        llm_hedge_after: None,
        llm_max_tokens: 80,
        tts_default_model: "bulbul:v4-flash".into(),
        playout_lead: Duration::from_millis(60),
        billing_pulse: Duration::ZERO,
        pulse_wrap_up: Duration::from_secs(5),
    }
}

fn params() -> CallParams {
    CallParams {
        call_id: "test-call".into(),
        lead_name: "Ravi".into(),
        system_prompt: "You sell clinic marketing.".into(),
        kb_context: None,
        has_kb: true,
        has_booking: false,
        today: "Wednesday, 2026-10-28".into(),
        fixed_language: Some("en-IN".into()),
        initial_language: "en-IN".into(),
        voice_override: None,
        tts_model: None,
        llm_primary: None,
        greeting_text: "Hello, this is an AI assistant. Am I speaking with Ravi?".into(),
        disclosure_text: String::new(),
        disclosure_audio: None,
        greeting_audio: None,
        max_duration: Duration::from_secs(60),
    }
}

struct Harness {
    media: mpsc::Sender<MediaIn>,
    commands: mpsc::Sender<CallCommand>,
    out: Arc<Mutex<Vec<MediaOut>>>,
    task: tokio::task::JoinHandle<CallOutcome>,
    stt: FakeStt,
    tts: FakeTts,
    llm: FakeLlm,
    tools: Arc<Tools>,
    control: Arc<Control>,
}

fn start(params: CallParams, settings: PipelineSettings, stt: FakeStt, llm: FakeLlm) -> Harness {
    // Stream TTS audio at 2x real time, like a real provider (not all at once).
    let tts = FakeTts {
        chunk_delay: Duration::from_millis(10),
        ..FakeTts::default()
    };
    start_with_tts(params, settings, stt, llm, tts, Arc::new(NoLines))
}

fn start_with_tts(
    params: CallParams,
    settings: PipelineSettings,
    stt: FakeStt,
    llm: FakeLlm,
    tts: FakeTts,
    lines: Arc<dyn LineAudio>,
) -> Harness {
    let deps = Deps {
        stt: Arc::new(stt.clone()),
        tts: Arc::new(tts.clone()),
        llm: Arc::new(llm.clone()),
        vad: Arc::new(VadPool::new(&EnergyFactory, 1).unwrap()),
        assets: assets(),
        lines,
    };
    let tools = Arc::new(Tools::default());
    let control = Arc::new(Control::default());
    let (media_tx, media_rx) = mpsc::channel(4096);
    let (out_tx, mut out_rx) = mpsc::channel(4096);
    let (cmd_tx, cmd_rx) = mpsc::channel(4);
    let out = Arc::new(Mutex::new(Vec::new()));
    let sink = out.clone();
    tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            sink.lock().unwrap().push(m);
        }
    });
    let task = tokio::spawn(pipeline::run_call(
        params,
        Arc::new(settings),
        deps,
        tools.clone(),
        control.clone(),
        Prewarmed::default(),
        media_rx,
        out_tx,
        cmd_rx,
        CancellationToken::new(),
    ));
    Harness {
        media: media_tx,
        commands: cmd_tx,
        out,
        task,
        stt,
        tts,
        llm,
        tools,
        control,
    }
}

fn loud_frame() -> Bytes {
    Bytes::from(
        (0..160)
            .map(|i| audio::mulaw::encode(if i % 2 == 0 { 9000 } else { -9000 }))
            .collect::<Vec<u8>>(),
    )
}

fn quiet_frame() -> Bytes {
    Bytes::from(vec![audio::mulaw::SILENCE; 160])
}

impl Harness {
    async fn begin(&self) {
        self.media.send(MediaIn::Start).await.unwrap();
    }

    /// Sends `ms` of audio at real-time pace.
    async fn audio(&self, loud: bool, ms: u64) {
        let frame = if loud { loud_frame() } else { quiet_frame() };
        for _ in 0..ms / 20 {
            if self
                .media
                .send(MediaIn::Audio(frame.clone()))
                .await
                .is_err()
            {
                return; // the call ended
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Sends silence until `cond` holds or `timeout` passes.
    async fn silence_until(&self, timeout: Duration, cond: impl Fn(&Self) -> bool) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < deadline {
            if cond(self) {
                return true;
            }
            self.audio(false, 40).await;
        }
        cond(self)
    }

    fn spoken(&self) -> Vec<String> {
        self.tts.spoken()
    }

    fn cleared(&self) -> bool {
        self.out
            .lock()
            .unwrap()
            .iter()
            .any(|m| matches!(m, MediaOut::Clear))
    }

    fn closed(&self) -> bool {
        self.out
            .lock()
            .unwrap()
            .iter()
            .any(|m| matches!(m, MediaOut::Close))
    }

    async fn hangup_and_wait(self) -> (CallOutcome, Self) {
        let _ = self.commands.send(CallCommand::Hangup).await;
        self.finish().await
    }

    async fn finish(mut self) -> (CallOutcome, Self) {
        let outcome = tokio::time::timeout(Duration::from_secs(5), &mut self.task)
            .await
            .expect("call ended")
            .unwrap();
        (outcome, self)
    }
}

#[tokio::test]
async fn greeting_then_one_turn_with_latency_and_gated_stt() {
    let stt = FakeStt::with_script(&["I want more patients"]);
    let llm = FakeLlm::with_script(vec![FakeReply::Text(
        "Sure, we can help with ads and bookings. Which matters most to you?".into(),
    )]);
    let h = start(params(), settings(), stt, llm);
    h.begin().await;
    assert!(
        h.silence_until(Duration::from_secs(3), |h| !h.spoken().is_empty())
            .await
    );
    assert!(h.spoken()[0].starts_with("Hello, this is an AI assistant"));

    h.audio(false, 400).await;
    h.audio(true, 600).await;
    let replied = h
        .silence_until(Duration::from_secs(4), |h| h.spoken().len() >= 3)
        .await;
    assert!(replied, "spoken: {:?}", h.spoken());
    // Let the reply finish playing.
    h.audio(false, 1200).await;

    let total_audio_bytes = (400 + 600 + 1200) * 8;
    let (outcome, h) = h.hangup_and_wait().await;
    assert_eq!(outcome.end_reason, "api_hangup");
    assert_eq!(h.control.hangups.load(Ordering::SeqCst), 1);

    let roles: Vec<_> = outcome.turns.iter().map(|t| t.role).collect();
    assert_eq!(
        roles,
        vec![Speaker::Assistant, Speaker::User, Speaker::Assistant]
    );
    assert_eq!(outcome.turns[1].text, "I want more patients");
    assert!(outcome.turns[2].text.starts_with("Sure, we can help"));
    assert!(!outcome.turns[2].interrupted);

    assert_eq!(outcome.latencies.len(), 1);
    let l = &outcome.latencies[0];
    assert!(l.stt_final_ms.is_some() && l.llm_first_token_ms.is_some());
    assert!(l.first_audio_sent_ms.unwrap() < 1000, "{l:?}");
    assert_eq!(l.llm_model.as_deref(), Some("sarvam-105b-conversations"));
    assert!(outcome.greeting_latency_ms.is_some());

    // STT only received the utterance (plus pre-roll and trailing silence), not the whole call.
    let sent = h.stt.audio_bytes.load(Ordering::Relaxed);
    assert!(sent > 600 * 8 / 2, "sent {sent}");
    // 300 ms pre-roll + 600 ms speech + 400 ms VAD tail, well below the whole call.
    assert!(sent <= (300 + 600 + 500) * 8, "sent {sent}");
    assert!(
        sent < total_audio_bytes,
        "sent {sent} of {total_audio_bytes}"
    );
    assert!(outcome.stt_seconds > 0.0);
    assert!(
        outcome
            .tts_chars
            .get("bulbul:v4-flash")
            .copied()
            .unwrap_or(0)
            > 50
    );
    assert!(outcome.llm_usage.contains_key("sarvam-105b-conversations"));
}

#[tokio::test]
async fn barge_in_clears_audio_and_keeps_only_played_text() {
    let stt = FakeStt::with_script(&["Tell me everything", "Stop, what is the price"]);
    let long = "This is a long answer that keeps going for quite a while. ".repeat(4);
    let llm = FakeLlm::with_script(vec![
        FakeReply::Text(long),
        FakeReply::Text("It starts at ten thousand a month.".into()),
    ]);
    // Interrupt quickly, while the long answer is still being synthesized.
    let mut s = settings();
    s.barge_in_talk = Duration::from_millis(120);
    let h = start(params(), s, stt, llm);
    h.begin().await;
    h.silence_until(Duration::from_secs(3), |h| !h.spoken().is_empty())
        .await;
    h.audio(false, 2500).await; // greeting plays out
    h.audio(true, 500).await;
    assert!(
        h.silence_until(Duration::from_secs(3), |h| h.spoken().len() >= 2)
            .await
    );
    h.audio(false, 300).await; // agent is speaking
    h.audio(true, 400).await; // caller interrupts
    assert!(h.cleared(), "no clearAudio sent");
    h.silence_until(Duration::from_secs(4), |h| {
        h.spoken().iter().any(|s| s.contains("ten thousand"))
    })
    .await;
    h.audio(false, 600).await;
    let (outcome, h) = h.hangup_and_wait().await;

    let interrupted: Vec<_> = outcome.turns.iter().filter(|t| t.interrupted).collect();
    assert_eq!(interrupted.len(), 1, "{:#?}", outcome.turns);
    let heard = &interrupted[0].text;
    assert!(heard.len() < 200, "kept too much: {heard}");
    // A replacement TTS socket was opened after the busy one was dropped.
    assert!(h.tts.opened.load(Ordering::Relaxed) >= 3);
    assert!(outcome
        .turns
        .iter()
        .any(|t| t.role == Speaker::User && t.text == "Stop, what is the price"));
}

#[tokio::test]
async fn caller_pausing_mid_sentence_is_merged_into_one_turn() {
    let stt = FakeStt::with_script(&["Yeah", "I need a better CRM"]);
    let llm = FakeLlm::with_script(vec![
        FakeReply::Text(
            "Okay, let me explain everything we offer for clinics in detail. ".repeat(3),
        ),
        FakeReply::Text("Sure, we can automate that for you.".into()),
    ]);
    // Speech at a real pace (~60 ms per character), delivered ~10x faster
    // than real time like Bulbul, so being cut off after ~200 ms means only
    // a word or two was heard.
    let tts = FakeTts {
        chunk_delay: Duration::from_millis(2),
        audio_per_char: Duration::from_millis(60),
        ..FakeTts::default()
    };
    let h = start_with_tts(params(), settings(), stt, llm, tts, Arc::new(NoLines));
    h.begin().await;
    h.silence_until(Duration::from_secs(3), |h| !h.spoken().is_empty())
        .await;
    h.audio(false, 2500).await; // greeting plays out
    h.audio(true, 300).await; // "Yeah", then a pause
    assert!(
        h.silence_until(Duration::from_secs(3), |h| h.spoken().len() >= 2)
            .await
    );
    h.audio(true, 400).await; // caller carries on right away
    assert!(h.cleared(), "no clearAudio sent");
    h.silence_until(Duration::from_secs(4), |h| {
        h.spoken().iter().any(|s| s.contains("automate"))
    })
    .await;
    h.audio(false, 600).await;
    let (outcome, h) = h.hangup_and_wait().await;

    let requests: Vec<String> = h
        .llm
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|(_, u)| u.clone())
        .collect();
    assert_eq!(
        requests.last().map(String::as_str),
        Some("Yeah I need a better CRM"),
        "{:#?}",
        outcome.turns
    );
    let users: Vec<&str> = outcome
        .turns
        .iter()
        .filter(|t| t.role == Speaker::User)
        .map(|t| t.text.as_str())
        .collect();
    assert_eq!(users, ["Yeah I need a better CRM"], "{:#?}", outcome.turns);
    // Both utterances (300 ms + 400 ms of speech) stay with the merged turn.
    let merged = outcome
        .turns
        .iter()
        .find(|t| t.role == Speaker::User)
        .and_then(|t| t.audio.as_ref())
        .map_or(0, |a| a.len());
    assert!(merged >= 700 * 8, "merged turn has {merged} bytes of audio");
    // The reply the caller talked over is dropped, not kept as a fragment.
    assert!(
        outcome.turns.iter().all(|t| !t.text.contains("explain")),
        "{:#?}",
        outcome.turns
    );
}

#[tokio::test]
async fn short_phrases_are_learned_and_then_played_from_cache() {
    let stt = FakeStt::with_script(&["I run a clinic", "We need more patients"]);
    let llm = FakeLlm::with_script(vec![
        FakeReply::Text("Got it. What kind of clinic is it?".into()),
        FakeReply::Text("Got it. Do you run ads today?".into()),
    ]);
    let tts = FakeTts {
        chunk_delay: Duration::from_millis(2),
        ..FakeTts::default()
    };
    let lines = Arc::new(MemoryLines::default());
    let h = start_with_tts(params(), settings(), stt, llm, tts, lines.clone());
    h.begin().await;
    h.silence_until(Duration::from_secs(3), |h| !h.spoken().is_empty())
        .await;
    h.audio(false, 2500).await; // greeting plays out
    for _ in 0..2 {
        h.audio(true, 500).await;
        h.audio(false, 2500).await; // the reply plays out
    }
    let (outcome, h) = h.hangup_and_wait().await;

    // Synthesized once, then served from the cache.
    let spoken = h.spoken();
    assert_eq!(
        spoken.iter().filter(|s| s.as_str() == "Got it.").count(),
        1,
        "{spoken:?}"
    );
    assert!(
        spoken.iter().any(|s| s.contains("Do you run ads")),
        "{spoken:?}"
    );
    assert!(lines.has("Got it."));
    assert_eq!(outcome.tts_cached_chars, "Got it.".len() as u64);
    // The learned clause is still part of what the caller heard.
    assert!(outcome
        .turns
        .iter()
        .any(|t| t.role == Speaker::Assistant && t.text.starts_with("Got it. Do you run ads")));
}

#[tokio::test]
async fn silence_reprompts_once_then_says_goodbye_and_hangs_up() {
    let mut s = settings();
    s.silence_reprompt = Duration::from_millis(400);
    let h = start(params(), s, FakeStt::default(), FakeLlm::default());
    h.begin().await;
    let ended = h
        .silence_until(Duration::from_secs(8), Harness::closed)
        .await;
    assert!(ended);
    let lines = assets().lines("en-IN").clone();
    let spoken = h.spoken();
    assert!(spoken.contains(&lines.reprompt), "{spoken:?}");
    assert!(spoken.contains(&lines.silence_goodbye), "{spoken:?}");
    let (outcome, h) = h.finish().await;
    assert_eq!(outcome.end_reason, "silence");
    assert_eq!(h.control.hangups.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn llm_timeout_falls_back_then_one_moment_then_primary() {
    let stt = FakeStt::with_script(&["hello", "are you there"]);
    let llm = FakeLlm::with_script(vec![
        FakeReply::Hang,                          // primary: no first token
        FakeReply::Text("Fallback here.".into()), // fallback answers
        FakeReply::Status(503),                   // turn 2: primary 503
        FakeReply::Status(500),                   // fallback 500
        FakeReply::Text("Back again.".into()),    // primary retry after "one moment"
    ]);
    let h = start(params(), settings(), stt, llm);
    h.begin().await;
    h.audio(false, 2500).await;
    h.audio(true, 500).await;
    h.silence_until(Duration::from_secs(3), |h| {
        h.spoken().iter().any(|s| s.contains("Fallback here"))
    })
    .await;
    h.audio(false, 600).await;
    h.audio(true, 500).await;
    let ok = h
        .silence_until(Duration::from_secs(3), |h| {
            h.spoken().iter().any(|s| s.contains("Back again"))
        })
        .await;
    assert!(ok, "{:?}", h.spoken());
    let one_moment = assets().lines("en-IN").one_moment.clone();
    assert!(h.spoken().contains(&one_moment));
    h.audio(false, 500).await;
    let (outcome, h) = h.hangup_and_wait().await;
    assert_eq!(
        h.llm.models(),
        vec![
            "sarvam-105b-conversations",
            "gemma4",
            "sarvam-105b-conversations",
            "gemma4",
            "sarvam-105b-conversations"
        ]
    );
    assert_eq!(outcome.latencies[0].llm_model.as_deref(), Some("gemma4"));
    assert_eq!(outcome.latencies[0].llm_attempts, 2);
    assert_eq!(outcome.latencies[1].llm_attempts, 3);
}

#[tokio::test]
async fn tts_budget_ends_the_call_politely() {
    let mut s = settings();
    s.max_tts_chars_per_call = 90; // greeting (~56) + a little
    let stt = FakeStt::with_script(&["tell me more"]);
    let llm = FakeLlm::with_script(vec![FakeReply::Text(
        "Short first part. This second sentence is much too long to fit in the remaining budget."
            .into(),
    )]);
    let h = start(params(), s, stt, llm);
    h.begin().await;
    h.audio(false, 2500).await;
    h.audio(true, 500).await;
    assert!(
        h.silence_until(Duration::from_secs(6), Harness::closed)
            .await
    );
    let budget_line = assets().lines("en-IN").budget_closing.clone();
    assert!(h.spoken().contains(&budget_line), "{:?}", h.spoken());
    assert!(!h.spoken().iter().any(|s| s.contains("second sentence")));
    let (outcome, _) = h.finish().await;
    assert_eq!(outcome.end_reason, "tts_budget");
}

#[tokio::test]
async fn send_details_tool_records_follow_up() {
    let stt = FakeStt::with_script(&["send me the price list"]);
    let llm = FakeLlm::with_script(vec![
        FakeReply::Tool {
            name: "send_details".into(),
            arguments: r#"{"topic":"price list"}"#.into(),
        },
        FakeReply::Text("Done, I'll send it on WhatsApp.".into()),
    ]);
    let h = start(params(), settings(), stt, llm);
    h.begin().await;
    h.audio(false, 2500).await;
    h.audio(true, 500).await;
    assert!(
        h.silence_until(Duration::from_secs(3), |h| {
            h.spoken().iter().any(|s| s.contains("WhatsApp"))
        })
        .await
    );
    h.audio(false, 600).await;
    let (outcome, h) = h.hangup_and_wait().await;
    assert_eq!(*h.tools.followups.lock().unwrap(), vec!["price list"]);
    assert!(outcome.turns.iter().any(|t| t.role == Speaker::Tool));
    // send_details is instant: no "let me check" filler.
    assert!(!h.spoken().iter().any(|s| s.contains("let me check")));
}

#[tokio::test]
async fn each_caller_turn_keeps_its_own_audio() {
    let stt = FakeStt::with_script(&["I run a clinic", "Tell me about ads please"]);
    let llm = FakeLlm::with_script(vec![
        FakeReply::Text("Nice.".into()),
        FakeReply::Text("Sure.".into()),
    ]);
    let h = start(params(), settings(), stt, llm);
    h.begin().await;
    h.audio(false, 2500).await;
    h.audio(true, 500).await;
    assert!(
        h.silence_until(Duration::from_secs(3), |h| {
            h.spoken().iter().any(|s| s.contains("Nice"))
        })
        .await
    );
    h.audio(false, 1000).await;
    h.audio(true, 1000).await;
    assert!(
        h.silence_until(Duration::from_secs(3), |h| {
            h.spoken().iter().any(|s| s.contains("Sure"))
        })
        .await
    );
    h.audio(false, 600).await;
    let (outcome, _) = h.hangup_and_wait().await;
    let audio: Vec<(Speaker, usize)> = outcome
        .turns
        .iter()
        .map(|t| (t.role, t.audio.as_ref().map_or(0, |a| a.len())))
        .collect();
    let users: Vec<usize> = audio
        .iter()
        .filter(|(r, _)| *r == Speaker::User)
        .map(|(_, n)| *n)
        .collect();
    assert_eq!(users.len(), 2, "{audio:?}");
    // 500 ms then 1000 ms of speech (8 bytes per ms), plus pre-roll and the
    // pause that ended each turn, but never the other turn's speech.
    assert!((500 * 8..1500 * 8).contains(&users[0]), "{audio:?}");
    assert!((1000 * 8..2000 * 8).contains(&users[1]), "{audio:?}");
    assert!(
        audio.iter().all(|(r, n)| *r == Speaker::User || *n == 0),
        "{audio:?}"
    );
}

#[tokio::test]
async fn switch_language_tool_changes_the_call_language() {
    let stt = FakeStt::with_script(&["can you talk in tamil"]);
    let llm = FakeLlm::with_script(vec![
        FakeReply::Tool {
            name: "switch_language".into(),
            arguments: r#"{"language":"ta-IN"}"#.into(),
        },
        FakeReply::Text("சரி, எந்த நாள் வசதியாக இருக்கும்?".into()),
    ]);
    let h = start(params(), settings(), stt, llm);
    h.begin().await;
    h.audio(false, 2500).await;
    h.audio(true, 500).await;
    assert!(
        h.silence_until(Duration::from_secs(3), |h| {
            h.spoken().iter().any(|s| s.contains("எந்த நாள்"))
        })
        .await
    );
    h.audio(false, 600).await;
    let (outcome, _) = h.hangup_and_wait().await;
    assert_eq!(outcome.detected_language.as_deref(), Some("ta-IN"));
}

#[tokio::test]
async fn hindi_in_english_letters_switches_to_hindi_before_the_reply() {
    let stt = FakeStt::with_script(&["haan ji kal subah free hai kya"]);
    let llm = FakeLlm::with_script(vec![FakeReply::Text(
        "[happy] हाँ जी, कल सुबह समय खाली है।".into(),
    )]);
    let h = start(params(), settings(), stt, llm.clone());
    h.begin().await;
    h.audio(false, 2500).await;
    h.audio(true, 500).await;
    assert!(
        h.silence_until(Duration::from_secs(3), |h| {
            h.spoken().iter().any(|s| s.contains("खाली"))
        })
        .await
    );
    let spoken = h.spoken();
    let speakers = h.tts.speakers.lock().unwrap().clone();
    let hindi = spoken.iter().position(|s| s.contains("खाली")).unwrap();
    assert!(
        speakers[hindi].starts_with("aparna_hi"),
        "{}",
        speakers[hindi]
    );
    h.audio(false, 600).await;
    let (outcome, _) = h.hangup_and_wait().await;
    assert_eq!(outcome.detected_language.as_deref(), Some("hi-IN"));
}

#[tokio::test]
async fn a_male_voice_says_its_name_once_after_the_switch() {
    // Tamil's voice is Gokul (voices.toml `name` and `intro`).
    let stt = FakeStt::with_script(&["can you talk in tamil", "seri"]);
    let llm = FakeLlm::with_script(vec![
        FakeReply::Text("[neutral] சரி, எந்த நாள் வசதி?".into()),
        FakeReply::Text("[happy] சரி.".into()),
    ]);
    let h = start(params(), settings(), stt, llm.clone());
    h.begin().await;
    for _ in 0..2 {
        h.audio(false, 2500).await;
        h.audio(true, 500).await;
    }
    assert!(
        h.silence_until(Duration::from_secs(5), |h| {
            h.spoken().iter().filter(|s| s.contains("சரி")).count() >= 2
        })
        .await,
        "{:?}",
        h.spoken()
    );
    let spoken = h.spoken();
    h.audio(false, 600).await;
    h.hangup_and_wait().await;
    let intro = spoken
        .iter()
        .position(|s| s.contains("கோகுல்"))
        .expect("intro");
    let reply = spoken.iter().position(|s| s.contains("எந்த நாள்")).unwrap();
    assert!(intro < reply, "the name comes first: {spoken:?}");
    assert_eq!(spoken.iter().filter(|s| s.contains("கோகுல்")).count(), 1);
    // The model knows its new name from the next request on.
    let systems = llm.systems.lock().unwrap().clone();
    assert!(!systems[0].contains("YOUR NAME"));
    assert!(systems[1].contains("you are Gokul"), "{}", systems[1]);
}

#[tokio::test]
async fn a_reply_in_another_script_switches_the_voice() {
    // The model answers in Telugu without calling switch_language.
    let stt = FakeStt::with_script(&["can you talk in telugu"]);
    let llm = FakeLlm::with_script(vec![FakeReply::Text(
        "[neutral] సరే, మీకు ఏ రోజు వీలవుతుంది?".into(),
    )]);
    let h = start(params(), settings(), stt, llm);
    h.begin().await;
    h.audio(false, 2500).await;
    h.audio(true, 500).await;
    assert!(
        h.silence_until(Duration::from_secs(3), |h| {
            h.spoken().iter().any(|s| s.contains("ఏ రోజు"))
        })
        .await
    );
    let spoken = h.spoken();
    let speakers = h.tts.speakers.lock().unwrap().clone();
    let telugu = spoken.iter().position(|s| s.contains("ఏ రోజు")).unwrap();
    assert_eq!(speakers[telugu], "pooja_te_conversation");
    h.audio(false, 600).await;
    let (outcome, _) = h.hangup_and_wait().await;
    assert_eq!(outcome.detected_language.as_deref(), Some("te-IN"));
}

#[tokio::test]
async fn tone_tags_change_the_voice_and_are_not_spoken() {
    let stt = FakeStt::with_script(&["my name is asha", "what is bitcoin price"]);
    let llm = FakeLlm::with_script(vec![
        FakeReply::Text("[happy] Lovely, thanks Asha.".into()),
        FakeReply::Text("[serious] Sorry, I can't help with that one.".into()),
    ]);
    let h = start(params(), settings(), stt, llm);
    h.begin().await;
    for _ in 0..2 {
        h.audio(false, 2500).await;
        h.audio(true, 500).await;
    }
    assert!(
        h.silence_until(Duration::from_secs(4), |h| {
            h.spoken().iter().any(|s| s.contains("can't help"))
        })
        .await,
        "spoken: {:?}",
        h.spoken()
    );
    let spoken = h.spoken();
    assert!(
        spoken
            .iter()
            .all(|s| !s.contains('[') && !s.contains("happy")),
        "{spoken:?}"
    );
    // The English voice has no tone styles in voices.toml: tones set the pace.
    let paces = h.tts.paces.lock().unwrap().clone();
    let pace_of = |needle: &str| {
        spoken
            .iter()
            .zip(&paces)
            .find(|(s, _)| s.contains(needle))
            .map(|(_, p)| *p)
    };
    // [happy] plays as neutral (the upbeat voice sounded over the top).
    assert!((pace_of("Lovely").unwrap() - 1.0).abs() < 1e-6, "{paces:?}");
    assert!(pace_of("can't help").unwrap() < 1.0, "{paces:?}");
    h.audio(false, 600).await;
    h.hangup_and_wait().await;
}

#[tokio::test]
async fn max_duration_says_closing_line_and_hangs_up() {
    let mut p = params();
    p.max_duration = Duration::from_millis(1500);
    let h = start(p, settings(), FakeStt::default(), FakeLlm::default());
    h.begin().await;
    assert!(
        h.silence_until(Duration::from_secs(8), Harness::closed)
            .await
    );
    let closing = assets().lines("en-IN").max_duration_closing.clone();
    assert!(h.spoken().contains(&closing));
    let (outcome, _) = h.finish().await;
    assert_eq!(outcome.end_reason, "max_duration");
}

#[tokio::test]
async fn max_duration_closing_is_aligned_to_the_billing_block() {
    // 2 s blocks, 0.5 s wrap-up, 5 s max: the closing starts 0.5 s before
    // the 4 s boundary (minus 1 s answer-to-stream), not at 5 s.
    let mut p = params();
    p.max_duration = Duration::from_secs(5);
    let mut s = settings();
    s.billing_pulse = Duration::from_secs(2);
    s.pulse_wrap_up = Duration::from_millis(500);
    let closing = assets().lines("en-IN").max_duration_closing.clone();
    let h = start(p, s, FakeStt::default(), FakeLlm::default());
    let started = std::time::Instant::now();
    h.begin().await;
    assert!(
        h.silence_until(Duration::from_secs(6), |h| h.spoken().contains(&closing))
            .await
    );
    let at = started.elapsed();
    assert!(
        at >= Duration::from_secs(2) && at < Duration::from_millis(3500),
        "closing started at {at:?}"
    );
    assert!(
        h.silence_until(Duration::from_secs(8), Harness::closed)
            .await
    );
    let (outcome, _) = h.finish().await;
    assert_eq!(outcome.end_reason, "max_duration");
}

#[tokio::test]
async fn eager_mode_still_answers_with_the_final_transcript() {
    let mut s = settings();
    s.eager_llm = true;
    let stt = FakeStt::with_script(&["what does it cost"]);
    let llm = FakeLlm::with_script(vec![
        FakeReply::Text("Speculative answer.".into()),
        FakeReply::Text("It starts at ten thousand a month.".into()),
    ]);
    let h = start(params(), s, stt, llm);
    h.begin().await;
    h.audio(false, 2500).await;
    h.audio(true, 500).await;
    assert!(
        h.silence_until(Duration::from_secs(4), |h| {
            h.spoken()
                .iter()
                .any(|s| s.contains("ten thousand") || s.contains("Speculative"))
        })
        .await
    );
    h.audio(false, 800).await;
    let (outcome, h) = h.hangup_and_wait().await;
    // The speculative turn started on a partial that differed from the final, so it
    // was discarded and never spoken.
    assert!(
        !h.spoken().iter().any(|s| s.contains("Speculative")),
        "{:?}",
        h.spoken()
    );
    assert!(outcome.turns.iter().any(|t| t.text == "what does it cost"));
}

const DISCLOSURE: &str = "This is an AI assistant, and this call is recorded.";
const BODY: &str = "This is Priya from Star Smile Dental. Shall we book your check-up?";

/// A greeting in two cached clips: the disclosure, then the body.
fn split_greeting(disclosure_ms: usize, body_ms: usize) -> CallParams {
    let silence = |ms: usize| Some(Bytes::from(vec![audio::mulaw::SILENCE; ms * 8]));
    CallParams {
        greeting_text: format!("{DISCLOSURE} {BODY}"),
        disclosure_text: DISCLOSURE.into(),
        disclosure_audio: silence(disclosure_ms),
        greeting_audio: silence(body_ms),
        ..params()
    }
}

#[tokio::test]
async fn caller_can_interrupt_the_greeting_body() {
    let stt = FakeStt::with_script(&["What are your timings"]);
    let llm = FakeLlm::with_script(vec![FakeReply::Text("We are open ten to six.".into())]);
    let h = start(split_greeting(1000, 3000), settings(), stt, llm);
    h.begin().await;
    h.audio(false, 1500).await; // disclosure done, body playing
    assert!(!h.cleared());
    h.audio(true, 600).await; // caller talks over the body
    assert!(h.cleared(), "greeting body was not stopped");
    assert!(
        h.silence_until(Duration::from_secs(3), |h| {
            h.spoken().iter().any(|s| s.contains("ten to six"))
        })
        .await,
        "{:?}",
        h.spoken()
    );
    h.audio(false, 600).await;
    let (outcome, _) = h.hangup_and_wait().await;

    let greeting = &outcome.turns[0];
    assert!(greeting.interrupted, "{greeting:?}");
    assert!(greeting.text.starts_with(DISCLOSURE), "{greeting:?}");
    assert!(!greeting.text.contains("check-up?"), "{greeting:?}");
    assert!(outcome
        .turns
        .iter()
        .any(|t| t.role == Speaker::User && t.text == "What are your timings"));
    assert!(outcome.greeting_latency_ms.is_some());
}

#[tokio::test]
async fn speech_during_the_disclosure_waits_for_it() {
    let stt = FakeStt::with_script(&["Who is this"]);
    let llm = FakeLlm::with_script(vec![FakeReply::Text("I'm calling from the clinic.".into())]);
    let h = start(split_greeting(3000, 2000), settings(), stt, llm);
    h.begin().await;
    h.audio(false, 300).await;
    h.audio(true, 600).await; // caller talks over the disclosure
    h.audio(false, 1400).await; // ~2.3 s: disclosure still playing
    assert!(!h.cleared(), "the disclosure was interrupted");
    assert!(h.spoken().is_empty(), "answered over the disclosure");
    // Answered once the disclosure ends, instead of playing the body.
    assert!(
        h.silence_until(Duration::from_secs(3), |h| {
            h.spoken().iter().any(|s| s.contains("from the clinic"))
        })
        .await
    );
    assert!(h.cleared());
    h.audio(false, 600).await;
    let (outcome, _) = h.hangup_and_wait().await;

    let greeting = &outcome.turns[0];
    assert_eq!(greeting.text, DISCLOSURE, "{:#?}", outcome.turns);
    assert!(greeting.interrupted);
    assert!(outcome
        .turns
        .iter()
        .any(|t| t.role == Speaker::User && t.text == "Who is this"));
}

#[tokio::test]
async fn slow_tools_play_the_checking_line_once_before_the_reply() {
    let stt = FakeStt::with_script(&["Is Friday free"]);
    let check = || FakeReply::Tool {
        name: "check_availability".into(),
        arguments: r#"{"date":"2026-10-30"}"#.into(),
    };
    let llm = FakeLlm::with_script(vec![
        check(),
        check(),
        FakeReply::Text("Friday has free times from ten.".into()),
    ]);
    let h = start(params(), settings(), stt, llm);
    h.begin().await;
    h.audio(false, 2500).await;
    h.audio(true, 500).await;
    assert!(
        h.silence_until(Duration::from_secs(3), |h| {
            h.spoken().iter().any(|s| s.contains("free times"))
        })
        .await
    );
    let spoken = h.spoken();
    let checking = |s: &&String| s.as_str() == "One moment, let me check.";
    assert_eq!(spoken.iter().filter(checking).count(), 1, "{spoken:?}");
    let at = |p: &dyn Fn(&String) -> bool| spoken.iter().position(p).unwrap();
    assert!(
        at(&|s| s.contains("let me check")) < at(&|s| s.contains("free times")),
        "{spoken:?}"
    );
    h.hangup_and_wait().await;
}

#[tokio::test]
async fn talking_over_the_goodbye_does_not_restart_the_call() {
    let stt = FakeStt::with_script(&["That's all, nothing else", "Okay okay"]);
    let llm = FakeLlm::with_script(vec![
        FakeReply::Tool {
            name: "end_call".into(),
            arguments: r#"{"reason":"done"}"#.into(),
        },
        FakeReply::Text("This must never be said.".into()),
    ]);
    // A goodbye that takes about two seconds to play.
    let tts = FakeTts {
        chunk_delay: Duration::from_millis(2),
        audio_per_char: Duration::from_millis(60),
        ..FakeTts::default()
    };
    let h = start_with_tts(params(), settings(), stt, llm, tts, Arc::new(NoLines));
    h.begin().await;
    h.audio(false, 2500).await;
    h.audio(true, 500).await;
    let goodbye = assets().lines("en-IN").goodbye.clone();
    assert!(
        h.silence_until(Duration::from_secs(3), |h| h.spoken().contains(&goodbye))
            .await
    );
    h.audio(true, 500).await; // "okay okay" while the goodbye plays
    assert!(
        h.silence_until(Duration::from_secs(6), Harness::closed)
            .await
    );
    let (outcome, h) = h.finish().await;
    assert_eq!(outcome.end_reason, "agent_end_call");
    assert_eq!(h.llm.requests.lock().unwrap().len(), 1);
    assert!(
        outcome.turns.iter().all(|t| !t.text.contains("never")),
        "{:#?}",
        outcome.turns
    );
}

fn ack_settings(wait_ms: u64) -> PipelineSettings {
    let mut s = settings();
    s.ack_wait = Duration::from_millis(wait_ms);
    s
}

#[tokio::test]
async fn a_lone_yes_waits_for_the_caller_to_go_on() {
    let stt = FakeStt::with_script(&["Yes.", "Arjun Kumar"]);
    let llm = FakeLlm::with_script(vec![FakeReply::Text("Thanks, Arjun.".into())]);
    let h = start(params(), ack_settings(1500), stt, llm);
    h.begin().await;
    h.audio(false, 2500).await;
    h.audio(true, 300).await; // "Yes."
    h.audio(false, 800).await; // thinking...
    assert!(
        h.llm.requests.lock().unwrap().is_empty(),
        "answered the lone yes"
    );
    h.audio(true, 500).await; // "Arjun Kumar"
    assert!(
        h.silence_until(Duration::from_secs(3), |h| {
            h.spoken().iter().any(|s| s.contains("Arjun"))
        })
        .await
    );
    let (outcome, h) = h.hangup_and_wait().await;
    let requests: Vec<String> = h
        .llm
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|(_, u)| u.clone())
        .collect();
    assert_eq!(requests, ["Yes. Arjun Kumar"], "{:#?}", outcome.turns);
}

#[tokio::test]
async fn a_lone_yes_is_answered_after_the_wait() {
    let stt = FakeStt::with_script(&["Yes"]);
    let llm = FakeLlm::with_script(vec![FakeReply::Text("Great, it's booked.".into())]);
    let h = start(params(), ack_settings(700), stt, llm);
    h.begin().await;
    h.audio(false, 2500).await;
    h.audio(true, 300).await;
    h.audio(false, 500).await;
    assert!(h.llm.requests.lock().unwrap().is_empty());
    assert!(
        h.silence_until(Duration::from_secs(3), |h| {
            h.spoken().iter().any(|s| s.contains("booked"))
        })
        .await
    );
}

#[tokio::test]
async fn the_checking_filler_changes_between_tool_calls() {
    let stt = FakeStt::with_script(&["Is Friday free", "And Saturday"]);
    let check = || FakeReply::Tool {
        name: "check_availability".into(),
        arguments: r#"{"date":"2026-10-30"}"#.into(),
    };
    let llm = FakeLlm::with_script(vec![
        check(),
        FakeReply::Text("Friday is free.".into()),
        check(),
        FakeReply::Text("Saturday is free too.".into()),
    ]);
    let h = start(params(), settings(), stt, llm);
    h.begin().await;
    h.audio(false, 2500).await;
    h.audio(true, 500).await;
    assert!(
        h.silence_until(Duration::from_secs(3), |h| {
            h.spoken().iter().any(|s| s.contains("Friday is free"))
        })
        .await
    );
    h.audio(false, 1500).await;
    h.audio(true, 500).await;
    assert!(
        h.silence_until(Duration::from_secs(3), |h| {
            h.spoken().iter().any(|s| s.contains("Saturday"))
        })
        .await
    );
    let fillers = assets().lines("en-IN").checking.clone();
    let spoken = h.spoken();
    let used: Vec<&String> = spoken.iter().filter(|s| fillers.contains(s)).collect();
    assert_eq!(used, [&fillers[0], &fillers[1]], "{spoken:?}");
}

#[tokio::test]
async fn speech_is_resent_when_tts_fails_or_stalls() {
    for (fail, stall) in [(2, 0), (0, 1)] {
        let stt = FakeStt::with_script(&["Which day is free"]);
        let llm = FakeLlm::with_script(vec![FakeReply::Text("Friday is free.".into())]);
        let tts = FakeTts {
            chunk_delay: Duration::from_millis(10),
            ..FakeTts::default()
        };
        let h = start_with_tts(
            params(),
            settings(),
            stt,
            llm,
            tts.clone(),
            Arc::new(NoLines),
        );
        h.begin().await;
        h.audio(false, 2500).await; // greeting plays out
                                    // The reply's first attempts fail (socket closed) or hang.
        tts.fail_speaks.store(fail, Ordering::SeqCst);
        tts.stall_speaks.store(stall, Ordering::SeqCst);
        h.audio(true, 500).await;
        assert!(
            h.silence_until(Duration::from_secs(8), |h| {
                h.spoken().iter().any(|s| s.contains("Friday is free"))
            })
            .await,
            "reply never spoken (fail {fail}, stall {stall}): {:?}",
            h.spoken()
        );
        let (outcome, _) = h.hangup_and_wait().await;
        assert!(
            outcome
                .turns
                .iter()
                .any(|t| t.role == Speaker::Assistant && t.text.contains("Friday is free")),
            "{:#?}",
            outcome.turns
        );
    }
}

#[tokio::test]
async fn a_late_reply_gets_a_cached_filler_first_and_a_fast_one_does_not() {
    for slow in [true, false] {
        let stt = FakeStt::with_script(&["Tell me about the plots"]);
        let mut llm = FakeLlm::with_script(vec![FakeReply::Text("There are forty plots.".into())]);
        if slow {
            llm.first_token_delay = Duration::from_millis(1500);
        }
        let mut s = settings();
        // Speech end is detected 400 ms after the caller stops (vad_silence).
        s.late_reply = Duration::from_millis(900);
        s.llm_first_token_timeout = Duration::from_secs(3);
        let filler = assets().lines("en-IN").checking[0].clone();
        let lines = MemoryLines::default();
        // 200 ms of a byte the fake TTS never produces.
        lines
            .0
            .lock()
            .unwrap()
            .insert(filler.clone(), Bytes::from(vec![0x42u8; 1600]));
        let tts = FakeTts {
            chunk_delay: Duration::from_millis(10),
            ..FakeTts::default()
        };
        let h = start_with_tts(params(), s, stt, llm, tts, Arc::new(lines));
        h.begin().await;
        h.audio(false, 2500).await;
        h.audio(true, 500).await;
        assert!(
            h.silence_until(Duration::from_secs(4), |h| {
                h.spoken().iter().any(|s| s.contains("forty plots"))
            })
            .await
        );
        h.audio(false, 600).await;
        let filler_played = h
            .out
            .lock()
            .unwrap()
            .iter()
            .any(|m| matches!(m, MediaOut::Play(f) if f.iter().all(|&b| b == 0x42)));
        assert_eq!(filler_played, slow, "slow={slow}");
        let (outcome, _) = h.hangup_and_wait().await;
        let reply = outcome
            .turns
            .iter()
            .find(|t| t.role == Speaker::Assistant && t.text.contains("forty plots"))
            .map(|t| t.text.clone())
            .unwrap_or_default();
        assert_eq!(reply.starts_with(&filler), slow, "{reply}");
        // The filler does not count as the reply's first audio.
        let v2v = outcome.latencies.last().and_then(|l| l.first_audio_sent_ms);
        assert_eq!(v2v.is_some_and(|ms| ms >= 1500), slow, "{v2v:?}");
    }
}

#[tokio::test]
async fn a_short_remark_over_the_agent_is_answered_after_it() {
    let stt = FakeStt::with_script(&["Tell me about the plots", "how much is it"]);
    let long = "Star Garden has forty six plots on ten acres of coffee. ".repeat(3);
    let llm = FakeLlm::with_script(vec![
        FakeReply::Text(long),
        FakeReply::Text("Prices are shared on WhatsApp.".into()),
    ]);
    let mut s = settings();
    s.barge_in_talk = Duration::from_millis(2000);
    let h = start(params(), s, stt, llm);
    h.begin().await;
    h.audio(false, 2500).await;
    h.audio(true, 500).await;
    assert!(
        h.silence_until(Duration::from_secs(3), |h| h.spoken().len() >= 2)
            .await
    );
    h.audio(false, 200).await; // the agent is speaking
    h.audio(true, 500).await; // a short remark, well under 2 s
    assert!(!h.cleared(), "a short remark cut the agent off");
    assert!(
        h.silence_until(Duration::from_secs(8), |h| {
            h.spoken().iter().any(|s| s.contains("WhatsApp"))
        })
        .await,
        "the remark was never answered: {:?}",
        h.spoken()
    );
    let (outcome, h) = h.hangup_and_wait().await;
    let requests: Vec<String> = h
        .llm
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|(_, u)| u.clone())
        .collect();
    assert_eq!(
        requests.last().map(String::as_str),
        Some("how much is it"),
        "{requests:?}"
    );
    // The long reply played to the end (the last one is cut by the hangup).
    assert!(
        outcome
            .turns
            .iter()
            .filter(|t| t.text.contains("forty six"))
            .all(|t| !t.interrupted),
        "{:#?}",
        outcome.turns
    );
}

#[tokio::test]
async fn a_lone_fragment_is_ignored() {
    let stt = FakeStt::with_script(&["I"]);
    let llm = FakeLlm::with_script(vec![FakeReply::Text(
        "Sorry, could you say that again?".into(),
    )]);
    let h = start(params(), ack_settings(600), stt, llm);
    h.begin().await;
    h.audio(false, 2500).await;
    h.audio(true, 300).await; // "I"
    h.audio(false, 1500).await; // and nothing more
    assert!(
        h.llm.requests.lock().unwrap().is_empty(),
        "answered a lone \"I\""
    );
    let (outcome, _) = h.hangup_and_wait().await;
    assert!(
        outcome.turns.iter().all(|t| t.role != Speaker::User),
        "{:#?}",
        outcome.turns
    );
}

#[tokio::test]
async fn after_one_switch_the_language_holds_until_three_turns_in_another() {
    let stt = FakeStt::with_script(&[
        "[[ta-IN]]எனக்கு நிலம் வேண்டும்",
        "[[en-IN]]okay tell me more",
        "[[en-IN]]what about the price",
        "[[en-IN]]please speak in english",
    ]);
    let llm = FakeLlm::with_script(vec![
        FakeReply::Text("சரி, சொல்லுங்கள்.".into()),
        FakeReply::Text("ஸ்டார் கார்டனில் நாற்பது மனைகள் உள்ளன.".into()),
        FakeReply::Text("விலை வாட்ஸ்அப்பில் அனுப்புகிறேன்.".into()),
        FakeReply::Text("Sure, I will continue in English.".into()),
    ]);
    let mut p = params();
    p.fixed_language = None;
    let h = start(p, settings(), stt, llm);
    h.begin().await;
    h.audio(false, 2500).await;
    for n in 1..=4 {
        h.audio(true, 500).await;
        assert!(
            h.silence_until(Duration::from_secs(4), |h| h
                .llm
                .requests
                .lock()
                .unwrap()
                .len()
                >= n)
                .await
        );
        h.audio(false, 2500).await;
    }
    let (outcome, h) = h.hangup_and_wait().await;
    let requests: Vec<String> = h
        .llm
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|(_, u)| u.clone())
        .collect();
    // Tamil from the first turn; two English turns are not enough to leave it.
    for r in &requests[..3] {
        assert!(r.contains("(Reply in Tamil"), "{requests:#?}");
    }
    // The third English turn in a row switches back.
    assert!(!requests[3].contains("(Reply in"), "{requests:#?}");
    assert_eq!(outcome.detected_language.as_deref(), Some("en-IN"));
}
