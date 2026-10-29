//! The per-call real-time pipeline.
//!
//! ```text
//! media in ──► VAD (worker pool) ──► STT gate ──► STT stream ──► turn detector
//!                                                             │ final transcript
//!                                                             ▼
//!                          turn task: LLM stream (+ tools, retries, fallback)
//!                                                             │ tokens
//!                                                             ▼
//!                          shaper: sanitize, clauses, length caps
//!                                                             │ clauses
//!                                                             ▼
//!                          TTS session (+ hot spare) ──► playout (20 ms, paced) ──► media out
//! ```
//!
//! [`run_call`] owns one call. Every stage is its own task connected by
//! bounded channels; the call actor is a single loop that reacts to events
//! and owns all state, so the hot path takes no locks.

pub mod actor;
pub mod playout;
pub mod prompt;
pub mod romanized;
pub mod shaper;
pub mod turn;
pub mod types;
pub mod vad;

pub use actor::run_call;
pub use types::*;
