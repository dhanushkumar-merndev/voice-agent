//! Sarvam speech providers (protocols verified against docs.sarvam.ai on 2026-10-28).

mod batch;
mod stt;
mod tts;

pub use batch::{transcribe_wav, BatchTranscript, SarvamBatchSttConfig, BATCH_MAX_SECONDS};
pub use stt::{SarvamStt, SarvamSttConfig};
pub use tts::{SarvamTts, SarvamTtsConfig};

/// Parses a number that Sarvam may send as a JSON number or a string.
fn num(v: &serde_json::Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn record_error(service: &'static str, code: &str) {
    metrics::counter!(
        common::metric_names::SARVAM_ERRORS_TOTAL,
        "service" => service,
        "code" => code.to_owned()
    )
    .increment(1);
}
