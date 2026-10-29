//! Plivo integration.
//!
//! - [`client`]: REST client: place a call, start recording, hang up,
//!   download and delete recordings.
//! - [`xml`]: answer XML with a bidirectional `<Stream>`.
//! - [`stream`]: serde types for the audio-stream WebSocket protocol.
//! - [`signature`]: `X-Plivo-Signature-V3` webhook validation.
//!
//! Protocol references (verified 2026-10-28):
//! <https://plivo.com/docs/voice-agents/audio-streaming/concepts/audio-streaming-reference>,
//! <https://plivo.com/docs/voice/api/calls>, <https://plivo.com/docs/voice/concepts/signature-validation>.

pub mod client;
pub mod signature;
pub mod stream;
pub mod xml;

pub use client::{PlivoClient, PlivoError};
