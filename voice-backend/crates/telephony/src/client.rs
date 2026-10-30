//! Plivo Voice REST API client.
//!
//! All requests use HTTP basic auth (auth ID + auth token) against
//! `https://api.plivo.com/v1/Account/{auth_id}/...`. The shared `reqwest`
//! client keeps TLS connections to Plivo warm.

use bytes::Bytes;
use serde::{Deserialize, Serialize};

/// Plivo API error.
#[derive(Debug, thiserror::Error)]
pub enum PlivoError {
    /// Network or TLS failure.
    #[error("plivo request failed: {0}")]
    Http(#[from] reqwest::Error),
    /// Plivo answered with a non-success status.
    #[error("plivo returned {status}: {body}")]
    Status {
        /// HTTP status code.
        status: u16,
        /// Response body (truncated).
        body: String,
    },
}

/// Parameters for an outbound call.
#[derive(Debug, Clone, Serialize)]
pub struct MakeCall<'a> {
    /// Caller ID (E.164).
    pub from: &'a str,
    /// Destination (E.164).
    pub to: &'a str,
    /// Called on answer; must return XML.
    pub answer_url: &'a str,
    /// Always `POST`.
    pub answer_method: &'static str,
    /// Called when the destination starts ringing.
    pub ring_url: &'a str,
    /// Always `POST`.
    pub ring_method: &'static str,
    /// Called when the call ends.
    pub hangup_url: &'a str,
    /// Always `POST`.
    pub hangup_method: &'static str,
    /// Hard cap on call duration in seconds.
    pub time_limit: u32,
    /// Seconds to ring before giving up.
    pub ring_timeout: u32,
}

/// Response to a successful call request.
#[derive(Debug, Clone, Deserialize)]
pub struct MakeCallResponse {
    /// Identifier of the call request (the CallUUID arrives in webhooks).
    pub request_uuid: String,
}

/// Response to a successful record request.
#[derive(Debug, Clone, Deserialize)]
pub struct RecordResponse {
    /// Recording ID.
    pub recording_id: Option<String>,
    /// Where the file will be available.
    pub url: Option<String>,
}

/// Billing fields of a finished call's detail record
/// (`GET /Call/{call_uuid}/`). Amounts are in the account currency.
#[derive(Debug, Clone, Deserialize)]
pub struct CallDetail {
    /// Seconds billed (after the billing interval is applied).
    pub billed_duration: Option<i64>,
    /// Total charge for the call, e.g. `"0.85000"`.
    pub total_amount: Option<String>,
    /// Per-minute rate for the destination.
    pub total_rate: Option<String>,
}

/// One recording from `GET /Recording/`.
#[derive(Debug, Clone, Deserialize)]
pub struct Recording {
    /// Recording ID.
    pub recording_id: String,
    /// Where the file can be downloaded.
    pub recording_url: String,
}

#[derive(Deserialize)]
struct RecordingList {
    #[serde(default)]
    objects: Vec<Recording>,
}

#[derive(Serialize)]
struct RecordParams<'a> {
    time_limit: u32,
    file_format: &'static str,
    callback_url: &'a str,
    callback_method: &'static str,
}

/// Plivo REST client. Cheap to clone.
#[derive(Debug, Clone)]
pub struct PlivoClient {
    http: reqwest::Client,
    auth_id: String,
    auth_token: String,
    base: String,
}

impl PlivoClient {
    /// Creates a client for `auth_id`. `api_base` is normally `https://api.plivo.com`.
    pub fn new(http: reqwest::Client, auth_id: &str, auth_token: &str, api_base: &str) -> Self {
        Self {
            http,
            auth_id: auth_id.to_owned(),
            auth_token: auth_token.to_owned(),
            base: format!("{}/v1/Account/{auth_id}", api_base.trim_end_matches('/')),
        }
    }

    /// Places an outbound call.
    pub async fn make_call(&self, params: &MakeCall<'_>) -> Result<MakeCallResponse, PlivoError> {
        let resp = self
            .http
            .post(format!("{}/Call/", self.base))
            .basic_auth(&self.auth_id, Some(&self.auth_token))
            .json(params)
            .send()
            .await?;
        Ok(check(resp).await?.json().await?)
    }

