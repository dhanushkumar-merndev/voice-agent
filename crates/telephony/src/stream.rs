//! Plivo audio-stream WebSocket protocol.
//!
//! Inbound (Plivo → us): `start`, `media` (~20 ms of base64 μ-law),
//! `dtmf`, `playedStream` (a checkpoint we set has finished playing) and
//! `clearedAudio`. Plivo has no `stop` event; the stream ends when the
//! socket closes.
//!
//! Outbound (us → Plivo): `playAudio`, `checkpoint`, `clearAudio`.
//! Each `playAudio` payload should stay under 16 KB of base64.

use serde::{Deserialize, Serialize};

/// Messages Plivo sends on the stream.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum Inbound {
    /// First message: identifies the call and stream.
    Start {
        /// Stream details.
        start: StartInfo,
        /// Custom headers from the `<Stream extraHeaders>` attribute.
        #[serde(default)]
        extra_headers: Option<String>,
    },
    /// A chunk of caller audio.
    Media {
        /// The audio chunk.
        media: MediaInfo,
    },
    /// The caller pressed a key.
    Dtmf {
        /// Key details.
        dtmf: DtmfInfo,
    },
    /// Audio up to the named checkpoint has been played to the caller.
    PlayedStream {
        /// Checkpoint name we sent.
        name: String,
    },
    /// The playback queue was cleared.
    ClearedAudio {},
}

/// Payload of the `start` event.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartInfo {
    /// Plivo CallUUID.
    pub call_id: String,
    /// Stream ID, required on `checkpoint` and `clearAudio`.
    pub stream_id: String,
    /// Audio format of inbound media.
    #[serde(default)]
    pub media_format: Option<MediaFormat>,
}

/// Audio format announced on `start`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaFormat {
    /// e.g. `audio/x-mulaw`.
    pub encoding: String,
    /// e.g. 8000.
    pub sample_rate: u32,
}

/// Payload of the `media` event.
#[derive(Debug, Clone, Deserialize)]
pub struct MediaInfo {
    /// Base64 audio.
    pub payload: String,
    /// Chunk counter, when present.
    #[serde(default)]
    pub chunk: Option<u64>,
}

/// Payload of the `dtmf` event.
#[derive(Debug, Clone, Deserialize)]
pub struct DtmfInfo {
    /// The key: 0-9, *, #, A-D.
    pub digit: String,
}

/// Messages we send on the stream.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum Outbound<'a> {
    /// Queue audio for playback.
    PlayAudio {
        /// The audio.
        media: PlayMedia<'a>,
    },
    /// Mark a point in the playback queue; Plivo answers with `playedStream`.
    Checkpoint {
        /// Stream ID from `start`.
        #[serde(rename = "streamId")]
        stream_id: &'a str,
        /// Checkpoint name.
        name: &'a str,
    },
    /// Drop all queued audio (barge-in).
    ClearAudio {
        /// Stream ID from `start`.
        #[serde(rename = "streamId")]
        stream_id: &'a str,
    },
}

/// Audio for `playAudio`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayMedia<'a> {
    /// Must match the stream's content type.
    pub content_type: &'static str,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Base64 audio.
    pub payload: &'a str,
}

impl<'a> Outbound<'a> {
    /// A `playAudio` message for base64 μ-law at 8 kHz.
    pub fn play_mulaw(payload_b64: &'a str) -> Self {
        Self::PlayAudio {
            media: PlayMedia {
                content_type: "audio/x-mulaw",
                sample_rate: 8000,
                payload: payload_b64,
            },
        }
    }

    /// Serializes to JSON.
    pub fn to_json(&self) -> String {
        // Serializing these plain structs cannot fail.
        serde_json::to_string(self).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inbound_events() {
        let start = r#"{"event":"start","sequenceNumber":1,"extra_headers":"","start":{"callId":"c-1","streamId":"s-1","accountId":"a","tracks":["inbound"],"mediaFormat":{"encoding":"audio/x-mulaw","sampleRate":8000}}}"#;
        match serde_json::from_str::<Inbound>(start).unwrap() {
            Inbound::Start { start, .. } => {
                assert_eq!(start.call_id, "c-1");
                assert_eq!(start.stream_id, "s-1");
                assert_eq!(start.media_format.unwrap().sample_rate, 8000);
            }
            other => panic!("unexpected {other:?}"),
        }
        let media = r#"{"event":"media","sequenceNumber":2,"streamId":"s-1","media":{"track":"inbound","timestamp":"1","chunk":1,"payload":"//8="}}"#;
        assert!(matches!(
            serde_json::from_str::<Inbound>(media).unwrap(),
            Inbound::Media { .. }
        ));
        let played = r#"{"event":"playedStream","sequenceNumber":3,"streamId":"s-1","name":"c7"}"#;
        match serde_json::from_str::<Inbound>(played).unwrap() {
            Inbound::PlayedStream { name } => assert_eq!(name, "c7"),
            other => panic!("unexpected {other:?}"),
        }
        let cleared = r#"{"event":"clearedAudio","sequenceNumber":4,"streamId":"s-1"}"#;
        assert!(matches!(
            serde_json::from_str::<Inbound>(cleared).unwrap(),
            Inbound::ClearedAudio {}
        ));
    }

    #[test]
    fn serializes_outbound_events() {
        let play = Outbound::play_mulaw("AAA=").to_json();
        assert_eq!(
            play,
            r#"{"event":"playAudio","media":{"contentType":"audio/x-mulaw","sampleRate":8000,"payload":"AAA="}}"#
        );
        let cp = Outbound::Checkpoint {
            stream_id: "s",
            name: "c1",
        }
        .to_json();
        assert_eq!(cp, r#"{"event":"checkpoint","streamId":"s","name":"c1"}"#);
        let clear = Outbound::ClearAudio { stream_id: "s" }.to_json();
        assert_eq!(clear, r#"{"event":"clearAudio","streamId":"s"}"#);
    }
}
