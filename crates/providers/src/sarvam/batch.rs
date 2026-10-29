//! Sarvam batch (REST) speech-to-text for short clips (up to 30 s).
//!
//! `POST /speech-to-text` with a multipart `file`. Used after the call to
//! re-transcribe caller turns: it is more accurate than the realtime stream.

use reqwest::multipart::{Form, Part};
use serde::Deserialize;

use super::record_error;
use crate::ProviderError;

/// Longest clip the REST endpoint accepts.
pub const BATCH_MAX_SECONDS: f64 = 30.0;

/// Batch STT settings.
#[derive(Debug, Clone)]
pub struct SarvamBatchSttConfig {
    /// Endpoint URL, normally `https://api.sarvam.ai/speech-to-text`.
    pub url: String,
    /// Model, normally `saaras:v4`.
    pub model: String,
    /// `api-subscription-key`.
    pub api_key: String,
}

/// One batch transcription result.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BatchTranscript {
    /// The transcript text.
    pub transcript: String,
    /// Detected language (BCP-47), when Sarvam reports one.
    #[serde(default)]
    pub language_code: Option<String>,
}

/// Transcribes one WAV clip with auto-detected language.
pub async fn transcribe_wav(
    http: &reqwest::Client,
    config: &SarvamBatchSttConfig,
    wav: Vec<u8>,
) -> Result<BatchTranscript, ProviderError> {
    let file = Part::bytes(wav)
        .file_name("turn.wav")
        .mime_str("audio/wav")
        .map_err(|e| ProviderError::Protocol(e.to_string()))?;
    let form = Form::new()
        .part("file", file)
        .text("model", config.model.clone())
        .text("mode", "transcribe")
        .text("language_code", "unknown");
    let resp = http
        .post(&config.url)
        .header("api-subscription-key", &config.api_key)
        .multipart(form)
        .send()
        .await?;
    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        let body: String = resp
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(500)
            .collect();
        record_error("stt_batch", &status.to_string());
        return Err(ProviderError::Status { status, body });
    }
    resp.json::<BatchTranscript>()
        .await
        .map_err(|e| ProviderError::Protocol(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_response() {
        let r: BatchTranscript = serde_json::from_str(
            r#"{"request_id":"x","transcript":"வணக்கம்","language_code":"ta-IN","language_probability":0.93}"#,
        )
        .unwrap();
        assert_eq!(r.transcript, "வணக்கம்");
        assert_eq!(r.language_code.as_deref(), Some("ta-IN"));
    }
}
