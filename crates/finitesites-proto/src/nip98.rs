//! NIP-98 HTTP authorization: a kind-27235 nostr event, base64-encoded into
//! `Authorization: Nostr <event>`, binding the signer to one URL + method
//! (+ body hash) inside a small freshness window.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use sha2::{Digest, Sha256};

use crate::event::NostrEvent;
use crate::limits::{MAX_AUTH_HEADER_BYTES, NIP98_MAX_SKEW_SECONDS};
use crate::{ProtoError, hex};

pub const NIP98_KIND: u32 = 27235;
pub const AUTH_SCHEME: &str = "Nostr ";

/// Build the value for the `Authorization` header.
pub fn build_auth_header(
    secret_key: &[u8; 32],
    url: &str,
    method: &str,
    body: Option<&[u8]>,
    now_unix: u64,
) -> Result<String, ProtoError> {
    assert!(!url.is_empty() && !method.is_empty());
    let mut tags = vec![
        vec!["u".to_string(), url.to_string()],
        vec!["method".to_string(), method.to_string()],
    ];
    if let Some(body_bytes) = body {
        let digest = Sha256::digest(body_bytes);
        tags.push(vec!["payload".to_string(), hex::encode(&digest)]);
    }
    let event = NostrEvent::sign(secret_key, now_unix, NIP98_KIND, tags, String::new())?;
    let encoded = BASE64.encode(serde_json::to_vec(&event).expect("event always serializes"));
    let header = format!("{AUTH_SCHEME}{encoded}");
    assert!(header.len() > AUTH_SCHEME.len());
    Ok(header)
}

