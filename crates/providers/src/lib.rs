//! Provider abstraction for the real-time pipeline.
//!
//! The pipeline only sees the traits [`stt::SttProvider`],
//! [`tts::TtsProvider`] and [`llm::LlmProvider`]; any provider can be
//! swapped by configuration without touching pipeline code.
//!
//! Implementations:
//! - [`sarvam::SarvamStt`]: Sarvam realtime streaming STT (`saaras:v4`).
//! - [`sarvam::SarvamTts`]: Sarvam Bulbul streaming TTS over WebSocket.
//! - [`openai::OpenAiCompatLlm`]: any OpenAI-compatible chat completions API
//!   (Sarvam's, by default), streaming with tool calls.
//! - [`fake`]: deterministic fakes for tests and offline runs.

pub mod error;
pub mod fake;
pub mod llm;
pub mod metered;
pub mod openai;
pub mod sarvam;
pub mod stt;
pub mod tts;
mod ws;

pub use error::ProviderError;
