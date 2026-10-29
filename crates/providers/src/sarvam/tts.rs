//! Sarvam Bulbul streaming TTS over WebSocket.
//!
//! Protocol (`wss://api.sarvam.ai/text-to-speech/ws?model=<model>&send_completion_event=true`,
//! header `api-subscription-key`):
//!
//! - Client → server, per utterance: `{"type":"config","data":{...}}`
//!   (sent once and again on voice changes), `{"type":"text","data":{"text":...}}`,
//!   `{"type":"flush"}`. Keep-alive: `{"type":"ping"}` (idle sockets close
//!   after about a minute).
//! - Server → client: `{"type":"audio","data":{"audio":"<base64>"}}`,
//!   `{"type":"event","data":{"event_type":"final"}}` after each flushed
//!   utterance, `{"type":"error","data":{"message":...}}`.
//!
//! There is no cancel message: barge-in closes the socket (the pipeline
//! keeps a spare). v4 Flash streams `wav` or `mp3` only per its docs, so
//! by default we request 8 kHz WAV, strip each utterance's header and
//! μ-law-encode locally. `SARVAM_TTS_CODEC=mulaw` requests μ-law directly.
//! `enable_preprocessing` lets Sarvam normalize numbers, ₹, dates and names.

use std::{collections::VecDeque, time::Duration};

