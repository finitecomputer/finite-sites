//! HTTP client for the Finite Sites API. Every request is signed with
//! NIP-98 over the exact URL and method, with the body hash bound for
//! requests that carry one.

use std::io::Read as _;

use finitesites_proto::dto::{
    ApiErrorBody, ClaimRequest, ClaimResponse, EditorsRequest, EditorsResponse, EmailLoginRequest,
    EmailLoginResponse, EmailRedeemRequest, EmailRedeemResponse, GitAuthRequest, GitAuthResponse,
    OwnerEmailRequest, ProjectApplyRequest, ProjectApplyResponse, PublishBeginRequest,
    PublishBeginResponse, PublishFinalizeResponse, SharingRequest, SharingResponse,
    SiteListResponse, SiteSummary, SourceSnapshotRequest,
};
use finitesites_proto::{PublishManifest, nip98};

use crate::CliError;
use crate::keys::KeyFile;

pub struct Client {
    base_url: String,
}

fn now_unix() -> u64 {
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    assert!(now > 0);
    now as u64
}

impl Client {
    pub fn from_env() -> Client {
        let base_url = std::env::var("FINITE_SITES_API")
            .unwrap_or_else(|_| "http://127.0.0.1:8787".to_string());
        Client {
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// Sign and send one request; decode the JSON response or surface the
    /// server's error body.
    fn request<T: serde::de::DeserializeOwned>(
        &self,
        key: &KeyFile,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
    ) -> Result<T, CliError> {
        assert!(path.starts_with('/'));
        let url = format!("{}{}", self.base_url, path);
        let auth_header = nip98::build_auth_header(&key.secret, &url, method, body, now_unix())
            .map_err(|error| CliError::Key(format!("cannot sign request: {error}")))?;

        let request = ureq::request(method, &url)
            .set("Authorization", &auth_header)
            .timeout(std::time::Duration::from_secs(600));
        let result = match body {
            Some(bytes) => request
                .set("Content-Type", content_type_for_body(path))
                .send_bytes(bytes),
            None => request.call(),
        };
        let response = match result {
            Ok(response) => response,
            Err(ureq::Error::Status(code, response)) => {
                let message = response
                    .into_json::<ApiErrorBody>()
                    .map(|body| body.message)
                    .unwrap_or_else(|_| "no error details".to_string());
                return Err(CliError::Api(format!("{method} {path}: {code}: {message}")));
            }
            Err(transport) => {
                return Err(CliError::Http(format!(
                    "{method} {url} failed: {transport} (is finitesitesd running?)"
                )));
            }
        };
        response
            .into_json::<T>()
            .map_err(|error| CliError::Api(format!("invalid response from server: {error}")))
    }

    pub fn claim(
        &self,
        user: &KeyFile,
        name: &str,
        site_pubkey: &str,
        owner_email: Option<&str>,
    ) -> Result<ClaimResponse, CliError> {
        let body = serde_json::to_vec(&ClaimRequest {
            name: name.to_string(),
            site_pubkey: site_pubkey.to_string(),
            owner_email: owner_email.map(str::to_string),
        })
        .expect("request serializes");
        self.request(user, "POST", "/api/v1/sites/claim", Some(&body))
    }

    pub fn apply_project(
        &self,
        user: &KeyFile,
        request: &ProjectApplyRequest,
    ) -> Result<ProjectApplyResponse, CliError> {
        let body = serde_json::to_vec(request).expect("request serializes");
        self.request(user, "POST", "/api/v1/projects/apply", Some(&body))
    }

    pub fn auth_git(
        &self,
        key: &KeyFile,
        project_slug: &str,
        request: &GitAuthRequest,
    ) -> Result<GitAuthResponse, CliError> {
        let body = serde_json::to_vec(request).expect("request serializes");
        self.request(
            key,
            "POST",
            &format!("/api/v1/projects/{project_slug}/git-auth"),
            Some(&body),
        )
    }

    pub fn begin_publish(
        &self,
        site_key: &KeyFile,
        name: &str,
        manifest: &PublishManifest,
        spa: bool,
        actor_email: Option<&str>,
        source: Option<&SourceSnapshotRequest>,
    ) -> Result<PublishBeginResponse, CliError> {
        let body = serde_json::to_vec(&PublishBeginRequest {
            manifest: manifest.clone(),
            spa,
            start_command: None,
            actor_email: actor_email.map(str::to_string),
            source: source.cloned(),
        })
        .expect("request serializes");
        self.request(
            site_key,
            "POST",
            &format!("/api/v1/sites/{name}/publish"),
            Some(&body),
        )
    }

    pub fn begin_publish_app(
        &self,
        site_key: &KeyFile,
        name: &str,
        manifest: &PublishManifest,
        start_command: &str,
    ) -> Result<PublishBeginResponse, CliError> {
        let body = serde_json::to_vec(&PublishBeginRequest {
            manifest: manifest.clone(),
            spa: false,
            start_command: Some(start_command.to_string()),
            actor_email: None,
            source: None,
        })
        .expect("request serializes");
        self.request(
            site_key,
            "POST",
            &format!("/api/v1/sites/{name}/publish"),
            Some(&body),
        )
    }

    pub fn upload_blob(
        &self,
        site_key: &KeyFile,
        publish_id: &str,
        sha256: &str,
        bytes: &[u8],
    ) -> Result<(), CliError> {
        let path = format!("/api/v1/publishes/{publish_id}/blobs/{sha256}");
        let _: serde_json::Value = self.request(site_key, "PUT", &path, Some(bytes))?;
        Ok(())
    }

    pub fn finalize_publish(
        &self,
        site_key: &KeyFile,
        publish_id: &str,
    ) -> Result<PublishFinalizeResponse, CliError> {
        self.request(
            site_key,
            "POST",
            &format!("/api/v1/publishes/{publish_id}/finalize"),
            None,
        )
    }

    pub fn list_sites(&self, user: &KeyFile) -> Result<SiteListResponse, CliError> {
        self.request(user, "GET", "/api/v1/sites", None)
    }

    pub fn site_status(&self, key: &KeyFile, name: &str) -> Result<SiteSummary, CliError> {
        self.request(key, "GET", &format!("/api/v1/sites/{name}"), None)
    }

    pub fn set_sharing(
        &self,
        key: &KeyFile,
        name: &str,
        request: &SharingRequest,
    ) -> Result<SharingResponse, CliError> {
        let body = serde_json::to_vec(request).expect("request serializes");
        self.request(
            key,
            "POST",
            &format!("/api/v1/sites/{name}/sharing"),
            Some(&body),
        )
    }

    pub fn request_email_login(&self, email: &str) -> Result<EmailLoginResponse, CliError> {
        let body = serde_json::to_vec(&EmailLoginRequest {
            email: email.to_string(),
        })
        .expect("request serializes");
        let url = format!("{}/api/v1/email-auth/request", self.base_url);
        let result = ureq::post(&url)
            .set("Content-Type", "application/json")
            .send_bytes(&body);
        match result {
            Ok(response) => response
                .into_json::<EmailLoginResponse>()
                .map_err(|error| CliError::Api(format!("invalid response from server: {error}"))),
            Err(ureq::Error::Status(code, response)) => {
                let message = response
                    .into_json::<ApiErrorBody>()
                    .map(|body| body.message)
                    .unwrap_or_else(|_| "no error details".to_string());
                Err(CliError::Api(format!(
                    "POST /api/v1/email-auth/request: {code}: {message}"
                )))
            }
            Err(transport) => Err(CliError::Http(format!(
                "POST {url} failed: {transport} (is finitesitesd running?)"
            ))),
        }
    }

    pub fn redeem_email_login(
        &self,
        key: &KeyFile,
        email: &str,
        token: &str,
    ) -> Result<EmailRedeemResponse, CliError> {
        let body = serde_json::to_vec(&EmailRedeemRequest {
            email: email.to_string(),
            token: token.to_string(),
        })
        .expect("request serializes");
        self.request(key, "POST", "/api/v1/email-auth/redeem", Some(&body))
    }

    pub fn set_owner_email(
        &self,
        key: &KeyFile,
        name: &str,
        owner_email: &str,
    ) -> Result<EditorsResponse, CliError> {
        let body = serde_json::to_vec(&OwnerEmailRequest {
            owner_email: owner_email.to_string(),
        })
        .expect("request serializes");
        self.request(
            key,
            "POST",
            &format!("/api/v1/sites/{name}/owner-email"),
            Some(&body),
        )
    }

    pub fn list_editors(
        &self,
        key: &KeyFile,
        name: &str,
        actor_email: Option<&str>,
    ) -> Result<EditorsResponse, CliError> {
        let path = match actor_email {
            Some(email) => format!("/api/v1/sites/{name}/editors?email={}", url_encode(email)),
            None => format!("/api/v1/sites/{name}/editors"),
        };
        self.request(key, "GET", &path, None)
    }

    pub fn update_editors(
        &self,
        key: &KeyFile,
        name: &str,
        request: &EditorsRequest,
    ) -> Result<EditorsResponse, CliError> {
        let body = serde_json::to_vec(request).expect("request serializes");
        self.request(
            key,
            "POST",
            &format!("/api/v1/sites/{name}/editors"),
            Some(&body),
        )
    }

    pub fn source_snapshot(
        &self,
        key: &KeyFile,
        name: &str,
        actor_email: Option<&str>,
    ) -> Result<Vec<u8>, CliError> {
        let path = match actor_email {
            Some(email) => format!("/api/v1/sites/{name}/source?email={}", url_encode(email)),
            None => format!("/api/v1/sites/{name}/source"),
        };
        assert!(path.starts_with('/'));
        let url = format!("{}{}", self.base_url, path);
        let auth_header = nip98::build_auth_header(&key.secret, &url, "GET", None, now_unix())
            .map_err(|error| CliError::Key(format!("cannot sign request: {error}")))?;
        let result = ureq::get(&url)
            .set("Authorization", &auth_header)
            .timeout(std::time::Duration::from_secs(600))
            .call();
        let response = match result {
            Ok(response) => response,
            Err(ureq::Error::Status(code, response)) => {
                let message = response
                    .into_json::<ApiErrorBody>()
                    .map(|body| body.message)
                    .unwrap_or_else(|_| "no error details".to_string());
                return Err(CliError::Api(format!("GET {path}: {code}: {message}")));
            }
            Err(transport) => {
                return Err(CliError::Http(format!(
                    "GET {url} failed: {transport} (is finitesitesd running?)"
                )));
            }
        };
        let mut bytes = Vec::new();
        response
            .into_reader()
            .read_to_end(&mut bytes)
            .map_err(|error| CliError::Api(format!("cannot read source response: {error}")))?;
        Ok(bytes)
    }
}

fn url_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

fn content_type_for_body(path: &str) -> &'static str {
    if path.contains("/blobs/") {
        "application/octet-stream"
    } else {
        "application/json"
    }
}
