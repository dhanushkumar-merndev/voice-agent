//! Deterministic fake providers for tests and offline runs
//! (`PROVIDER_MODE=fake`). They follow the same contracts as the real
//! providers, with configurable delays.

use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use async_trait::async_trait;
use bytes::Bytes;
use common::assets::{TokenUsage, Voice};
use tokio::sync::mpsc;

use crate::{
    llm::{
        ChatRequest, Completion, FunctionCall, LlmEndpoint, LlmEvent, LlmProvider, LlmStream,
        ToolCall,
    },
    stt::{SttCommand, SttEvent, SttOptions, SttProvider, SttSession},
    tts::{TtsCommand, TtsEvent, TtsProvider, TtsSession},
    ProviderError,
};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

// ---------------------------------------------------------------- STT ----

/// Fake STT: returns the next scripted transcript for each utterance.
#[derive(Debug, Clone, Default)]
pub struct FakeStt {
    /// Transcripts returned in order; when empty, `"fake utterance N"`. A
    /// line starting `[[ta-IN]]` reports that language for this final.
    pub script: Arc<Mutex<VecDeque<String>>>,
    /// Language reported with each final (auto-detect simulation).
    pub language: Option<String>,
    /// Delay between `SpeechEnd` and the final transcript.
    pub final_delay: Duration,
    /// Audio bytes received across all sessions (billing check).
    pub audio_bytes: Arc<AtomicUsize>,
}

impl FakeStt {
    /// A fake with scripted transcripts.
    pub fn with_script(lines: &[&str]) -> Self {
        Self {
            script: Arc::new(Mutex::new(lines.iter().map(|s| (*s).to_owned()).collect())),
            final_delay: Duration::from_millis(30),
            ..Self::default()
        }
    }
}

#[async_trait]
impl SttProvider for FakeStt {
    fn name(&self) -> &'static str {
        "fake"
    }

    async fn open(&self, _options: SttOptions) -> Result<SttSession, ProviderError> {
        let (cmd_tx, mut cmd_rx) = mpsc::channel(256);
        let (evt_tx, evt_rx) = mpsc::channel(64);
        let this = self.clone();
        tokio::spawn(async move {
            let mut utterance = 0u32;
            let mut bytes_this_utterance = 0usize;
            let mut billed = 0usize;
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    SttCommand::SpeechStart => bytes_this_utterance = 0,
                    SttCommand::Audio(a) => {
                        bytes_this_utterance += a.len();
                        billed += a.len();
                        this.audio_bytes.fetch_add(a.len(), Ordering::Relaxed);
                        if bytes_this_utterance == a.len() {
                            let _ = evt_tx
                                .send(SttEvent::Partial {
                                    utterance,
                                    text: "…".into(),
                                    language: this.language.clone(),
                                })
                                .await;
                        }
                    }
                    SttCommand::SpeechEnd => {
                        let text = lock(&this.script)
                            .pop_front()
                            .unwrap_or_else(|| format!("fake utterance {utterance}"));
                        let evt_tx = evt_tx.clone();
                        let (language, text) = match text
                            .strip_prefix("[[")
                            .and_then(|rest| rest.split_once("]]"))
                        {
                            Some((lang, rest)) => (Some(lang.to_owned()), rest.to_owned()),
                            None => (this.language.clone(), text),
                        };
                        let delay = this.final_delay;
                        let u = utterance;
                        tokio::spawn(async move {
                            tokio::time::sleep(delay).await;
                            let _ = evt_tx
                                .send(SttEvent::Final {
                                    utterance: u,
                                    text,
                                    language_confidence: language.as_ref().map(|_| 0.95),
                                    language,
                                })
                                .await;
                        });
                        utterance += 1;
                    }
                    SttCommand::End => {
                        let _ = evt_tx
                            .send(SttEvent::SessionEnd {
                                billed_secs: billed as f64 / 8000.0,
                            })
                            .await;
                        break;
                    }
                }
            }
        });
        Ok(SttSession {
            commands: cmd_tx,
            events: evt_rx,
        })
    }
}

// ---------------------------------------------------------------- TTS ----

