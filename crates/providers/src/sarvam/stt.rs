//! Sarvam realtime streaming STT (`saaras:v4`; `saaras:v3-realtime` also works).
//!
//! Protocol (`wss://api.sarvam.ai/speech-to-text-realtime/ws`, header
//! `API-Subscription-Key`):
//!
//! - Query: `model`, `language_code` (`auto` or BCP-47), `encoding=mulaw`,
//!   `sample_rate=8000`, `endpointing=manual`, `stream_type=fast`.
//! - Client → server: `{"event":"speech_start"}`,
//!   `{"event":"audio_input","audio":"<base64>"}`, `{"event":"speech_end"}`,
//!   `{"event":"ping"}`, `{"event":"end"}`.
//! - Server → client: `session.begin`, `transcript.partial`
//!   (`utterance_idx`, `text`, `language`), `transcript.final` (adds
//!   `language_confidence`), `session.end` (`audio_duration_s` = billed
//!   audio), `error` (`code`, `is_fatal`, `message`), `pong`.
//!
//! With manual endpointing the pipeline decides turn boundaries with its
//! own VAD and only streams audio while the caller speaks.

use std::time::Duration;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use bytes::BytesMut;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use super::{num, record_error};
use crate::{
    stt::{SttCommand, SttEvent, SttOptions, SttProvider, SttSession},
    ws, ProviderError,
};

/// Settings for [`SarvamStt`].
#[derive(Debug, Clone)]
pub struct SarvamSttConfig {
    /// WebSocket URL.
    pub url: String,
    /// Model ID.
    pub model: String,
    /// API key.
    pub api_key: String,
}

/// Sarvam realtime STT provider.
#[derive(Debug, Clone)]
pub struct SarvamStt {
    config: SarvamSttConfig,
}

impl SarvamStt {
    /// Creates the provider.
    pub fn new(config: SarvamSttConfig) -> Self {
        Self { config }
    }

    fn url(&self, options: &SttOptions) -> String {
        let language = options.language.as_deref().unwrap_or("auto");
        // Odia is `or-IN` on the realtime endpoint (legacy APIs used `od-IN`).
        let language = if language == "od-IN" {
            "or-IN"
        } else {
            language
        };
        format!(
            "{}?model={}&language_code={}&encoding=mulaw&sample_rate=8000&endpointing=manual&stream_type=fast",
            self.config.url, self.config.model, language
        )
    }
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const PING_EVERY: Duration = Duration::from_secs(20);
/// Coalesce up to 100 ms of queued audio into one message.
const MAX_BATCH_BYTES: usize = 800;

#[async_trait]
impl SttProvider for SarvamStt {
    fn name(&self) -> &'static str {
        "sarvam"
    }

