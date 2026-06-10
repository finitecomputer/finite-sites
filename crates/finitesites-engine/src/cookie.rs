//! Viewer cookies: an HMAC-signed `(site_id, email, expires_at)` triple.
//!
//! The cookie is scoped to one site id, so a cookie for `a.finite.chat`
//! says nothing about `b.finite.chat` even though both are signed with the
//! same server secret. Share-table membership is re-checked at view time;
//! the cookie only proves "this email completed a magic link for this site".

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64URL;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Payload fields are joined with `\n`; emails and site ids cannot contain
/// newlines (validated at the boundary), so the encoding is unambiguous.
const FIELD_SEPARATOR: char = '\n';

#[derive(Debug, PartialEq, Eq)]
pub struct ViewerCookie {
    pub site_id: String,
    pub email: String,
    pub expires_at: u64,
}

impl ViewerCookie {
    pub fn sign(&self, secret: &[u8; 32]) -> String {
        assert!(!self.site_id.contains(FIELD_SEPARATOR));
        assert!(!self.email.contains(FIELD_SEPARATOR));
        let payload = format!(
            "{}{FIELD_SEPARATOR}{}{FIELD_SEPARATOR}{}",
            self.site_id, self.email, self.expires_at
        );
        let mut mac = HmacSha256::new_from_slice(secret).expect("hmac accepts 32-byte keys");
        mac.update(payload.as_bytes());
        let tag = mac.finalize().into_bytes();
        format!(
            "{}.{}",
            BASE64URL.encode(payload.as_bytes()),
            BASE64URL.encode(tag)
        )
    }

    /// Verify a cookie for a specific site at a specific time.
    pub fn verify(
        secret: &[u8; 32],
        value: &str,
        expected_site_id: &str,
        now: u64,
    ) -> Option<ViewerCookie> {
        // Cookies are small; reject anything oversized before decoding.
        if value.len() > 1024 {
            return None;
        }
        let (payload_b64, tag_b64) = value.split_once('.')?;
        let payload = BASE64URL.decode(payload_b64).ok()?;
        let claimed_tag = BASE64URL.decode(tag_b64).ok()?;

        let mut mac = HmacSha256::new_from_slice(secret).expect("hmac accepts 32-byte keys");
        mac.update(&payload);
        // Constant-time comparison via the hmac crate.
        if mac.verify_slice(&claimed_tag).is_err() {
            return None;
        }

        let payload_text = String::from_utf8(payload).ok()?;
        let mut parts = payload_text.split(FIELD_SEPARATOR);
        let site_id = parts.next()?;
        let email = parts.next()?;
        let expires_at: u64 = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        if site_id != expected_site_id {
            return None;
        }
        if now > expires_at {
            return None;
        }
        Some(ViewerCookie {
            site_id: site_id.to_string(),
            email: email.to_string(),
            expires_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: [u8; 32] = [7u8; 32];
    const OTHER_SECRET: [u8; 32] = [8u8; 32];

    fn cookie() -> ViewerCookie {
        ViewerCookie {
            site_id: "site_abc".into(),
            email: "a@example.com".into(),
            expires_at: 2_000,
        }
    }

    #[test]
    fn sign_verify_roundtrip() {
        let value = cookie().sign(&SECRET);
        let verified = ViewerCookie::verify(&SECRET, &value, "site_abc", 1_000).unwrap();
        assert_eq!(verified, cookie());
    }

    #[test]
    fn rejects_wrong_site_wrong_secret_expired_and_tampered() {
        let value = cookie().sign(&SECRET);
        assert!(ViewerCookie::verify(&SECRET, &value, "site_other", 1_000).is_none());
        assert!(ViewerCookie::verify(&OTHER_SECRET, &value, "site_abc", 1_000).is_none());
        assert!(ViewerCookie::verify(&SECRET, &value, "site_abc", 2_001).is_none());

        let tampered = format!("x{value}");
        assert!(ViewerCookie::verify(&SECRET, &tampered, "site_abc", 1_000).is_none());
        assert!(ViewerCookie::verify(&SECRET, "garbage", "site_abc", 1_000).is_none());
    }
}
