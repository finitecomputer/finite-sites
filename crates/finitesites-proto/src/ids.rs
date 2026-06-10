//! Registry id generation.
//!
//! Ids are `{prefix}_{16 random bytes as hex}`. The prefix makes ids
//! self-describing in logs and foreign keys.

use crate::hex;

pub const SITE_ID_PREFIX: &str = "site";
pub const VERSION_ID_PREFIX: &str = "ver";
pub const PUBLISH_ID_PREFIX: &str = "pub";
pub const CLAIM_ID_PREFIX: &str = "claim";

pub fn new_id(prefix: &str) -> String {
    assert!(!prefix.is_empty() && prefix.len() <= 8);
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("operating system randomness must be available");
    let id = format!("{prefix}_{}", hex::encode(&bytes));
    assert!(id.len() == prefix.len() + 1 + 32);
    id
}

/// 32 bytes of OS randomness for secrets (keys, tokens, cookie secrets).
pub fn random_32() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("operating system randomness must be available");
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_have_prefix_and_are_unique() {
        let a = new_id(SITE_ID_PREFIX);
        let b = new_id(SITE_ID_PREFIX);
        assert!(a.starts_with("site_"));
        assert_ne!(a, b);
    }
}
