//! Outbound mail. Two implementations behind one trait:
//!
//! - `DevMailer` (default): writes each magic-link email to a file under
//!   `DATA/outbox/` and logs the link. Local development only.
//! - `HttpMailer`: sends through Resend or Postmark via their JSON APIs.
//!   Selected with `--mailer resend|postmark`; the API key comes from the
//!   RESEND_API_KEY / POSTMARK_SERVER_TOKEN environment variable so secrets
//!   stay in the service env file, never in argv.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use finitesites_proto::{hex, ids};

#[derive(Debug, thiserror::Error)]
pub enum MailerError {
    #[error("mail io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("mail send failed: {0}")]
    Send(String),
}

pub trait Mailer: Send + Sync {
    fn send_login_link(&self, email: &str, site_name: &str, url: &str) -> Result<(), MailerError>;
}

/// Message text is shared by every mailer so dev output matches what real
/// recipients see.
fn login_link_subject(site_name: &str) -> String {
    format!("Your link to {site_name}")
}

fn login_link_text(site_name: &str, url: &str) -> String {
    format!(
        "Open this link to view {site_name}:\n\n{url}\n\n\
         The link works once and expires in 15 minutes. If you did not \
         request it, you can ignore this email.\n"
    )
}

// ---- dev mailer ------------------------------------------------------------

pub struct DevMailer {
    outbox_dir: PathBuf,
}

impl DevMailer {
    pub fn new(outbox_dir: PathBuf) -> Result<DevMailer, MailerError> {
        std::fs::create_dir_all(&outbox_dir)?;
        Ok(DevMailer { outbox_dir })
    }
}

impl Mailer for DevMailer {
    fn send_login_link(&self, email: &str, site_name: &str, url: &str) -> Result<(), MailerError> {
        let nonce = hex::encode(&ids::random_32()[..4]);
        let safe_email: String = email
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let path = self.outbox_dir.join(format!("{nonce}-{safe_email}.txt"));
        let mut file = std::fs::File::create(&path)?;
        writeln!(file, "To: {email}")?;
        writeln!(file, "Subject: {}", login_link_subject(site_name))?;
        writeln!(file)?;
        write!(file, "{}", login_link_text(site_name, url))?;
        eprintln!(
            "dev-mail: login link for {email} -> {url} (written to {})",
            path.display()
        );
        Ok(())
    }
}

// ---- http mailer (Resend / Postmark) ----------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailProvider {
    Resend,
    Postmark,
}

impl MailProvider {
    pub fn parse(value: &str) -> Option<MailProvider> {
        match value {
            "resend" => Some(MailProvider::Resend),
            "postmark" => Some(MailProvider::Postmark),
            _ => None,
        }
    }

    pub fn api_key_env_var(&self) -> &'static str {
        match self {
            MailProvider::Resend => "RESEND_API_KEY",
            MailProvider::Postmark => "POSTMARK_SERVER_TOKEN",
        }
    }

    fn endpoint(&self) -> &'static str {
        match self {
            MailProvider::Resend => "https://api.resend.com/emails",
            MailProvider::Postmark => "https://api.postmarkapp.com/email",
        }
    }

    fn auth_header(&self) -> &'static str {
        match self {
            MailProvider::Resend => "Authorization",
            MailProvider::Postmark => "X-Postmark-Server-Token",
        }
    }
}

pub struct HttpMailer {
    provider: MailProvider,
    api_key: String,
    from_address: String,
    agent: ureq::Agent,
}

impl HttpMailer {
    pub fn new(provider: MailProvider, api_key: String, from_address: String) -> HttpMailer {
        assert!(!api_key.is_empty() && from_address.contains('@'));
        HttpMailer {
            provider,
            api_key,
            from_address,
            // Login mail is latency-sensitive; fail fast and let the viewer
            // retry rather than hanging the request.
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(10))
                .build(),
        }
    }
}

/// Build the provider-specific JSON payload. Split out for tests.
fn build_payload(
    provider: MailProvider,
    from_address: &str,
    to_email: &str,
    site_name: &str,
    url: &str,
) -> serde_json::Value {
    match provider {
        MailProvider::Resend => serde_json::json!({
            "from": from_address,
            "to": [to_email],
            "subject": login_link_subject(site_name),
            "text": login_link_text(site_name, url),
        }),
        MailProvider::Postmark => serde_json::json!({
            "From": from_address,
            "To": to_email,
            "Subject": login_link_subject(site_name),
            "TextBody": login_link_text(site_name, url),
            "MessageStream": "outbound",
        }),
    }
}

impl Mailer for HttpMailer {
    fn send_login_link(&self, email: &str, site_name: &str, url: &str) -> Result<(), MailerError> {
        let payload = build_payload(self.provider, &self.from_address, email, site_name, url);
        let auth_value = match self.provider {
            MailProvider::Resend => format!("Bearer {}", self.api_key),
            MailProvider::Postmark => self.api_key.clone(),
        };
        let result = self
            .agent
            .post(self.provider.endpoint())
            .set(self.provider.auth_header(), &auth_value)
            .set("Accept", "application/json")
            .send_json(payload);
        match result {
            Ok(_response) => Ok(()),
            Err(ureq::Error::Status(code, response)) => {
                // Provider error bodies are short JSON; bound the read and
                // log enough to debug deliverability without the API key.
                let body = response
                    .into_string()
                    .unwrap_or_else(|_| "unreadable body".to_string());
                let truncated: String = body.chars().take(500).collect();
                Err(MailerError::Send(format!(
                    "provider returned {code}: {truncated}"
                )))
            }
            Err(transport) => Err(MailerError::Send(format!("transport error: {transport}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payloads_match_provider_shapes() {
        let resend = build_payload(
            MailProvider::Resend,
            "Finite Sites <sites@finite.chat>",
            "friend@example.com",
            "hello",
            "https://hello.finite.chat/_finite/auth?token=abc",
        );
        assert_eq!(resend["to"][0], "friend@example.com");
        assert_eq!(resend["subject"], "Your link to hello");
        assert!(resend["text"].as_str().unwrap().contains("token=abc"));

        let postmark = build_payload(
            MailProvider::Postmark,
            "sites@finite.chat",
            "friend@example.com",
            "hello",
            "https://hello.finite.chat/_finite/auth?token=abc",
        );
        assert_eq!(postmark["To"], "friend@example.com");
        assert_eq!(postmark["MessageStream"], "outbound");
        assert!(postmark["TextBody"].as_str().unwrap().contains("token=abc"));
    }

    #[test]
    fn provider_parsing_and_env_vars() {
        assert_eq!(MailProvider::parse("resend"), Some(MailProvider::Resend));
        assert_eq!(
            MailProvider::parse("postmark"),
            Some(MailProvider::Postmark)
        );
        assert_eq!(MailProvider::parse("sendgrid"), None);
        assert_eq!(MailProvider::Resend.api_key_env_var(), "RESEND_API_KEY");
        assert_eq!(
            MailProvider::Postmark.api_key_env_var(),
            "POSTMARK_SERVER_TOKEN"
        );
    }
}
