//! Email validation for the sharing ACL. Conservative on purpose: the goal
//! is a clean ACL table and unambiguous cookie payloads, not RFC 5322.

use finitesites_proto::limits::MAX_EMAIL_BYTES;

use crate::EngineError;

/// Normalize (trim + lowercase) and validate an email address.
pub fn validate_email(raw: &str) -> Result<String, EngineError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_EMAIL_BYTES as usize {
        return Err(EngineError::Validation("email empty or too long"));
    }
    let chars_are_safe = trimmed
        .bytes()
        .all(|b| b.is_ascii_graphic() && b != b'\\' && b != b'"' && b != b',');
    if !chars_are_safe {
        return Err(EngineError::Validation("email contains unsafe character"));
    }
    let Some((local, domain)) = trimmed.split_once('@') else {
        return Err(EngineError::Validation("email must contain one @"));
    };
    if local.is_empty() || domain.is_empty() || domain.contains('@') {
        return Err(EngineError::Validation("email must contain one @"));
    }
    let domain_has_dot = domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.');
    if !domain_has_dot {
        return Err(EngineError::Validation("email domain looks invalid"));
    }
    Ok(trimmed.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_and_normalizes() {
        assert_eq!(validate_email(" A@Example.COM ").unwrap(), "a@example.com");
        assert_eq!(
            validate_email("first.last+tag@sub.domain.org").unwrap(),
            "first.last+tag@sub.domain.org"
        );
    }

    #[test]
    fn rejects_bad_emails() {
        for bad in [
            "",
            "no-at-sign",
            "@nodomain.com",
            "nolocal@",
            "two@@ats.com",
            "a@b@c.com",
            "nodot@localhost",
            "trailingdot@domain.",
            "space in@local.com",
            "quote\"d@x.com",
        ] {
            assert!(validate_email(bad).is_err(), "{bad}");
        }
    }
}
