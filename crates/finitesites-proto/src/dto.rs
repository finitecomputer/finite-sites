//! JSON wire DTOs for the control-plane API. JSON is allowed here because
//! these are bounded request/response messages; authoritative state lives in
//! the registry schema, never in these shapes.

use serde::{Deserialize, Serialize};

use crate::manifest::PublishManifest;
use crate::project_config::ProjectConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimRequest {
    pub name: String,
    /// X-only pubkey hex of the per-site workspace-held signing key.
    pub site_pubkey: String,
    /// Optional human-facing owner email for email-keyed publishing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimResponse {
    pub site_id: String,
    pub name: String,
    pub url: String,
    pub status: String,
    /// True when this claim already existed for the same owner + site key.
    pub already_claimed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceSnapshotRequest {
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceSnapshotInfo {
    pub version_number: u32,
    pub sha256: String,
    pub size: u64,
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
    /// Email identity for email-keyed publishing. When omitted, the signer
    /// must be the Site Key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_email: Option<String>,
    /// Optional source archive attached to the finalized version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceSnapshotRequest>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceSnapshotInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailLoginRequest {
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailLoginResponse {
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailRedeemRequest {
    pub email: String,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailRedeemResponse {
    pub email: String,
    pub pubkey: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnerEmailRequest {
    pub owner_email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditorsRequest {
    /// Email identity for email-keyed owner actions. When omitted, the signer
    /// must be the Owner User Key or Site Key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_email: Option<String>,
    #[serde(default)]
    pub add_emails: Vec<String>,
    #[serde(default)]
    pub remove_emails: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditorsResponse {
    pub owner_email: Option<String>,
    pub editor_emails: Vec<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_email: Option<String>,
    pub active_version: Option<u32>,
    pub shared_emails: Vec<String>,
    #[serde(default)]
    pub editor_emails: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceSnapshotInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteListResponse {
    pub sites: Vec<SiteSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectApplyRequest {
    pub config: ProjectConfig,
    /// True means validate and return the exact operations without mutating
    /// registry state or writing a git repository.
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub collaborators: Vec<ProjectCollaboratorSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectCollaboratorSpec {
    /// Milestone 1 supports External Principals by verified email. Native
    /// npub shares use the same role shape once Agent Delegations land.
    pub email: String,
    #[serde(default = "default_project_role")]
    pub role: String,
}

fn default_project_role() -> String {
    "editor".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectApplyResponse {
    pub dry_run: bool,
    pub project_id: Option<String>,
    pub slug: String,
    pub created: bool,
    pub git_remote_url: String,
    pub finite_toml: String,
    pub outputs: Vec<ProjectOutputSummary>,
    pub collaborators: Vec<ProjectCollaboratorSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectOutputSummary {
    pub output_id: String,
    pub kind: String,
    pub site_name: String,
    pub site_id: Option<String>,
    pub site_url: String,
    pub branch: String,
    pub path: String,
    pub spa: bool,
    pub created: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectCollaboratorSummary {
    pub principal_id: Option<String>,
    pub email: String,
    pub role: String,
    pub created: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitAuthRequest {
    /// Email identity whose verified local key signs this request.
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitAuthResponse {
    pub project_slug: String,
    pub git_remote_url: String,
    pub credential_id: String,
    /// Use as the HTTPS Basic username for standard git clients.
    pub username: String,
    /// Returned once. Store it in the agent's git credential helper, not in
    /// source control or project files.
    pub password: String,
    pub expires_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiErrorBody {
    pub error: String,
    pub message: String,
}
