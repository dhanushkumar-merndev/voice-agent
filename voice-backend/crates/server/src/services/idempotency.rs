//! `Idempotency-Key` support for create endpoints.
//!
//! The first request with a key inserts a placeholder row; when it
//! succeeds the response is stored and later requests with the same key
//! (and the same body) get the stored response. A different body, or a
//! request while the first is still running, gets `409 conflict`. Failed
//! requests remove their row so the client can retry.

use axum::http::{HeaderMap, StatusCode};
use common::{ids::TenantId, AppError, ErrorCode};
use serde_json::Value;
use sqlx::PgPool;

/// Header name.
pub const HEADER: &str = "idempotency-key";

/// Outcome of [`begin`].
#[derive(Debug)]
pub enum Begin {
    /// Proceed; call [`finish`] or [`abandon`] afterwards.
    Fresh(Option<Key>),
    /// Return this stored response.
    Replay(StatusCode, Value),
}

/// A claimed key.
#[derive(Debug, Clone)]
pub struct Key {
    tenant: TenantId,
    endpoint: &'static str,
    key: String,
}

/// Reads the key header, if present and valid.
pub fn header(headers: &HeaderMap) -> Result<Option<String>, AppError> {
    match headers.get(HEADER) {
        None => Ok(None),
        Some(v) => {
            let k = v
                .to_str()
                .map_err(|_| AppError::bad_request("Idempotency-Key must be ASCII"))?
                .trim();
            if k.is_empty() || k.len() > 255 {
                return Err(AppError::bad_request(
                    "Idempotency-Key must be 1-255 characters",
                ));
            }
            Ok(Some(k.to_owned()))
        }
    }
}

/// Claims `key` for this request (identified by `request_hash`).
pub async fn begin(
    db: &PgPool,
    tenant: TenantId,
    endpoint: &'static str,
    key: Option<String>,
    request_hash: &str,
) -> Result<Begin, AppError> {
    let Some(key) = key else {
        return Ok(Begin::Fresh(None));
    };
    let inserted = sqlx::query(
        "INSERT INTO idempotency_keys (tenant_id, endpoint, key, request_hash) VALUES ($1, $2, $3, $4) \
         ON CONFLICT DO NOTHING",
    )
    .bind(tenant.0)
    .bind(endpoint)
    .bind(&key)
    .bind(request_hash)
    .execute(db)
    .await?
    .rows_affected();
    if inserted == 1 {
        return Ok(Begin::Fresh(Some(Key {
            tenant,
            endpoint,
            key,
        })));
    }
    let (hash, status, response): (String, Option<i16>, Option<Value>) = sqlx::query_as(
        "SELECT request_hash, status_code, response FROM idempotency_keys \
         WHERE tenant_id = $1 AND endpoint = $2 AND key = $3",
    )
    .bind(tenant.0)
    .bind(endpoint)
    .bind(&key)
    .fetch_one(db)
    .await?;
    if hash != request_hash {
        return Err(AppError::new(
            ErrorCode::Conflict,
            "Idempotency-Key was already used with a different request",
        ));
    }
    match (status, response) {
        (Some(code), Some(body)) => Ok(Begin::Replay(
            StatusCode::from_u16(code as u16).unwrap_or(StatusCode::OK),
            body,
        )),
        _ => Err(AppError::new(
            ErrorCode::Conflict,
            "a request with this Idempotency-Key is still in progress",
        )),
    }
}

/// Stores the response for a claimed key.
pub async fn finish(db: &PgPool, key: Option<Key>, status: StatusCode, body: &Value) {
    let Some(k) = key else { return };
    let r = sqlx::query(
        "UPDATE idempotency_keys SET status_code = $4, response = $5 \
         WHERE tenant_id = $1 AND endpoint = $2 AND key = $3",
    )
    .bind(k.tenant.0)
    .bind(k.endpoint)
    .bind(&k.key)
    .bind(status.as_u16() as i16)
    .bind(body)
    .execute(db)
    .await;
    if let Err(e) = r {
        tracing::warn!(error = %e, "could not store idempotent response");
    }
}

/// Releases a claimed key after a failure.
pub async fn abandon(db: &PgPool, key: Option<Key>) {
    let Some(k) = key else { return };
    let _ = sqlx::query(
        "DELETE FROM idempotency_keys WHERE tenant_id = $1 AND endpoint = $2 AND key = $3",
    )
    .bind(k.tenant.0)
    .bind(k.endpoint)
    .bind(&k.key)
    .execute(db)
    .await;
}