use async_trait::async_trait;
use audio::{
    mulaw,
    wav::{WavEncoding, WavStripper},
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use bytes::Bytes;
use common::{assets::Voice, config::TtsCodec};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use super::record_error;
use crate::{
    tts::{TtsCommand, TtsEvent, TtsProvider, TtsSession},
    ws, ProviderError,
};

/// Settings for [`SarvamTts`].
#[derive(Debug, Clone)]
pub struct SarvamTtsConfig {
    /// WebSocket URL (without query).
    pub url: String,
    /// API key.
    pub api_key: String,
    /// Codec requested from the server.
    pub codec: TtsCodec,
}

/// Sarvam Bulbul TTS provider.
#[derive(Debug, Clone)]
pub struct SarvamTts {
    config: SarvamTtsConfig,
}

impl SarvamTts {
    /// Creates the provider.
    pub fn new(config: SarvamTtsConfig) -> Self {
        Self { config }
    }
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const PING_EVERY: Duration = Duration::from_secs(30);
/// If an utterance produced audio and then nothing for this long without a
/// completion event, treat it as done.
const DONE_FALLBACK: Duration = Duration::from_millis(1500);

fn config_message(voice: &Voice, codec: TtsCodec) -> String {
    json!({
        "type": "config",
        "data": {
            "speaker": voice.speaker,
            "language_code": voice.language,
            "pace": voice.pace,
            "speech_sample_rate": 8000,
            "output_audio_codec": codec.as_str(),
            "enable_preprocessing": true,
        }
    })
    .to_string()
}

#[async_trait]
impl TtsProvider for SarvamTts {
    fn name(&self) -> &'static str {
        "sarvam"
    }

    async fn open(&self, voice: &Voice) -> Result<TtsSession, ProviderError> {
        let url = format!(
            "{}?model={}&send_completion_event=true",
            self.config.url, voice.model
        );
        let socket = ws::connect(
            &url,
            "api-subscription-key",
            &self.config.api_key,
            CONNECT_TIMEOUT,
        )
        .await
        .inspect_err(|e| record_error("tts", &e.metric_code()))?;
        let (mut sink, mut stream) = socket.split();
        let codec = self.config.codec;
        sink.send(Message::Text(config_message(voice, codec).into()))
            .await
            .map_err(|e| ProviderError::Connect(e.to_string()))?;

        let (cmd_tx, mut cmd_rx) = mpsc::channel::<TtsCommand>(64);
        let (evt_tx, evt_rx) = mpsc::channel::<TtsEvent>(256);
        // Clause IDs in the order their text was sent; audio belongs to the front.
        let (id_tx, mut id_rx) = mpsc::unbounded_channel::<u64>();

        tokio::spawn(async move {
            let mut ping = tokio::time::interval(PING_EVERY);
            ping.tick().await;
            loop {
                let cmd = tokio::select! {
                    cmd = cmd_rx.recv() => cmd,
                    _ = ping.tick() => {
                        if sink.send(Message::Text(json!({"type": "ping"}).to_string().into())).await.is_err() {
                            break;
                        }
                        continue;
                    }
                };
                let ok = match cmd {
                    None | Some(TtsCommand::Close) => break,
                    Some(TtsCommand::Configure(v)) => sink
                        .send(Message::Text(config_message(&v, codec).into()))
                        .await
                        .is_ok(),
                    Some(TtsCommand::Speak { id, text }) => {
                        let _ = id_tx.send(id);
                        let text_msg = json!({"type": "text", "data": {"text": text}}).to_string();
                        sink.send(Message::Text(text_msg.into())).await.is_ok()
                            && sink
                                .send(Message::Text(json!({"type": "flush"}).to_string().into()))
                                .await
                                .is_ok()
                    }
                };
                if !ok {
                    break;
                }
            }
            let _ = sink.close().await;
        });

        tokio::spawn(async move {
            let mut pending: VecDeque<u64> = VecDeque::new();
            let mut stripper = WavStripper::new();
            let mut got_audio = false;
            let mut out = Vec::with_capacity(4096);
            // A PCM sample split across messages: its first byte waits here.
            let mut carry: Option<u8> = None;
            let mut pcm = Vec::with_capacity(4096);
            loop {
                let fallback = tokio::time::sleep(DONE_FALLBACK);
                let msg = tokio::select! {
                    msg = stream.next() => msg,
                    Some(id) = id_rx.recv() => { pending.push_back(id); continue; }
                    () = fallback, if got_audio => {
                        // No completion event arrived: close out the current clause.
                        if let Some(id) = pending.pop_front() {
                            if evt_tx.send(TtsEvent::Done { id }).await.is_err() { break; }
                        }
                        stripper.reset();
                        got_audio = false;
                        continue;
                    }
                };
                while let Ok(id) = id_rx.try_recv() {
                    pending.push_back(id);
                }
                let text = match msg {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Close(frame))) => {
                        if let Some(f) = frame {
                            let code = u16::from(f.code);
                            if code != 1000 {
                                record_error("tts", &code.to_string());
                                let _ = evt_tx
                                    .send(TtsEvent::Error {
                                        message: format!("closed {code}: {}", f.reason),
                                        fatal: true,
                                    })
                                    .await;
                            }
                        }
                        break;
                    }
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => {
                        record_error("tts", "socket");
                        let _ = evt_tx
                            .send(TtsEvent::Error {
                                message: e.to_string(),
                                fatal: true,
                            })
                            .await;
                        break;
                    }
                    None => break,
                };
                let Ok(v) = serde_json::from_str::<Value>(text.as_str()) else {
                    continue;
                };
                match v["type"].as_str() {
                    Some("audio") => {
                        let Some(b64) = v["data"]["audio"].as_str() else {
                            continue;
                        };
                        let Ok(raw) = STANDARD.decode(b64) else {
                            continue;
                        };
                        let Some(&id) = pending.front() else { continue };
                        let samples = stripper.push(Bytes::from(raw));
                        if samples.is_empty() {
                            continue;
                        }
                        got_audio = true;
                        let is_mulaw = match stripper.format() {
                            Some(f) => f.encoding == WavEncoding::Mulaw,
                            None => codec == TtsCodec::Mulaw,
                        };
                        let data = if is_mulaw {
                            samples
                        } else {
                            pcm.clear();
                            pcm.extend(carry.take());
                            pcm.extend_from_slice(&samples);
                            if pcm.len() % 2 == 1 {
                                carry = pcm.pop();
                            }
                            out.clear();
                            mulaw::encode_pcm16le(&pcm, &mut out);
                            Bytes::copy_from_slice(&out)
                        };
                        if evt_tx.send(TtsEvent::Audio { id, data }).await.is_err() {
                            break;
                        }
                    }
                    Some("event") | Some("events") => {
                        if v["data"]["event_type"].as_str() == Some("final") {
                            if let Some(id) = pending.pop_front() {
                                if evt_tx.send(TtsEvent::Done { id }).await.is_err() {
                                    break;
                                }
                            }
                            stripper.reset();
                            got_audio = false;
                            carry = None;
                        }
                    }
                    Some("error") => {
                        let message = v["data"]["message"]
                            .as_str()
                            .or_else(|| v["data"]["error"].as_str())
                            .unwrap_or("tts error")
                            .to_owned();
                        record_error("tts", "error_message");
                        // The utterance that failed will not complete.
                        if let Some(id) = pending.pop_front() {
                            let _ = evt_tx.send(TtsEvent::Done { id }).await;
                        }
                        if evt_tx
                            .send(TtsEvent::Error {
                                message,
                                fatal: false,
                            })
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    _ => {}
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_message_shape() {
        let voice = Voice {
            language: "hi-IN".into(),
            model: "bulbul:v4-flash".into(),
            speaker: "aparna_hi_customer".into(),
            pace: 1.0,
        };
        let v: Value = serde_json::from_str(&config_message(&voice, TtsCodec::Wav)).unwrap();
        assert_eq!(v["type"], "config");
        assert_eq!(v["data"]["speaker"], "aparna_hi_customer");
        assert_eq!(v["data"]["language_code"], "hi-IN");
        assert_eq!(v["data"]["speech_sample_rate"], 8000);
        assert_eq!(v["data"]["output_audio_codec"], "wav");
        assert_eq!(v["data"]["enable_preprocessing"], true);
    }
}
