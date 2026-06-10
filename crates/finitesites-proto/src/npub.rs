//! npub (NIP-19) encoding for display and operator input. The registry
//! stores pubkeys as 32-byte lowercase hex; npub is a human-facing skin.

use bech32::{Bech32, Hrp};

use crate::{ProtoError, hex};

const NPUB_HRP: &str = "npub";

pub fn encode_npub(pubkey_hex: &str) -> Result<String, ProtoError> {
    let bytes = hex::decode32(pubkey_hex)?;
    let hrp = Hrp::parse(NPUB_HRP).expect("static hrp is valid");
    let encoded = bech32::encode::<Bech32>(hrp, &bytes)
        .map_err(|_| ProtoError::InvalidNpub("encode failed"))?;
    assert!(encoded.starts_with("npub1"));
    Ok(encoded)
}

pub fn decode_npub(npub: &str) -> Result<String, ProtoError> {
    let (hrp, bytes) =
        bech32::decode(npub).map_err(|_| ProtoError::InvalidNpub("invalid bech32"))?;
    if hrp.as_str() != NPUB_HRP {
        return Err(ProtoError::InvalidNpub("wrong prefix"));
    }
    if bytes.len() != 32 {
        return Err(ProtoError::InvalidNpub("wrong payload length"));
    }
    Ok(hex::encode(&bytes))
}

/// Accept either form for operator-facing input.
pub fn pubkey_from_hex_or_npub(input: &str) -> Result<String, ProtoError> {
    if hex::is_hex32(input) {
        Ok(input.to_string())
    } else {
        decode_npub(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // NIP-19 test vector: fiatjaf's pubkey.
    const HEX: &str = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
    const NPUB: &str = "npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6";

    #[test]
    fn matches_nip19_test_vector() {
        assert_eq!(encode_npub(HEX).unwrap(), NPUB);
        assert_eq!(decode_npub(NPUB).unwrap(), HEX);
    }

    #[test]
    fn accepts_either_form() {
        assert_eq!(pubkey_from_hex_or_npub(HEX).unwrap(), HEX);
        assert_eq!(pubkey_from_hex_or_npub(NPUB).unwrap(), HEX);
        assert!(pubkey_from_hex_or_npub("nsec1...").is_err());
        assert!(pubkey_from_hex_or_npub("zz").is_err());
    }
}
