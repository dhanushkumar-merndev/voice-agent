//! Audio primitives for 8 kHz mono telephony audio.
//!
//! - [`mulaw`]: G.711 μ-law ↔ 16-bit PCM using lookup tables.
//! - [`wav`]: streaming RIFF/WAV header stripper for provider audio.
//! - [`frame`]: splits byte streams into fixed 20 ms frames.
//! - [`preroll`]: ring buffer of recent frames, replayed to STT when speech starts.
//!
//! Everything works on borrowed slices or [`bytes::Bytes`]; the hot path
//! does no per-sample allocation.

pub mod frame;
pub mod mulaw;
pub mod preroll;
pub mod wav;

/// Telephony sample rate (Hz).
pub const SAMPLE_RATE: u32 = 8000;
/// Samples (and μ-law bytes) per 20 ms frame at 8 kHz.
pub const FRAME_SAMPLES: usize = 160;
/// Frame duration in milliseconds.
pub const FRAME_MS: u32 = 20;

/// Duration in milliseconds of `bytes` of 8 kHz μ-law audio.
pub fn mulaw_duration_ms(bytes: usize) -> u64 {
    (bytes as u64 * 1000) / u64::from(SAMPLE_RATE)
}
