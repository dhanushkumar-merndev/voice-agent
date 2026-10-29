//! Streaming speech-to-text abstraction.
//!
//! A session is opened per call (before answer, to hide the handshake).
//! The pipeline marks utterances explicitly with
//! [`SttCommand::SpeechStart`] / [`SttCommand::SpeechEnd`] and only sends
//! audio in between, so silence is never billed.

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::mpsc;

use crate::ProviderError;

/// Options for a session.
#[derive(Debug, Clone, Default)]
pub struct SttOptions {
    /// BCP-47 code, or `None` for automatic language detection.
    pub language: Option<String>,
}

/// Commands to an open session.
#[derive(Debug, Clone)]
pub enum SttCommand {
    /// The caller started speaking.
    SpeechStart,
    /// 8 kHz μ-law audio (≤ 100 ms per command).
    Audio(Bytes),
    /// The caller stopped speaking: finalize the utterance.
    SpeechEnd,
    /// Ends the session gracefully; a [`SttEvent::SessionEnd`] follows.
    End,
}

/// Events from a session.
#[derive(Debug, Clone, PartialEq)]
pub enum SttEvent {
    /// Interim transcript for the current utterance.
    Partial {
        /// Utterance counter.
        utterance: u32,
        /// Text so far.
        text: String,
        /// Detected language (auto mode only).
        language: Option<String>,
    },
    /// Final transcript for an utterance.
    Final {
        /// Utterance counter.
        utterance: u32,
        /// Final text.
        text: String,
        /// Detected language (auto mode only).
        language: Option<String>,
        /// Detection confidence 0..1 (auto mode only).
        language_confidence: Option<f32>,
    },
    /// The session closed; `billed_secs` is the provider's billed audio.
    SessionEnd {
        /// Billed audio duration in seconds.
        billed_secs: f64,
    },
    /// The provider reported an error.
    Error {
        /// Provider error code.
        code: String,
        /// Message.
        message: String,
        /// `true` if the session is unusable.
        fatal: bool,
    },
}

/// An open STT session: a command sender and an event receiver.
#[derive(Debug)]
pub struct SttSession {
    /// Commands to the provider. Audio is sent with `try_send` so a slow
    /// provider never blocks the call loop.
    pub commands: mpsc::Sender<SttCommand>,
    /// Events from the provider. `None` means the session is gone.
    pub events: mpsc::Receiver<SttEvent>,
}

impl SttSession {
    /// Ends the session and waits briefly for the provider's billed duration.
    pub async fn close(mut self, wait: std::time::Duration) -> Option<f64> {
        if self.commands.send(SttCommand::End).await.is_err() {
            return None;
        }
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            match tokio::time::timeout_at(deadline, self.events.recv()).await {
                Ok(Some(SttEvent::SessionEnd { billed_secs })) => return Some(billed_secs),
                Ok(Some(_)) => continue,
                Ok(None) | Err(_) => return None,
            }
        }
    }
}

/// A streaming STT provider.
#[async_trait]
pub trait SttProvider: Send + Sync {
    /// Short provider name for metrics and logs.
    fn name(&self) -> &'static str;

    /// Opens a session.
    async fn open(&self, options: SttOptions) -> Result<SttSession, ProviderError>;
}
