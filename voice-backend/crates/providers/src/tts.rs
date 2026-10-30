//! Streaming text-to-speech abstraction.
//!
//! A session is a persistent connection for one TTS model. The pipeline
//! sends one [`TtsCommand::Speak`] per clause; the provider streams μ-law
//! 8 kHz audio for it and then [`TtsEvent::Done`]. Clauses are processed in
//! order, so the next clause can be queued while the current one plays.

use std::time::Duration;

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use common::assets::Voice;
use tokio::sync::mpsc;

use crate::ProviderError;

/// Commands to an open session.
#[derive(Debug, Clone)]
pub enum TtsCommand {
    /// Changes speaker, language or pace for following clauses (same model).
    Configure(Voice),
    /// Synthesizes `text` (flushed immediately) and tags its audio with `id`.
    Speak {
        /// Caller-chosen clause ID.
        id: u64,
        /// Text to speak.
        text: String,
    },
    /// Closes the connection. Pending audio is discarded.
    Close,
}

/// Events from a session.
#[derive(Debug, Clone)]
pub enum TtsEvent {
    /// μ-law 8 kHz audio for clause `id`.
    Audio {
        /// Clause ID.
        id: u64,
        /// Audio bytes.
        data: Bytes,
    },
    /// All audio for clause `id` has been delivered.
    Done {
        /// Clause ID.
        id: u64,
    },
    /// The provider reported an error.
    Error {
        /// Message.
        message: String,
        /// `true` if the session is unusable.
        fatal: bool,
    },
}

/// An open TTS session.
#[derive(Debug)]
pub struct TtsSession {
    /// Model this session synthesizes with.
    pub model: String,
    /// Commands to the provider.
    pub commands: mpsc::Sender<TtsCommand>,
    /// Events from the provider. `None` means the session is gone.
    pub events: mpsc::Receiver<TtsEvent>,
}

/// A streaming TTS provider.
#[async_trait]
pub trait TtsProvider: Send + Sync {
    /// Short provider name for metrics and logs.
    fn name(&self) -> &'static str;

    /// Opens a session configured for `voice`.
    async fn open(&self, voice: &Voice) -> Result<TtsSession, ProviderError>;
}

/// Synthesizes `text` completely on a fresh session (used for cached lines
/// such as the greeting). Returns μ-law 8 kHz audio.
pub async fn synthesize(
    provider: &dyn TtsProvider,
    voice: &Voice,
    text: &str,
    timeout: Duration,
) -> Result<Bytes, ProviderError> {
    let mut session = provider.open(voice).await?;
    session
        .commands
        .send(TtsCommand::Speak {
            id: 0,
            text: text.to_owned(),
        })
        .await
        .map_err(|_| ProviderError::Closed)?;
    let mut audio = BytesMut::new();
    let result = tokio::time::timeout(timeout, async {
        while let Some(event) = session.events.recv().await {
            match event {
                TtsEvent::Audio { data, .. } => audio.extend_from_slice(&data),
                TtsEvent::Done { .. } => return Ok(()),
                TtsEvent::Error { message, .. } => return Err(ProviderError::Protocol(message)),
            }
        }
        Err(ProviderError::Closed)
    })
    .await;
    let _ = session.commands.send(TtsCommand::Close).await;
    match result {
        Ok(Ok(())) if !audio.is_empty() => Ok(audio.freeze()),
        Ok(Ok(())) => Err(ProviderError::Protocol("no audio returned".to_owned())),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(ProviderError::Timeout),
    }
}