/// Fake TTS: 5 ms of silence per character (or `audio_per_char`),
/// delivered in 20 ms chunks.
#[derive(Debug, Clone, Default)]
pub struct FakeTts {
    /// Every text spoken, in order.
    pub spoken: Arc<Mutex<Vec<String>>>,
    /// Speaker of each spoken text (from `open` and later `Configure`).
    pub speakers: Arc<Mutex<Vec<String>>>,
    /// Pace of each spoken text.
    pub paces: Arc<Mutex<Vec<f32>>>,
    /// Delay before the first audio chunk of each clause.
    pub first_byte_delay: Duration,
    /// Number of sessions opened.
    pub opened: Arc<AtomicUsize>,
    /// Delay between 20 ms audio chunks.
    pub chunk_delay: Duration,
    /// Audio length per character; zero means 5 ms. Real speech is ~60 ms.
    pub audio_per_char: Duration,
    /// The next this-many `Speak`s close the session without audio, like
    /// a rate-limited Sarvam socket.
    pub fail_speaks: Arc<AtomicUsize>,
    /// The next this-many `Speak`s never answer (a stuck socket).
    pub stall_speaks: Arc<AtomicUsize>,
}

impl FakeTts {
    /// Texts spoken so far.
    pub fn spoken(&self) -> Vec<String> {
        lock(&self.spoken).clone()
    }
}

#[async_trait]
impl TtsProvider for FakeTts {
    fn name(&self) -> &'static str {
        "fake"
    }

    async fn open(&self, voice: &Voice) -> Result<TtsSession, ProviderError> {
        self.opened.fetch_add(1, Ordering::Relaxed);
        let (cmd_tx, mut cmd_rx) = mpsc::channel(64);
        let (evt_tx, evt_rx) = mpsc::channel(256);
        let this = self.clone();
        let mut speaker = voice.speaker.clone();
        let mut pace = voice.pace;
        tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    TtsCommand::Configure(v) => (speaker, pace) = (v.speaker, v.pace),
                    TtsCommand::Close => break,
                    TtsCommand::Speak { id, text } => {
                        // Takes one from a counter that is not yet zero.
                        let take = |n: &AtomicUsize| {
                            let mut v = n.load(Ordering::SeqCst);
                            while v > 0 {
                                match n.compare_exchange(
                                    v,
                                    v - 1,
                                    Ordering::SeqCst,
                                    Ordering::SeqCst,
                                ) {
                                    Ok(_) => return true,
                                    Err(now) => v = now,
                                }
                            }
                            false
                        };
                        if take(&this.fail_speaks) {
                            return;
                        }
                        if take(&this.stall_speaks) {
                            continue;
                        }
                        lock(&this.spoken).push(text.clone());
                        lock(&this.speakers).push(speaker.clone());
                        lock(&this.paces).push(pace);
                        tokio::time::sleep(this.first_byte_delay).await;
                        let per_char = match this.audio_per_char.as_millis() {
                            0 => 40,
                            ms => ms as usize * 8,
                        };
                        let total = text.chars().count().max(1) * per_char;
                        let mut sent = 0;
                        while sent < total {
                            let n = (total - sent).min(160);
                            let data = Bytes::from(vec![audio::mulaw::SILENCE; n]);
                            if evt_tx.send(TtsEvent::Audio { id, data }).await.is_err() {
                                return;
                            }
                            sent += n;
                            if !this.chunk_delay.is_zero() {
                                tokio::time::sleep(this.chunk_delay).await;
                            }
                        }
                        if evt_tx.send(TtsEvent::Done { id }).await.is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Ok(TtsSession {
            model: voice.model.clone(),
            commands: cmd_tx,
            events: evt_rx,
        })
    }
}

// ---------------------------------------------------------------- LLM ----

/// One scripted LLM response.
#[derive(Debug, Clone)]
pub enum FakeReply {
    /// Stream this text, a few characters per token.
    Text(String),
    /// Request a tool call, then (on the next request) continue the script.
    Tool {
        /// Function name.
        name: String,
        /// JSON arguments.
        arguments: String,
    },
    /// Fail with this HTTP status.
    Status(u16),
    /// Accept the request but never send a token.
    Hang,
}