/// Verify an `Authorization` header against the request the server actually
/// received. Returns the authenticated pubkey hex.
pub fn verify_auth_header(
    header: &str,
    expected_url: &str,
    expected_method: &str,
    body: Option<&[u8]>,
    now_unix: u64,
) -> Result<String, ProtoError> {
    assert!(!expected_url.is_empty() && !expected_method.is_empty());
    if header.len() > MAX_AUTH_HEADER_BYTES as usize {
        return Err(ProtoError::InvalidAuthHeader("header too large"));
    }
    let encoded = header
        .strip_prefix(AUTH_SCHEME)
        .ok_or(ProtoError::InvalidAuthHeader("missing Nostr scheme"))?;
    let raw = BASE64
        .decode(encoded)
        .map_err(|_| ProtoError::InvalidAuthHeader("invalid base64"))?;
    let event: NostrEvent = serde_json::from_slice(&raw)
        .map_err(|_| ProtoError::InvalidAuthHeader("invalid event json"))?;

    if event.kind != NIP98_KIND {
        return Err(ProtoError::AuthRejected("wrong event kind"));
    }
    let oldest_acceptable = now_unix.saturating_sub(NIP98_MAX_SKEW_SECONDS);
    let newest_acceptable = now_unix.saturating_add(NIP98_MAX_SKEW_SECONDS);
    let created_at_is_fresh =
        event.created_at >= oldest_acceptable && event.created_at <= newest_acceptable;
    if !created_at_is_fresh {
        return Err(ProtoError::AuthRejected("event timestamp outside window"));
    }
    if event.tag_value("u") != Some(expected_url) {
        return Err(ProtoError::AuthRejected("url mismatch"));
    }
    if event.tag_value("method") != Some(expected_method) {
        return Err(ProtoError::AuthRejected("method mismatch"));
    }
    match (body, event.tag_value("payload")) {
        (Some(body_bytes), Some(claimed)) => {
            let digest = hex::encode(&Sha256::digest(body_bytes));
            if digest != claimed {
                return Err(ProtoError::AuthRejected("payload hash mismatch"));
            }
        }
        (Some(body_bytes), None) => {
            // Empty bodies may omit the payload tag; non-empty must bind it.
            if !body_bytes.is_empty() {
                return Err(ProtoError::AuthRejected("missing payload tag"));
            }
        }
        (None, Some(_)) => {
            return Err(ProtoError::AuthRejected("unexpected payload tag"));
        }
        (None, None) => {}
    }

    let pubkey = event.verify()?.to_string();
    assert!(hex::is_hex32(&pubkey));
    Ok(pubkey)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::pubkey_for_secret;

    const URL: &str = "http://127.0.0.1:8787/api/v1/sites/claim";
    const NOW: u64 = 1_750_000_000;

    fn secret(fill: u8) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        bytes[0] = fill;
        bytes[31] = 7;
        bytes
    }

    #[test]
    fn roundtrip_with_body() {
        let body = br#"{"name":"hello"}"#;
        let header = build_auth_header(&secret(1), URL, "POST", Some(body), NOW).unwrap();
        let pubkey = verify_auth_header(&header, URL, "POST", Some(body), NOW + 5).unwrap();
        assert_eq!(pubkey, pubkey_for_secret(&secret(1)).unwrap());
    }

    #[test]
    fn roundtrip_without_body() {
        let header = build_auth_header(&secret(1), URL, "GET", None, NOW).unwrap();
        assert!(verify_auth_header(&header, URL, "GET", None, NOW).is_ok());
    }

    #[test]
    fn rejects_url_mismatch() {
        let header = build_auth_header(&secret(1), URL, "GET", None, NOW).unwrap();
        assert_eq!(
            verify_auth_header(&header, "http://evil/", "GET", None, NOW),
            Err(ProtoError::AuthRejected("url mismatch"))
        );
    }

    #[test]
    fn rejects_method_mismatch() {
        let header = build_auth_header(&secret(1), URL, "GET", None, NOW).unwrap();
        assert_eq!(
            verify_auth_header(&header, URL, "POST", None, NOW),
            Err(ProtoError::AuthRejected("method mismatch"))
        );
    }

    #[test]
    fn rejects_stale_and_future_events() {
        let header = build_auth_header(&secret(1), URL, "GET", None, NOW).unwrap();
        let too_late = NOW + NIP98_MAX_SKEW_SECONDS + 1;
        let too_early = NOW - NIP98_MAX_SKEW_SECONDS - 1;
        assert_eq!(
            verify_auth_header(&header, URL, "GET", None, too_late),
            Err(ProtoError::AuthRejected("event timestamp outside window"))
        );
        assert_eq!(
            verify_auth_header(&header, URL, "GET", None, too_early),
            Err(ProtoError::AuthRejected("event timestamp outside window"))
        );
    }

    #[test]
    fn rejects_body_tampering() {
        let header = build_auth_header(&secret(1), URL, "POST", Some(b"original"), NOW).unwrap();
        assert_eq!(
            verify_auth_header(&header, URL, "POST", Some(b"tampered"), NOW),
            Err(ProtoError::AuthRejected("payload hash mismatch"))
        );
    }

    #[test]
    fn rejects_missing_payload_tag_for_nonempty_body() {
        let header = build_auth_header(&secret(1), URL, "POST", None, NOW).unwrap();
        assert_eq!(
            verify_auth_header(&header, URL, "POST", Some(b"body"), NOW),
            Err(ProtoError::AuthRejected("missing payload tag"))
        );
    }

    #[test]
    fn rejects_wrong_kind() {
        let event = NostrEvent::sign(
            &secret(1),
            NOW,
            1,
            vec![
                vec!["u".into(), URL.into()],
                vec!["method".into(), "GET".into()],
            ],
            String::new(),
        )
        .unwrap();
        let header = format!(
            "{AUTH_SCHEME}{}",
            BASE64.encode(serde_json::to_vec(&event).unwrap())
        );
        assert_eq!(
            verify_auth_header(&header, URL, "GET", None, NOW),
            Err(ProtoError::AuthRejected("wrong event kind"))
        );
    }

    #[test]
    fn rejects_garbage_header() {
        assert!(verify_auth_header("Bearer xyz", URL, "GET", None, NOW).is_err());
        assert!(verify_auth_header("Nostr not-base64!!!", URL, "GET", None, NOW).is_err());
    }
}
