//! HTTP client for the Finite Sites API. Every request is signed with
//! NIP-98 over the exact URL and method, with the body hash bound for
//! requests that carry one.

use finitesites_proto::dto::{
    ApiErrorBody, ClaimRequest, ClaimResponse, PublishBeginRequest, PublishBeginResponse,
    PublishFinalizeResponse, SharingRequest, SharingResponse, SiteListResponse, SiteSummary,
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
    ) -> Result<ClaimResponse, CliError> {
        let body = serde_json::to_vec(&ClaimRequest {
            name: name.to_string(),
            site_pubkey: site_pubkey.to_string(),
        })
        .expect("request serializes");
        self.request(user, "POST", "/api/v1/sites/claim", Some(&body))
    }

    pub fn begin_publish(
        &self,
        site_key: &KeyFile,
        name: &str,
        manifest: &PublishManifest,
        spa: bool,
    ) -> Result<PublishBeginResponse, CliError> {
        let body = serde_json::to_vec(&PublishBeginRequest {
            manifest: manifest.clone(),
            spa,
            start_command: None,
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
}

fn content_type_for_body(path: &str) -> &'static str {
    if path.contains("/blobs/") {
        "application/octet-stream"
    } else {
        "application/json"
    }
}