/// Fake LLM with a scripted reply queue (shared across models).
#[derive(Debug, Clone, Default)]
pub struct FakeLlm {
    /// Replies in order; when empty, a generic short answer.
    pub script: Arc<Mutex<VecDeque<FakeReply>>>,
    /// Every request received: (model, last user message).
    pub requests: Arc<Mutex<Vec<(String, String)>>>,
    /// The system message of every request.
    pub systems: Arc<Mutex<Vec<String>>>,
    /// Delay before the first token.
    pub first_token_delay: Duration,
}

impl FakeLlm {
    /// A fake with scripted replies.
    pub fn with_script(replies: Vec<FakeReply>) -> Self {
        Self {
            script: Arc::new(Mutex::new(replies.into())),
            ..Self::default()
        }
    }

    /// Models that received requests, in order.
    pub fn models(&self) -> Vec<String> {
        lock(&self.requests)
            .iter()
            .map(|(m, _)| m.clone())
            .collect()
    }
}

#[async_trait]
impl LlmProvider for FakeLlm {
    fn name(&self) -> &'static str {
        "fake"
    }

    async fn stream(
        &self,
        endpoint: &LlmEndpoint,
        request: &ChatRequest,
    ) -> Result<LlmStream, ProviderError> {
        let last_user = request
            .messages
            .iter()
            .rev()
            .find(|m| m.role == crate::llm::Role::User)
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        lock(&self.requests).push((endpoint.model.clone(), last_user));
        let system = request
            .messages
            .iter()
            .find(|m| m.role == crate::llm::Role::System)
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        lock(&self.systems).push(system);
        let reply = lock(&self.script)
            .pop_front()
            .unwrap_or_else(|| FakeReply::Text("Sure. How can I help you today?".into()));
        if let FakeReply::Status(status) = reply {
            return Err(ProviderError::Status {
                status,
                body: "fake".into(),
            });
        }
        let (tx, rx) = mpsc::channel(64);
        let delay = self.first_token_delay;
        tokio::spawn(async move {
            match reply {
                FakeReply::Hang => {
                    tx.closed().await;
                }
                FakeReply::Status(_) => {}
                FakeReply::Text(text) => {
                    tokio::time::sleep(delay).await;
                    let chars: Vec<char> = text.chars().collect();
                    for piece in chars.chunks(4) {
                        let s: String = piece.iter().collect();
                        if tx.send(Ok(LlmEvent::Token(s))).await.is_err() {
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }
                    let _ = tx
                        .send(Ok(LlmEvent::Usage(TokenUsage {
                            prompt_tokens: 100,
                            cached_prompt_tokens: 50,
                            completion_tokens: (chars.len() / 4) as u64,
                        })))
                        .await;
                    let _ = tx
                        .send(Ok(LlmEvent::Finish {
                            reason: Some("stop".into()),
                        }))
                        .await;
                }
                FakeReply::Tool { name, arguments } => {
                    tokio::time::sleep(delay).await;
                    let _ = tx
                        .send(Ok(LlmEvent::ToolCalls(vec![ToolCall {
                            id: "call_fake".into(),
                            kind: "function".into(),
                            function: FunctionCall { name, arguments },
                        }])))
                        .await;
                    let _ = tx
                        .send(Ok(LlmEvent::Finish {
                            reason: Some("tool_calls".into()),
                        }))
                        .await;
                }
            }
        });
        Ok(rx)
    }

    async fn complete(
        &self,
        endpoint: &LlmEndpoint,
        request: &ChatRequest,
    ) -> Result<Completion, ProviderError> {
        let mut rx = self.stream(endpoint, request).await?;
        let mut out = Completion::default();
        while let Some(event) = rx.recv().await {
            match event? {
                LlmEvent::Token(t) => out.content.push_str(&t),
                LlmEvent::ToolCalls(c) => out.tool_calls = c,
                LlmEvent::Usage(u) => out.usage = u,
                LlmEvent::Finish { reason } => {
                    out.finish_reason = reason;
                    break;
                }
            }
        }
        Ok(out)
    }
}
