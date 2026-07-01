//! JSON wire DTOs for the control-plane API. JSON is allowed here because
//! these are bounded request/response messages; authoritative state lives in
//! the registry schema, never in these shapes.

use serde::{Deserialize, Serialize};

use crate::project_config::ProjectConfig;

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
    #[serde(default)]
    pub invited_emails: Vec<String>,
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
pub struct ProjectInitRequest {
    pub config: ProjectConfig,
    /// True means validate and return the exact operations without mutating
    /// registry state or writing a git repository.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectGrantRequest {
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
pub struct ProjectInitResponse {
    pub dry_run: bool,
    pub project_id: Option<String>,
    pub slug: String,
    pub created: bool,
    pub git_remote_url: String,
    pub finite_toml: String,
    pub outputs: Vec<ProjectOutputSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectOutputSummary {
    pub output_id: String,
    pub kind: String,
    pub site_name: String,
    pub site_id: Option<String>,
    pub site_url: String,
    pub status: String,
    pub visibility: String,
    pub active_version: Option<u32>,
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
pub struct ProjectGrantResponse {
    pub project_slug: String,
    pub collaborator: ProjectCollaboratorSummary,
    #[serde(default)]
    pub invited_emails: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRevokeRequest {
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRevokeResponse {
    pub project_slug: String,
    pub email: String,
    pub removed: bool,
    pub revoked_git_credentials: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectStatusResponse {
    pub project_id: String,
    pub slug: String,
    pub git_remote_url: String,
    pub role: String,
    pub outputs: Vec<ProjectOutputSummary>,
    pub collaborators: Vec<ProjectCollaboratorSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectListResponse {
    pub projects: Vec<ProjectListItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectListItem {
    pub project_id: String,
    pub slug: String,
    pub git_remote_url: String,
    pub role: String,
    pub outputs: Vec<ProjectOutputSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitAuthRequest {
    /// Email identity whose verified local key signs this request. Omit this
    /// when the local User Key is already a native Project Collaborator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
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
