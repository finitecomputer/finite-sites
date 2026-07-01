//! HTTP client for the Finite Sites API. Every request is signed with
//! NIP-98 over the exact URL and method, with the body hash bound for
//! requests that carry one.

use finitesites_proto::dto::{
    ApiErrorBody, EmailLoginRequest, EmailLoginResponse, EmailRedeemRequest, EmailRedeemResponse,
    GitAuthRequest, GitAuthResponse, ProjectGrantRequest, ProjectGrantResponse, ProjectInitRequest,
    ProjectInitResponse, ProjectListResponse, ProjectRevokeRequest, ProjectRevokeResponse,
    ProjectStatusResponse,
};
use finitesites_proto::nip98;

use crate::CliError;
use crate::keys::KeyFile;

pub struct Client {
    base_url: String,
}

const DEFAULT_API_URL: &str = "https://api.finite.chat";

fn base_url_from_env_value(value: Option<String>) -> String {
    value
        .unwrap_or_else(|| DEFAULT_API_URL.to_string())
        .trim_end_matches('/')
        .to_string()
}

fn now_unix() -> u64 {
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    assert!(now > 0);
    now as u64
}

impl Client {
    pub fn from_env() -> Client {
        let base_url = base_url_from_env_value(std::env::var("FINITE_SITES_API").ok());
        Client { base_url }
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
                return Err(CliError::ApiStatus {
                    method: method.to_string(),
                    path: path.to_string(),
                    status: code,
                    message,
                });
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

    pub fn init_project(
        &self,
        user: &KeyFile,
        request: &ProjectInitRequest,
    ) -> Result<ProjectInitResponse, CliError> {
        let body = serde_json::to_vec(request).expect("request serializes");
        self.request(user, "POST", "/api/v1/projects/init", Some(&body))
    }

    pub fn project_status(
        &self,
        key: &KeyFile,
        project_slug: &str,
    ) -> Result<ProjectStatusResponse, CliError> {
        self.request(
            key,
            "GET",
            &format!("/api/v1/projects/{project_slug}"),
            None,
        )
    }

    pub fn project_list(&self, key: &KeyFile) -> Result<ProjectListResponse, CliError> {
        self.request(key, "GET", "/api/v1/projects", None)
    }

    pub fn grant_project(
        &self,
        key: &KeyFile,
        project_slug: &str,
        request: &ProjectGrantRequest,
        send_invite: bool,
    ) -> Result<ProjectGrantResponse, CliError> {
        let body = serde_json::to_vec(request).expect("request serializes");
        self.request(
            key,
            "POST",
            &format!(
                "/api/v1/projects/{project_slug}/grant{}",
                invite_query(send_invite)
            ),
            Some(&body),
        )
    }

    pub fn revoke_project(
        &self,
        key: &KeyFile,
        project_slug: &str,
        request: &ProjectRevokeRequest,
    ) -> Result<ProjectRevokeResponse, CliError> {
        let body = serde_json::to_vec(request).expect("request serializes");
        self.request(
            key,
            "POST",
            &format!("/api/v1/projects/{project_slug}/revoke"),
            Some(&body),
        )
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
}

fn invite_query(send_invites: bool) -> &'static str {
    if send_invites {
        "?send_invites=true"
    } else {
        ""
    }
}

fn content_type_for_body(path: &str) -> &'static str {
    if path.contains("/blobs/") {
        "application/octet-stream"
    } else {
        "application/json"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_api_is_the_default() {
        assert_eq!(base_url_from_env_value(None), "https://api.finite.chat");
        assert_eq!(
            base_url_from_env_value(Some("http://127.0.0.1:8787/".to_string())),
            "http://127.0.0.1:8787"
        );
    }
}
