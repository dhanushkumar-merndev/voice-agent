//! Application error type with stable, machine-readable codes.
//!
//! The HTTP layer turns an [`AppError`] into
//! `{"error": {"code": "...", "message": "..."}}`. Codes are part of the
//! public API and must not be renamed.

use serde::Serialize;

/// Stable error codes returned to API clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The request is malformed or fails validation.
    BadRequest,
    /// Missing or wrong `X-API-Key`.
    Unauthorized,
    /// Webhook signature missing or invalid.
    InvalidSignature,
    /// The resource does not exist (or belongs to another tenant).
    NotFound,
    /// The request conflicts with current state (e.g. idempotency key reuse).
    Conflict,
    /// The upload is too large.
    PayloadTooLarge,
    /// Too many requests.
    RateLimited,
    /// Outbound calls are not allowed at this time of day.
    OutsideCallHours,
    /// A provider needed for this request has no credentials configured.
    ProviderNotConfigured,
    /// An upstream provider failed.
    ProviderError,
    /// The server is draining or a dependency is down.
    Unavailable,
    /// An unexpected internal error.
    Internal,
}

impl ErrorCode {
    /// HTTP status code for this error.
    pub fn http_status(self) -> u16 {
        match self {
            Self::BadRequest => 400,
            Self::Unauthorized => 401,
            Self::InvalidSignature => 403,
            Self::NotFound => 404,
            Self::Conflict => 409,
            Self::PayloadTooLarge => 413,
            Self::OutsideCallHours => 422,
            Self::RateLimited => 429,
            Self::ProviderNotConfigured => 501,
            Self::ProviderError => 502,
            Self::Unavailable => 503,
            Self::Internal => 500,
        }
    }

    /// The code as it appears on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BadRequest => "bad_request",
            Self::Unauthorized => "unauthorized",
            Self::InvalidSignature => "invalid_signature",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::PayloadTooLarge => "payload_too_large",
            Self::RateLimited => "rate_limited",
            Self::OutsideCallHours => "outside_call_hours",
            Self::ProviderNotConfigured => "provider_not_configured",
            Self::ProviderError => "provider_error",
            Self::Unavailable => "unavailable",
            Self::Internal => "internal",
        }
    }
}

/// Error returned by request handlers and services.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// An error with a client-facing message.
    #[error("{message}")]
    Api {
        /// Stable error code.
        code: ErrorCode,
        /// Message safe to show to API clients.
        message: String,
    },
    /// Database failure (details are logged, not returned).
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    /// Object storage failure (details are logged, not returned).
    #[error("storage error: {0}")]
    Storage(String),
    /// Any other internal failure (details are logged, not returned).
    #[error("internal error: {0}")]
    Internal(String),
}

impl AppError {
    /// Creates an error with a client-facing message.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Api {
            code,
            message: message.into(),
        }
    }

    /// Shorthand for a `bad_request` error.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::BadRequest, message)
    }

    /// Shorthand for a `not_found` error.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    /// The stable code for this error.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Api { code, .. } => *code,
            Self::Db(sqlx::Error::RowNotFound) => ErrorCode::NotFound,
            Self::Db(_) | Self::Storage(_) | Self::Internal(_) => ErrorCode::Internal,
        }
    }

    /// Message that is safe to return to clients. Internal details are
    /// replaced with a generic message.
    pub fn public_message(&self) -> String {
        match self {
            Self::Api { message, .. } => message.clone(),
            Self::Db(sqlx::Error::RowNotFound) => "not found".to_owned(),
            Self::Db(_) | Self::Storage(_) | Self::Internal(_) => {
                "internal server error".to_owned()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_details_are_hidden() {
        let err = AppError::Storage("bucket xyz: AccessDenied for key minio".into());
        assert_eq!(err.code(), ErrorCode::Internal);
        assert_eq!(err.public_message(), "internal server error");
    }

    #[test]
    fn codes_serialize_like_as_str() {
        for code in [
            ErrorCode::OutsideCallHours,
            ErrorCode::ProviderNotConfigured,
        ] {
            let json = serde_json::to_string(&code).unwrap();
            assert_eq!(json, format!("\"{}\"", code.as_str()));
        }
    }
}
