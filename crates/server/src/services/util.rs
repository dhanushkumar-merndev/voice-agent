//! Small helpers: call hours, stream tokens, hashing, validation.

use std::sync::LazyLock;

use chrono::{Timelike, Utc};
use common::{config::CallHoursConfig, ids::CallId, AppError, ErrorCode};
use hmac::{Hmac, Mac};
use regex::Regex;
use sha2::{Digest, Sha256};

/// Rejects calls outside the configured window (local time of `CALL_HOURS_TZ`).
pub fn check_call_hours(cfg: &CallHoursConfig) -> Result<(), AppError> {
    if !cfg.enforced {
        return Ok(());
    }
    let hour = Utc::now().with_timezone(&cfg.timezone).hour();
    if hour >= cfg.start_hour && hour < cfg.end_hour {
        Ok(())
    } else {
        Err(AppError::new(
            ErrorCode::OutsideCallHours,
            format!(
                "calls are allowed only between {:02}:00 and {:02}:00 {}",
                cfg.start_hour,
                cfg.end_hour,
                cfg.timezone.name()
            ),
        ))
    }
}

/// Per-call token required on the media WebSocket URL: the first 16
/// bytes of HMAC-SHA256(secret, call_id), hex. Derived, never stored.
pub fn stream_token(secret: &str, call: CallId) -> String {
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return String::new();
    };
    mac.update(call.to_string().as_bytes());
    hex::encode(&mac.finalize().into_bytes()[..16])
}

/// Hex SHA-256.
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

static E164: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"^\+[1-9]\d{7,14}$").ok());

/// `true` for E.164 numbers such as `+919876543210`.
pub fn is_e164(number: &str) -> bool {
    E164.as_ref().is_some_and(|re| re.is_match(number))
}

/// `true` for `auto` or a BCP-47 code like `hi-IN`.
pub fn is_language(code: &str) -> bool {
    if code == "auto" {
        return true;
    }
    let mut parts = code.split('-');
    let lang = parts.next().unwrap_or_default();
    let region = parts.next().unwrap_or_default();
    (2..=3).contains(&lang.len())
        && lang.chars().all(|c| c.is_ascii_lowercase())
        && region.len() == 2
        && region.chars().all(|c| c.is_ascii_uppercase())
        && parts.next().is_none()
}

/// Percentile (nearest rank) of a sample; `None` when empty.
pub fn percentile(values: &[i64], p: f64) -> Option<i64> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_unstable();
    let rank = ((p / 100.0) * v.len() as f64).ceil() as usize;
    v.get(rank.saturating_sub(1).min(v.len() - 1)).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validators() {
        assert!(is_e164("+919876543210"));
        assert!(!is_e164("9876543210"));
        assert!(!is_e164("+91 98765"));
        assert!(is_language("auto") && is_language("ta-IN"));
        assert!(!is_language("tamil"));
    }

    #[test]
    fn tokens_are_stable_and_distinct() {
        let a = CallId::new();
        let b = CallId::new();
        assert_eq!(stream_token("s", a), stream_token("s", a));
        assert_ne!(stream_token("s", a), stream_token("s", b));
        assert_ne!(stream_token("s", a), stream_token("t", a));
        assert_eq!(stream_token("s", a).len(), 32);
    }

    #[test]
    fn percentiles() {
        let v = [100, 200, 300, 400, 500, 600, 700, 800, 900, 1000];
        assert_eq!(percentile(&v, 50.0), Some(500));
        assert_eq!(percentile(&v, 95.0), Some(1000));
        assert_eq!(percentile(&[], 50.0), None);
    }

    #[test]
    fn call_hours_disabled() {
        let cfg = CallHoursConfig {
            enforced: false,
            start_hour: 9,
            end_hour: 9,
            timezone: chrono_tz::Asia::Kolkata,
        };
        assert!(check_call_hours(&cfg).is_ok());
        let all_day = CallHoursConfig {
            enforced: true,
            start_hour: 0,
            end_hour: 24,
            ..cfg
        };
        assert!(check_call_hours(&all_day).is_ok());
    }
}