    async fn open(&self, options: SttOptions) -> Result<SttSession, ProviderError> {
        let socket = ws::connect(
            &self.url(&options),
            "api-subscription-key",
            &self.config.api_key,
            CONNECT_TIMEOUT,
        )
        .await
        .inspect_err(|e| record_error("stt", &e.metric_code()))?;
        let (mut sink, mut stream) = socket.split();
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<SttCommand>(256);
        let (evt_tx, evt_rx) = mpsc::channel::<SttEvent>(64);

        // Writer: commands → JSON, with keep-alive pings.
        tokio::spawn(async move {
            let mut ping = tokio::time::interval(PING_EVERY);
            ping.tick().await;
            let mut batch = BytesMut::with_capacity(MAX_BATCH_BYTES);
            loop {
                let cmd = tokio::select! {
                    cmd = cmd_rx.recv() => cmd,
                    _ = ping.tick() => {
                        if sink.send(Message::Text(json!({"event": "ping"}).to_string().into())).await.is_err() {
                            break;
                        }
                        continue;
                    }
                };
                let Some(cmd) = cmd else { break };
                let mut messages = Vec::with_capacity(2);
                match cmd {
                    SttCommand::Audio(first) => {
                        batch.clear();
                        batch.extend_from_slice(&first);
                        // Coalesce audio already waiting in the queue.
                        let mut next = None;
                        while batch.len() < MAX_BATCH_BYTES {
                            match cmd_rx.try_recv() {
                                Ok(SttCommand::Audio(more)) => batch.extend_from_slice(&more),
                                Ok(other) => {
                                    next = Some(other);
                                    break;
                                }
                                Err(_) => break,
                            }
                        }
                        messages.push(
                            json!({"event": "audio_input", "audio": STANDARD.encode(&batch)})
                                .to_string(),
                        );
                        if let Some(other) = next {
                            messages.push(control_message(&other));
                        }
                    }
                    other => messages.push(control_message(&other)),
                }
                for m in messages {
                    if sink.send(Message::Text(m.into())).await.is_err() {
                        return;
                    }
                }
            }
            let _ = sink.close().await;
        });

        // Reader: JSON → events.
        tokio::spawn(async move {
            while let Some(msg) = stream.next().await {
                let text = match msg {
                    Ok(Message::Text(t)) => t,
                    Ok(Message::Close(frame)) => {
                        if let Some(f) = frame {
                            let code = u16::from(f.code);
                            if code != 1000 {
                                record_error("stt", &code.to_string());
                                let _ = evt_tx
                                    .send(SttEvent::Error {
                                        code: code.to_string(),
                                        message: f.reason.to_string(),
                                        fatal: true,
                                    })
                                    .await;
                            }
                        }
                        break;
                    }
                    Ok(_) => continue,
                    Err(e) => {
                        record_error("stt", "socket");
                        let _ = evt_tx
                            .send(SttEvent::Error {
                                code: "socket".into(),
                                message: e.to_string(),
                                fatal: true,
                            })
                            .await;
                        break;
                    }
                };
                let Ok(value) = serde_json::from_str::<Value>(text.as_str()) else {
                    continue;
                };
                if let Some(event) = parse_event(&value) {
                    let end = matches!(event, SttEvent::SessionEnd { .. });
                    if evt_tx.send(event).await.is_err() || end {
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

fn control_message(cmd: &SttCommand) -> String {
    let event = match cmd {
        SttCommand::SpeechStart => "speech_start",
        SttCommand::SpeechEnd => "speech_end",
        SttCommand::End => "end",
        SttCommand::Audio(_) => "audio_input",
    };
    json!({ "event": event }).to_string()
}

/// Maps one server message to an event (`None` for messages we ignore).
fn parse_event(v: &Value) -> Option<SttEvent> {
    let utterance = v["utterance_idx"].as_u64().unwrap_or(0) as u32;
    let text = || v["text"].as_str().unwrap_or_default().trim().to_owned();
    let language = || v["language"].as_str().map(str::to_owned);
    match v["event"].as_str()? {
        "transcript.partial" => Some(SttEvent::Partial {
            utterance,
            text: text(),
            language: language(),
        }),
        "transcript.final" => Some(SttEvent::Final {
            utterance,
            text: text(),
            language: language(),
            language_confidence: num(&v["language_confidence"]).map(|c| c as f32),
        }),
        "session.end" => Some(SttEvent::SessionEnd {
            billed_secs: num(&v["audio_duration_s"]).unwrap_or(0.0),
        }),
        "error" => {
            let code = v["code"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| v["code"].as_i64().map(|c| c.to_string()))
                .unwrap_or_else(|| "unknown".into());
            record_error("stt", &code);
            Some(SttEvent::Error {
                code,
                message: v["message"].as_str().unwrap_or_default().to_owned(),
                fatal: v["is_fatal"].as_bool().unwrap_or(false),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_documented_messages() {
        let partial = json!({"event":"transcript.partial","utterance_idx":2,"text":" नमस्ते ","language":"hi-IN"});
        assert_eq!(
            parse_event(&partial),
            Some(SttEvent::Partial {
                utterance: 2,
                text: "नमस्ते".into(),
                language: Some("hi-IN".into())
            })
        );
        let fin = json!({"event":"transcript.final","utterance_idx":2,"text":"hello","language":"en-IN","language_confidence":"0.93"});
        match parse_event(&fin) {
            Some(SttEvent::Final {
                language_confidence,
                ..
            }) => assert!((language_confidence.unwrap() - 0.93).abs() < 1e-6),
            other => panic!("unexpected {other:?}"),
        }
        let end = json!({"event":"session.end","request_id":"r","audio_duration_s":"12.5"});
        assert_eq!(
            parse_event(&end),
            Some(SttEvent::SessionEnd { billed_secs: 12.5 })
        );
        let err =
            json!({"event":"error","code":"rate_limited","is_fatal":true,"message":"slow down"});
        assert!(matches!(
            parse_event(&err),
            Some(SttEvent::Error { fatal: true, .. })
        ));
        assert_eq!(parse_event(&json!({"event":"session.begin"})), None);
    }

    #[test]
    fn url_uses_auto_and_or_in() {
        let stt = SarvamStt::new(SarvamSttConfig {
            url: "wss://x/ws".into(),
            model: "saaras:v4".into(),
            api_key: "k".into(),
        });
        assert!(stt
            .url(&SttOptions { language: None })
            .contains("language_code=auto"));
        assert!(stt
            .url(&SttOptions {
                language: Some("od-IN".into())
            })
            .contains("language_code=or-IN"));
        assert!(stt
            .url(&SttOptions::default())
            .contains("encoding=mulaw&sample_rate=8000&endpointing=manual"));
    }
}
