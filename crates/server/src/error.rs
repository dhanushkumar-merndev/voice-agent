//! HTTP rendering of [`AppError`].

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use common::AppError;
use serde_json::json;

/// Handler error: renders as `{"error": {"code", "message"}}` with the
/// code's HTTP status. Server-side failures are logged with full details;
/// clients only see the public message.
#[derive(Debug)]
pub struct ApiError(pub AppError);

impl<E: Into<AppError>> From<E> for ApiError {
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let code = self.0.code();
        let status =
            StatusCode::from_u16(code.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        if status.is_server_error() {
            tracing::error!(error = %self.0, code = code.as_str(), "request failed");
        }
        let body = json!({ "error": { "code": code, "message": self.0.public_message() } });
        (status, Json(body)).into_response()
    }
}

/// Result type for handlers.
pub type ApiResult<T> = Result<T, ApiError>;
