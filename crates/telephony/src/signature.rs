//! `X-Plivo-Signature-V3` webhook validation.
//!
//! Mirrors `construct_post_url` in Plivo's official SDKs: the URL without
//! its query, then `?`, then the query parameters sorted as `k=v&k=v` (and
//! a `.` if there were any), then every form parameter sorted by name and
//! appended as `name` + `value` with no delimiter, then `.` + the
//! `X-Plivo-Signature-V3-Nonce` header. The signature is
//! base64(HMAC-SHA256(auth token, base string)). The header may hold
//! several comma-separated signatures (one per active token).

use base64::{engine::general_purpose::STANDARD, Engine as _};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// Header with the signature(s).
pub const SIGNATURE_HEADER: &str = "x-plivo-signature-v3";
/// Header with the nonce.
pub const NONCE_HEADER: &str = "x-plivo-signature-v3-nonce";

/// Computes the V3 signature for a POST request.
pub fn sign_post(auth_token: &str, url: &str, params: &[(String, String)], nonce: &str) -> String {
    let (base_url, query) = url.split_once('?').unwrap_or((url, ""));
    let mut query: Vec<(String, String)> = form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect();
    query.sort();
    let mut sorted: Vec<&(String, String)> = params.iter().collect();
    sorted.sort();

    let mut base = String::with_capacity(url.len() + 256);
    base.push_str(base_url);
    if !query.is_empty() || !params.is_empty() {
        base.push('?');
        let pairs: Vec<String> = query.iter().map(|(k, v)| format!("{k}={v}")).collect();
        base.push_str(&pairs.join("&"));
        if !query.is_empty() && !params.is_empty() {
            base.push('.');
        }
    }
    for (k, v) in sorted {
        base.push_str(k);
        base.push_str(v);
    }
    base.push('.');
    base.push_str(nonce);

    // HMAC accepts keys of any length; this cannot fail.
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(auth_token.as_bytes()) else {
        return String::new();
    };
    mac.update(base.as_bytes());
    STANDARD.encode(mac.finalize().into_bytes())
}

/// Validates a POST webhook. `signature_header` may contain several
/// comma-separated signatures; any match is accepted.
pub fn verify_post(
    auth_token: &str,
    url: &str,
    params: &[(String, String)],
    nonce: &str,
    signature_header: &str,
) -> bool {
    let expected = sign_post(auth_token, url, params, nonce);
    signature_header
        .split(',')
        .map(str::trim)
        .any(|candidate| bool::from(candidate.as_bytes().ct_eq(expected.as_bytes())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> Vec<(String, String)> {
        vec![
            ("To".into(), "+15555555555".into()),
            ("CallUUID".into(), "abc".into()),
            ("From".into(), "+15551111111".into()),
        ]
    }

    #[test]
    fn verifies_own_signature_regardless_of_param_order() {
        let url = "https://example.com/plivo/answer/1";
        let sig = sign_post("token", url, &params(), "n1");
        let mut reversed = params();
        reversed.reverse();
        assert!(verify_post("token", url, &reversed, "n1", &sig));
        assert!(verify_post(
            "token",
            url,
            &params(),
            "n1",
            &format!("bogus, {sig}")
        ));
    }

    #[test]
    fn rejects_tampering() {
        let url = "https://example.com/plivo/answer/1";
        let sig = sign_post("token", url, &params(), "n1");
        assert!(!verify_post("other", url, &params(), "n1", &sig));
        assert!(!verify_post("token", url, &params(), "n2", &sig));
        assert!(!verify_post(
            "token",
            "https://evil.com/x",
            &params(),
            "n1",
            &sig
        ));
        let mut changed = params();
        changed[0].1 = "+10000000000".into();
        assert!(!verify_post("token", url, &changed, "n1", &sig));
    }

    #[test]
    fn base_string_layout() {
        // Same base string as Plivo's SDK `construct_post_url`.
        let mac = |base: &[u8]| {
            let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(b"k") else {
                panic!("hmac");
            };
            mac.update(base);
            STANDARD.encode(mac.finalize().into_bytes())
        };
        assert_eq!(
            sign_post("k", "https://e.com/a", &params(), "n"),
            mac(b"https://e.com/a?CallUUIDabcFrom+15551111111To+15555555555.n")
        );
        assert_eq!(
            sign_post("k", "https://e.com/a?z=2&b=1", &params(), "n"),
            mac(b"https://e.com/a?b=1&z=2.CallUUIDabcFrom+15551111111To+15555555555.n")
        );
        assert_eq!(
            sign_post("k", "https://e.com/a", &[], "n"),
            mac(b"https://e.com/a.n")
        );
    }
}
