//! Site name rules: a site name is one lowercase DNS label that becomes
//! `{name}.{base_domain}`. First-come, globally unique, some names reserved.

use crate::ProtoError;

pub const MIN_NAME_LENGTH: usize = 3;
pub const MAX_NAME_LENGTH: usize = 63;

/// Names that would collide with platform surfaces or invite impersonation.
/// Keep sorted; `validate_site_name` binary-searches it.
const RESERVED_NAMES: &[&str] = &[
    "about",
    "abuse",
    "admin",
    "api",
    "app",
    "auth",
    "blog",
    "captions",
    "dashboard",
    "dev",
    "dns",
    "docs",
    "finite",
    "ftp",
    "git",
    "help",
    "imap",
    "login",
    "mail",
    "mx",
    "ns1",
    "ns2",
    "official",
    "pop",
    "root",
    "security",
    "site",
    "sites",
    "smtp",
    "ssl",
    "staging",
    "status",
    "support",
    "test",
    "vip",
    "web",
    "www",
];

pub fn validate_site_name(name: &str) -> Result<(), ProtoError> {
    if name.len() < MIN_NAME_LENGTH {
        return Err(ProtoError::InvalidSiteName("shorter than 3 characters"));
    }
    if name.len() > MAX_NAME_LENGTH {
        return Err(ProtoError::InvalidSiteName("longer than 63 characters"));
    }
    let bytes = name.as_bytes();
    let all_chars_valid = bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-');
    if !all_chars_valid {
        return Err(ProtoError::InvalidSiteName(
            "only lowercase letters, digits, and hyphens are allowed",
        ));
    }
    if bytes[0] == b'-' || bytes[bytes.len() - 1] == b'-' {
        return Err(ProtoError::InvalidSiteName(
            "may not start or end with a hyphen",
        ));
    }
    if RESERVED_NAMES.binary_search(&name).is_ok() {
        return Err(ProtoError::InvalidSiteName("name is reserved"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_list_is_sorted_for_binary_search() {
        let mut sorted = RESERVED_NAMES.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, RESERVED_NAMES);
    }

    #[test]
    fn accepts_normal_names() {
        for name in ["hello", "my-site", "abc", "a1b2c3", "x".repeat(63).as_str()] {
            assert_eq!(validate_site_name(name), Ok(()), "{name}");
        }
    }

    #[test]
    fn rejects_bad_names() {
        for name in [
            "ab",
            "",
            "Hello",
            "under_score",
            "-lead",
            "trail-",
            "dot.name",
            "x".repeat(64).as_str(),
        ] {
            assert!(validate_site_name(name).is_err(), "{name}");
        }
    }

    #[test]
    fn rejects_reserved_names() {
        for name in ["api", "www", "admin", "finite"] {
            assert_eq!(
                validate_site_name(name),
                Err(ProtoError::InvalidSiteName("name is reserved"))
            );
        }
    }
}
