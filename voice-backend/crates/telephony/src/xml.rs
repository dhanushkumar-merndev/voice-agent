//! Plivo answer XML.

/// Content type of the bidirectional stream: μ-law at 8 kHz, which is
/// what the phone network carries, so neither direction is transcoded.
pub const STREAM_CONTENT_TYPE: &str = "audio/x-mulaw;rate=8000";

/// Answer XML that connects the call to our media WebSocket.
///
/// `bidirectional` lets us play audio back; `keepCallAlive` keeps the call
/// up for as long as the stream is open. `stream_timeout_secs` is a safety
/// net above the call's own max duration.
pub fn stream_answer(ws_url: &str, status_callback_url: &str, stream_timeout_secs: u32) -> String {
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            "<Response>",
            r#"<Stream bidirectional="true" keepCallAlive="true" contentType="{ct}" "#,
            r#"streamTimeout="{timeout}" statusCallbackUrl="{cb}" statusCallbackMethod="POST">"#,
            "{url}</Stream>",
            "</Response>"
        ),
        ct = STREAM_CONTENT_TYPE,
        timeout = stream_timeout_secs,
        cb = escape(status_callback_url),
        url = escape(ws_url),
    )
}

/// XML that ends the call immediately.
pub fn hangup() -> String {
    r#"<?xml version="1.0" encoding="UTF-8"?><Response><Hangup/></Response>"#.to_owned()
}

/// Escapes text for XML attribute and element content.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_xml_is_escaped() {
        let xml = stream_answer(
            "wss://h/plivo/stream/1?t=a&b",
            "https://h/plivo/stream-status/1",
            600,
        );
        assert!(xml.contains(r#"bidirectional="true""#));
        assert!(xml.contains(r#"contentType="audio/x-mulaw;rate=8000""#));
        assert!(xml.contains("wss://h/plivo/stream/1?t=a&amp;b</Stream>"));
        assert!(!xml.contains("a&b"));
    }
}
