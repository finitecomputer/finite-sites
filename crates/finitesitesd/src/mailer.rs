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

use finitesites_proto::dto::ProjectOutputSummary;
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
    fn send_email_login_token(&self, email: &str, token: &str) -> Result<(), MailerError>;
    fn send_viewer_invite(&self, invite: &ViewerInvite<'_>) -> Result<(), MailerError>;
    fn send_project_collaborator_invite(
        &self,
        invite: &ProjectCollaboratorInvite<'_>,
    ) -> Result<(), MailerError>;
}

pub struct ViewerInvite<'a> {
    pub email: &'a str,
    pub site_name: &'a str,
    pub site_url: &'a str,
    pub login_url: &'a str,
}

pub struct ProjectCollaboratorInvite<'a> {
    pub email: &'a str,
    pub project_slug: &'a str,
    pub role: &'a str,
    pub api_url: &'a str,
    pub git_remote_url: &'a str,
    pub email_login_token: &'a str,
    pub outputs: &'a [ProjectOutputSummary],
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

fn email_login_subject() -> &'static str {
    "Your Finite Sites email login"
}

fn email_login_text(email: &str, token: &str) -> String {
    format!(
        "Run this command to verify {email} for Finite Sites publishing:\n\n\
         fsite email-redeem {email} {token}\n\n\
         The token works once and expires in 15 minutes. If you did not \
         request it, you can ignore this email.\n"
    )
}

fn viewer_invite_subject(site_name: &str) -> String {
    format!("You've been invited to view {site_name}")
}

fn viewer_invite_text(invite: &ViewerInvite<'_>) -> String {
    format!(
        "You and your agent have been invited to view {site_name}.\n\n\
         Open this one-time link to sign in:\n\n{login_url}\n\n\
         After signing in, view the site here:\n\n{site_url}\n\n\
         Agents should inspect these instructions first:\n\n{llms_url}\n\n\
         The sign-in link works once and expires in 15 minutes. If it expires, \
         open the site URL and request a fresh link for {email}.\n",
        site_name = invite.site_name,
        login_url = invite.login_url,
        site_url = invite.site_url,
        llms_url = output_url(invite.site_url, "/llms.txt"),
        email = invite.email,
    )
}

fn project_collaborator_invite_subject(project_slug: &str) -> String {
    format!("You've been invited to collaborate on {project_slug}")
}

fn project_collaborator_invite_text(invite: &ProjectCollaboratorInvite<'_>) -> String {
    let api_prefix = api_prefix(invite.api_url);
    let mut text = format!(
        "You and your agent have been invited to collaborate on {project_slug} as {role}.\n\n\
         To authenticate this machine for {email}, run:\n\n\
         {api_prefix}fsite email-redeem {email} {token}\n\n\
         Then mint a scoped git credential and clone the project:\n\n\
         {api_prefix}fsite auth git {project_slug} --email {email} --store --output json\n\
         git clone {git_remote_url}\n\n\
         Edit the repository, commit your changes, and push the deploy branch.\n\
         The email token works once and expires in 15 minutes. If it expires, run:\n\n\
         {api_prefix}fsite email-login {email}\n\n",
        project_slug = invite.project_slug,
        role = invite.role,
        email = invite.email,
        token = invite.email_login_token,
        api_prefix = api_prefix,
        git_remote_url = invite.git_remote_url,
    );
    if !invite.outputs.is_empty() {
        text.push_str("Project outputs:\n");
        for output in invite.outputs {
            text.push_str(&format!(
                "- {} ({}) -> {}\n",
                output.output_id, output.kind, output.site_url
            ));
        }
    }
    text
}

fn output_url(base_url: &str, path: &str) -> String {
    format!("{}{}", base_url.trim_end_matches('/'), path)
}

fn api_prefix(api_url: &str) -> String {
    if api_url == "https://api.finite.chat" {
        String::new()
    } else {
        format!("FINITE_SITES_API={api_url} ")
    }
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

    fn send_email_login_token(&self, email: &str, token: &str) -> Result<(), MailerError> {
        let nonce = hex::encode(&ids::random_32()[..4]);
        let safe_email: String = email
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let path = self
            .outbox_dir
            .join(format!("{nonce}-{safe_email}-email-login.txt"));
        let mut file = std::fs::File::create(&path)?;
        writeln!(file, "To: {email}")?;
        writeln!(file, "Subject: {}", email_login_subject())?;
        writeln!(file)?;
        write!(file, "{}", email_login_text(email, token))?;
        eprintln!(
            "dev-mail: email login token for {email} -> {token} (written to {})",
            path.display()
        );
        Ok(())
    }

    fn send_viewer_invite(&self, invite: &ViewerInvite<'_>) -> Result<(), MailerError> {
        self.write_text_email(
            invite.email,
            &viewer_invite_subject(invite.site_name),
            &viewer_invite_text(invite),
            "viewer-invite",
        )
    }

    fn send_project_collaborator_invite(
        &self,
        invite: &ProjectCollaboratorInvite<'_>,
    ) -> Result<(), MailerError> {
        self.write_text_email(
            invite.email,
            &project_collaborator_invite_subject(invite.project_slug),
            &project_collaborator_invite_text(invite),
            "project-invite",
        )
    }
}

