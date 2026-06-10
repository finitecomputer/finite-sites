//! Lowercase hex encoding, hand-rolled to avoid a dependency for 30 lines.

use crate::ProtoError;

const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";

pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX_CHARS[(byte >> 4) as usize] as char);
        out.push(HEX_CHARS[(byte & 0x0f) as usize] as char);
    }
    assert!(out.len() == bytes.len() * 2);
    out
}

pub fn decode(input: &str) -> Result<Vec<u8>, ProtoError> {
    if !input.len().is_multiple_of(2) {
        return Err(ProtoError::InvalidHex("odd length"));
    }
    let mut out = Vec::with_capacity(input.len() / 2);
    let raw = input.as_bytes();
    let mut index: usize = 0;
    // Bounded by input length, which callers bound at the wire boundary.
    while index < raw.len() {
        let high = decode_nibble(raw[index])?;
        let low = decode_nibble(raw[index + 1])?;
        out.push((high << 4) | low);
        index += 2;
    }
    assert!(out.len() == input.len() / 2);
    Ok(out)
}

/// Decode exactly 32 bytes of lowercase hex (ids, pubkeys, sha256 digests).
pub fn decode32(input: &str) -> Result<[u8; 32], ProtoError> {
    if input.len() != 64 {
        return Err(ProtoError::InvalidHex("expected 64 hex chars"));
    }
    let bytes = decode(input)?;
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// True when the string is exactly 64 lowercase hex chars.
pub fn is_hex32(input: &str) -> bool {
    input.len() == 64
        && input
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn decode_nibble(byte: u8) -> Result<u8, ProtoError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        // Uppercase hex is rejected on purpose: every id, pubkey, and digest
        // in the protocol is canonically lowercase, and accepting both forms
        // would create two spellings of the same key in the registry.
        _ => Err(ProtoError::InvalidHex("non-lowercase-hex character")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let bytes = [0u8, 1, 0xab, 0xff];
        let encoded = encode(&bytes);
        assert_eq!(encoded, "0001abff");
        assert_eq!(decode(&encoded).unwrap(), bytes);
    }

    #[test]
    fn rejects_uppercase() {
        assert_eq!(
            decode("AB"),
            Err(ProtoError::InvalidHex("non-lowercase-hex character"))
        );
    }

    #[test]
    fn rejects_odd_length() {
        assert_eq!(decode("abc"), Err(ProtoError::InvalidHex("odd length")));
    }

    #[test]
    fn decode32_wants_exactly_64_chars() {
        assert!(decode32(&"ab".repeat(32)).is_ok());
        assert!(decode32(&"ab".repeat(31)).is_err());
        assert!(is_hex32(&"ab".repeat(32)));
        assert!(!is_hex32(&"AB".repeat(32)));
    }
}