    /// Starts recording an answered call as MP3. Plivo's default
    /// `time_limit` is only 60 s, so the caller passes the call's max duration.
    pub async fn start_recording(
        &self,
        call_uuid: &str,
        time_limit: u32,
        callback_url: &str,
    ) -> Result<RecordResponse, PlivoError> {
        let resp = self
            .http
            .post(format!("{}/Call/{call_uuid}/Record/", self.base))
            .basic_auth(&self.auth_id, Some(&self.auth_token))
            .json(&RecordParams {
                time_limit,
                file_format: "mp3",
                callback_url,
                callback_method: "POST",
            })
            .send()
            .await?;
        Ok(check(resp).await?.json().await?)
    }

    /// Fetches the detail record of a finished call, including its cost.
    /// Plivo publishes it a few seconds after hangup; `Ok(None)` until then.
    pub async fn call_detail(&self, call_uuid: &str) -> Result<Option<CallDetail>, PlivoError> {
        let resp = self
            .http
            .get(format!("{}/Call/{call_uuid}/", self.base))
            .basic_auth(&self.auth_id, Some(&self.auth_token))
            .send()
            .await?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        Ok(Some(check(resp).await?.json().await?))
    }

    /// Lists the recordings Plivo holds for a call.
    pub async fn recordings_for_call(&self, call_uuid: &str) -> Result<Vec<Recording>, PlivoError> {
        let resp = self
            .http
            .get(format!("{}/Recording/", self.base))
            .query(&[("call_uuid", call_uuid)])
            .basic_auth(&self.auth_id, Some(&self.auth_token))
            .send()
            .await?;
        let list: RecordingList = check(resp).await?.json().await?;
        Ok(list.objects)
    }

    /// Hangs up an ongoing call (or cancels a queued one).
    pub async fn hangup(&self, call_uuid: &str) -> Result<(), PlivoError> {
        let resp = self
            .http
            .delete(format!("{}/Call/{call_uuid}/", self.base))
            .basic_auth(&self.auth_id, Some(&self.auth_token))
            .send()
            .await?;
        match resp.status().as_u16() {
            // 404: the call already ended.
            404 => Ok(()),
            _ => check(resp).await.map(|_| ()),
        }
    }

    /// Cancels a placed call that has not connected yet, by the
    /// `request_uuid` from [`Self::make_call`] (Plivo SDK `calls.cancel`).
    pub async fn cancel_request(&self, request_uuid: &str) -> Result<(), PlivoError> {
        let resp = self
            .http
            .delete(format!("{}/Request/{request_uuid}/", self.base))
            .basic_auth(&self.auth_id, Some(&self.auth_token))
            .send()
            .await?;
        match resp.status().as_u16() {
            // 404: it already connected or ended.
            404 => Ok(()),
            _ => check(resp).await.map(|_| ()),
        }
    }

    /// Downloads a recording file. Basic auth is sent in case recording
    /// URL authentication is enabled on the account.
    pub async fn download(&self, url: &str) -> Result<Bytes, PlivoError> {
        let resp = self
            .http
            .get(url)
            .basic_auth(&self.auth_id, Some(&self.auth_token))
            .send()
            .await?;
        Ok(check(resp).await?.bytes().await?)
    }

    /// Permanently deletes a recording from Plivo.
    pub async fn delete_recording(&self, recording_id: &str) -> Result<(), PlivoError> {
        let resp = self
            .http
            .delete(format!("{}/Recording/{recording_id}/", self.base))
            .basic_auth(&self.auth_id, Some(&self.auth_token))
            .send()
            .await?;
        match resp.status().as_u16() {
            404 => Ok(()),
            _ => check(resp).await.map(|_| ()),
        }
    }
}

async fn check(resp: reqwest::Response) -> Result<reqwest::Response, PlivoError> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status().as_u16();
    let mut body = resp.text().await.unwrap_or_default();
    body.truncate(500);
    Err(PlivoError::Status { status, body })
}
