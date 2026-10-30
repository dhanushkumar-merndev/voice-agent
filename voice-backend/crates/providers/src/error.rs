//! Provider error type.

/// Failure talking to a provider.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ProviderError {
    /// The provider has no credentials configured.
    #[error("provider not configured: {0}")]
    NotConfigured(&'static str),
    /// Could not connect (DNS, TCP, TLS, WebSocket handshake).
    #[error("connect failed: {0}")]
    Connect(String),
    /// The provider answered with an HTTP error status.
    #[error("http {status}: {body}")]
    Status {
        /// HTTP status code.
        status: u16,
        /// Response body (truncated).
        body: String,
    },
    /// The provider sent something we could not understand, or reported an error.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// The provider did not answer in time.
    #[error("timed out")]
    Timeout,
    /// The connection closed unexpectedly.
    #[error("connection closed")]
    Closed,
}

impl ProviderError {
    /// `true` for errors where retrying (possibly elsewhere) may help:
    /// connection problems, timeouts, 429 and 5xx.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Connect(_) | Self::Timeout | Self::Closed => true,
            Self::Status { status, .. } => *status == 429 || *status >= 500,
            Self::NotConfigured(_) | Self::Protocol(_) => false,
        }
    }

    /// Short label for metrics.
    pub fn metric_code(&self) -> String {
        match self {
            Self::NotConfigured(_) => "not_configured".to_owned(),
            Self::Connect(_) => "connect".to_owned(),
            Self::Status { status, .. } => status.to_string(),
            Self::Protocol(_) => "protocol".to_owned(),
            Self::Timeout => "timeout".to_owned(),
            Self::Closed => "closed".to_owned(),
        }
    }
}

impl From<reqwest::Error> for ProviderError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            Self::Timeout
        } else if e.is_connect() || e.is_request() {
            Self::Connect(e.to_string())
        } else {
            Self::Protocol(e.to_string())
        }
    }
}