impl DevMailer {
    fn write_text_email(
        &self,
        email: &str,
        subject: &str,
        text: &str,
        suffix: &str,
    ) -> Result<(), MailerError> {
        let nonce = hex::encode(&ids::random_32()[..4]);
        let safe_email: String = email
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let path = self
            .outbox_dir
            .join(format!("{nonce}-{safe_email}-{suffix}.txt"));
        let mut file = std::fs::File::create(&path)?;
        writeln!(file, "To: {email}")?;
        writeln!(file, "Subject: {subject}")?;
        writeln!(file)?;
        write!(file, "{text}")?;
        eprintln!(
            "dev-mail: {suffix} for {email} (written to {})",
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
    build_text_payload(
        provider,
        from_address,
        to_email,
        &login_link_subject(site_name),
        &login_link_text(site_name, url),
    )
}

fn build_text_payload(
    provider: MailProvider,
    from_address: &str,
    to_email: &str,
    subject: &str,
    text: &str,
) -> serde_json::Value {
    match provider {
        MailProvider::Resend => serde_json::json!({
            "from": from_address,
            "to": [to_email],
            "subject": subject,
            "text": text,
        }),
        MailProvider::Postmark => serde_json::json!({
            "From": from_address,
            "To": to_email,
            "Subject": subject,
            "TextBody": text,
            "MessageStream": "outbound",
        }),
    }
}

impl Mailer for HttpMailer {
    fn send_login_link(&self, email: &str, site_name: &str, url: &str) -> Result<(), MailerError> {
        let payload = build_payload(self.provider, &self.from_address, email, site_name, url);
        self.send_payload(payload)
    }

    fn send_email_login_token(&self, email: &str, token: &str) -> Result<(), MailerError> {
        let payload = build_text_payload(
            self.provider,
            &self.from_address,
            email,
            email_login_subject(),
            &email_login_text(email, token),
        );
        self.send_payload(payload)
    }

    fn send_viewer_invite(&self, invite: &ViewerInvite<'_>) -> Result<(), MailerError> {
        let payload = build_text_payload(
            self.provider,
            &self.from_address,
            invite.email,
            &viewer_invite_subject(invite.site_name),
            &viewer_invite_text(invite),
        );
        self.send_payload(payload)
    }

    fn send_project_collaborator_invite(
        &self,
        invite: &ProjectCollaboratorInvite<'_>,
    ) -> Result<(), MailerError> {
        let payload = build_text_payload(
            self.provider,
            &self.from_address,
            invite.email,
            &project_collaborator_invite_subject(invite.project_slug),
            &project_collaborator_invite_text(invite),
        );
        self.send_payload(payload)
    }
}

impl HttpMailer {
    fn send_payload(&self, payload: serde_json::Value) -> Result<(), MailerError> {
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
    use finitesites_proto::dto::ProjectOutputSummary;

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
    fn invite_texts_are_agent_actionable() {
        let viewer = viewer_invite_text(&ViewerInvite {
            email: "friend@example.com",
            site_name: "hello",
            site_url: "https://hello.finite.chat/",
            login_url: "https://hello.finite.chat/_finite/auth?token=abc",
        });
        assert!(viewer.contains("You and your agent have been invited to view hello"));
        assert!(viewer.contains("_finite/auth?token=abc"));
        assert!(viewer.contains("https://hello.finite.chat/llms.txt"));

        let outputs = vec![ProjectOutputSummary {
            output_id: "mockup".to_string(),
            kind: "site".to_string(),
            site_name: "finitechat-native-mockup".to_string(),
            site_id: Some("site_1".to_string()),
            site_url: "https://finitechat-native-mockup.finite.chat/".to_string(),
            branch: "main".to_string(),
            path: ".".to_string(),
            spa: false,
            created: false,
        }];
        let project = project_collaborator_invite_text(&ProjectCollaboratorInvite {
            email: "skyler@example.com",
            project_slug: "finitechat-native",
            role: "editor",
            api_url: "https://api.finite.chat",
            git_remote_url: "https://git.finite.chat/finitechat-native.git",
            email_login_token: "token123",
            outputs: &outputs,
        });
        assert!(project.contains("fsite email-redeem skyler@example.com token123"));
        assert!(project.contains(
            "fsite auth git finitechat-native --email skyler@example.com --store --output json"
        ));
        assert!(project.contains("git clone https://git.finite.chat/finitechat-native.git"));
        assert!(project.contains("mockup (site)"));
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
