//! JSON wire DTOs for the control-plane API. JSON is allowed here because
//! these are bounded request/response messages; authoritative state lives in
//! the registry schema, never in these shapes.

use serde::{Deserialize, Serialize};

use crate::manifest::PublishManifest;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimRequest {
    pub name: String,
    /// X-only pubkey hex of the per-site workspace-held signing key.
    pub site_pubkey: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimResponse {
    pub site_id: String,
    pub name: String,
    pub url: String,
    pub status: String,
    /// True when this claim already existed for the same owner + site key.
    pub already_claimed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishBeginRequest {
    pub manifest: PublishManifest,
    /// Single-page app: serve `/index.html` for unknown paths instead of a
    /// 404, so client-side routers handle deep links. Defaults to false.
    #[serde(default)]
    pub spa: bool,
    /// Tier 2 app publish: the shell command that starts the server (it
    /// must listen on `$PORT`). When set, the manifest must contain exactly
    /// one entry, the `/app.tar.gz` bundle. `None` means a static publish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishBeginResponse {
    pub publish_id: String,
    /// Hashes the server does not have yet; upload exactly these.
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishFinalizeResponse {
    pub site_id: String,
    pub version_number: u32,
    pub url: String,
    pub path_count: u32,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharingRequest {
    /// Target visibility: "private", "shared", or "public". Omit to keep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    /// Required by the server when visibility is "public"; proves the agent
    /// surfaced the public-site warning to the human first.
    #[serde(default)]
    pub confirm_public: bool,
    #[serde(default)]
    pub add_emails: Vec<String>,
    #[serde(default)]
    pub remove_emails: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharingResponse {
    pub visibility: String,
    pub shared_emails: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteSummary {
    pub site_id: String,
    pub name: String,
    pub url: String,
    pub status: String,
    pub visibility: String,
    /// "static" or "app". Defaulted for wire-compat with older peers.
    #[serde(default)]
    pub kind: String,
    pub active_version: Option<u32>,
    pub shared_emails: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteListResponse {
    pub sites: Vec<SiteSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiErrorBody {
    pub error: String,
    pub message: String,
}
