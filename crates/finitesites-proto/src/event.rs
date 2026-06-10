//! Minimal NIP-01 event encoding, signing, and verification.
//!
//! We implement the small slice of nostr we need (kind 27235 auth events)
//! directly on `secp256k1` + `sha2` instead of pulling a full nostr SDK.

use secp256k1::schnorr::Signature;
use secp256k1::{Keypair, Message, Secp256k1, XOnlyPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ProtoError, hex};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NostrEvent {
    pub id: String,
    pub pubkey: String,
    pub created_at: u64,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: String,
}

impl NostrEvent {
    /// Compute the canonical NIP-01 event id for these fields.
    pub fn compute_id(
        pubkey: &str,
        created_at: u64,
        kind: u32,
        tags: &[Vec<String>],
        content: &str,
    ) -> Result<String, ProtoError> {
        if !hex::is_hex32(pubkey) {
            return Err(ProtoError::InvalidEvent("pubkey must be 32-byte hex"));
        }
        // NIP-01: sha256 of `[0, pubkey, created_at, kind, tags, content]`
        // serialized as compact JSON. serde_json's escaping matches the
        // NIP-01 required escapes for the values we produce (empty or ASCII
        // content, URL/method tags).
        let canonical = serde_json::json!([0, pubkey, created_at, kind, tags, content]);
        let serialized =
            serde_json::to_string(&canonical).expect("canonical event array always serializes");
        let digest = Sha256::digest(serialized.as_bytes());
        let id = hex::encode(&digest);
        assert!(hex::is_hex32(&id));
        Ok(id)
    }

    /// Build and sign an event with the given secret key.
    pub fn sign(
        secret_key: &[u8; 32],
        created_at: u64,
        kind: u32,
        tags: Vec<Vec<String>>,
        content: String,
    ) -> Result<NostrEvent, ProtoError> {
        let secp = Secp256k1::new();
        let keypair = Keypair::from_seckey_slice(&secp, secret_key)
            .map_err(|_| ProtoError::InvalidEvent("invalid secret key"))?;
        let (xonly, _parity) = keypair.x_only_public_key();
        let pubkey = hex::encode(&xonly.serialize());

        let id = Self::compute_id(&pubkey, created_at, kind, &tags, &content)?;
        let digest = hex::decode32(&id).expect("computed id is always 32-byte hex");
        let message = Message::from_digest(digest);
        let sig = secp.sign_schnorr_no_aux_rand(&message, &keypair);

        let event = NostrEvent {
            id,
            pubkey,
            created_at,
            kind,
            tags,
            content,
            sig: hex::encode(sig.as_ref()),
        };
        debug_assert!(event.verify().is_ok());
        Ok(event)
    }

    /// Verify the event id matches its fields and the signature matches the
    /// id and pubkey. Returns the pubkey hex on success.
    pub fn verify(&self) -> Result<&str, ProtoError> {
        let expected_id = Self::compute_id(
            &self.pubkey,
            self.created_at,
            self.kind,
            &self.tags,
            &self.content,
        )?;
        if expected_id != self.id {
            return Err(ProtoError::InvalidEvent("id does not match fields"));
        }

        let pubkey_bytes = hex::decode32(&self.pubkey)?;
        let xonly = XOnlyPublicKey::from_slice(&pubkey_bytes)
            .map_err(|_| ProtoError::InvalidEvent("pubkey is not a valid x-only point"))?;
        let sig_bytes = hex::decode(&self.sig)?;
        let sig = Signature::from_slice(&sig_bytes).map_err(|_| ProtoError::InvalidSignature)?;
        let digest = hex::decode32(&self.id)?;
        let message = Message::from_digest(digest);

        let secp = Secp256k1::verification_only();
        if secp.verify_schnorr(&sig, &message, &xonly).is_ok() {
            Ok(&self.pubkey)
        } else {
            Err(ProtoError::InvalidSignature)
        }
    }

    /// First value of the first tag with this name, if present.
    pub fn tag_value(&self, name: &str) -> Option<&str> {
        // Bounded: tag counts are bounded by MAX_AUTH_HEADER_BYTES at decode.
        for tag in &self.tags {
            if tag.len() >= 2 && tag[0] == name {
                return Some(&tag[1]);
            }
        }
        None
    }
}

/// Derive the x-only public key hex for a secret key.
pub fn pubkey_for_secret(secret_key: &[u8; 32]) -> Result<String, ProtoError> {
    let secp = Secp256k1::new();
    let keypair = Keypair::from_seckey_slice(&secp, secret_key)
        .map_err(|_| ProtoError::InvalidEvent("invalid secret key"))?;
    let (xonly, _parity) = keypair.x_only_public_key();
    Ok(hex::encode(&xonly.serialize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_secret() -> [u8; 32] {
        let mut secret = [0u8; 32];
        secret[31] = 1;
        secret
    }

    #[test]
    fn sign_then_verify_roundtrips() {
        let event = NostrEvent::sign(
            &test_secret(),
            1_700_000_000,
            27235,
            vec![vec!["u".into(), "http://example.com/".into()]],
            String::new(),
        )
        .unwrap();
        assert_eq!(event.kind, 27235);
        assert_eq!(event.verify().unwrap(), event.pubkey);
    }

    #[test]
    fn tampered_content_fails_id_check() {
        let mut event = NostrEvent::sign(&test_secret(), 1, 1, vec![], "hello".into()).unwrap();
        event.content = "evil".into();
        assert_eq!(
            event.verify(),
            Err(ProtoError::InvalidEvent("id does not match fields"))
        );
    }

    #[test]
    fn tampered_sig_fails_signature_check() {
        let mut event = NostrEvent::sign(&test_secret(), 1, 1, vec![], "hello".into()).unwrap();
        // Replace with a structurally valid but wrong signature.
        let other = NostrEvent::sign(&test_secret(), 2, 1, vec![], "other".into()).unwrap();
        event.sig = other.sig;
        assert_eq!(event.verify(), Err(ProtoError::InvalidSignature));
    }

    #[test]
    fn tag_value_returns_first_match() {
        let event = NostrEvent::sign(
            &test_secret(),
            1,
            27235,
            vec![
                vec!["u".into(), "http://a/".into()],
                vec!["u".into(), "http://b/".into()],
                vec!["method".into(), "GET".into()],
            ],
            String::new(),
        )
        .unwrap();
        assert_eq!(event.tag_value("u"), Some("http://a/"));
        assert_eq!(event.tag_value("method"), Some("GET"));
        assert_eq!(event.tag_value("payload"), None);
    }

    #[test]
    fn pubkey_for_secret_matches_signed_event() {
        let event = NostrEvent::sign(&test_secret(), 1, 1, vec![], String::new()).unwrap();
        assert_eq!(pubkey_for_secret(&test_secret()).unwrap(), event.pubkey);
    }
}
