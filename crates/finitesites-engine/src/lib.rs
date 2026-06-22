//! Control-plane and serving logic for Finite Sites.
//!
//! The engine owns every decision: who may create project outputs, which git
//! pushes become versions, and who may view a site. The store persists, the
//! blob store holds bytes, and the HTTP layer above translates outcomes into
//! responses.

mod cookie;
mod email;

pub use cookie::ViewerCookie;
pub use email::validate_email;

use thiserror::Error;

use finitesites_blob::{BlobError, BlobStore};
use finitesites_proto::dto::{
    GitAuthResponse, ProjectApplyRequest, ProjectCollaboratorRemoveResponse,
    ProjectCollaboratorSummary, ProjectOutputSummary, SharingRequest, SharingResponse, SiteSummary,
};
use finitesites_proto::limits::{
    LOGIN_TOKEN_TTL_SECONDS, MAX_EMAIL_KEYS_PER_EMAIL, MAX_EMAILS_PER_SHARING_REQUEST,
    MAX_FILE_BYTES, MAX_PROJECT_COLLABORATORS, MAX_SHARES_PER_SITE, VIEWER_COOKIE_TTL_SECONDS,
};
use finitesites_proto::manifest::APP_BUNDLE_PATH;
use finitesites_proto::{ManifestFile, ProtoError, PublishManifest, hex, ids, names};
use finitesites_store::{
    GitRefEventRecord, ProjectApplyStoreOutcome, ProjectCollaboratorApply, ProjectCollaboratorRole,
    ProjectOutputApply, ProjectOutputRecord, ProjectRecord, SiteKind, SiteRecord, SiteStatus,
    Store, StoreError, Visibility,
};
use sha2::{Digest, Sha256};

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("pubkey has no active publish grant")]
    NotAllowlisted,
    #[error("name already claimed")]
    NameTaken,
    #[error("site not found")]
    SiteNotFound,
    #[error("project not found")]
    ProjectNotFound,
    #[error("signer is not authorized for this site")]
    NotAuthorized,
    #[error("too many sites for this owner")]
    TooManySites,
    #[error("too many shared emails for this site")]
    TooManyShares,
    #[error("too many active keys for this email")]
    TooManyEmailKeys,
    #[error("too many collaborators for this project")]
    TooManyProjectCollaborators,
    #[error("validation failed: {0}")]
    Validation(&'static str),
    #[error("conflict: {0}")]
    Conflict(&'static str),
    #[error(transparent)]
    Proto(#[from] ProtoError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Blob(#[from] BlobError),
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Domain under which sites live, e.g. `sites.localhost` or `finite.chat`.
    pub base_domain: String,
    /// `http` for local development, `https` behind real TLS.
    pub site_url_scheme: String,
    /// Port to include in generated site URLs; `None` for default ports.
    pub site_url_port: Option<u16>,
}

impl EngineConfig {
    pub fn site_url(&self, name: &str) -> String {
        assert!(!name.is_empty());
        let port_part = match self.site_url_port {
            Some(port) => format!(":{port}"),
            None => String::new(),
        };
        format!(
            "{}://{}.{}{}/",
            self.site_url_scheme, name, self.base_domain, port_part
        )
    }
}

#[derive(Debug)]
pub struct FinalizeOutcome {
    pub site_id: String,
    pub name: String,
    pub url: String,
    pub version_id: String,
    pub version_number: u32,
    pub path_count: u32,
    pub total_bytes: u64,
    /// Set for app sites: what the supervisor needs to (re)deploy.
    pub app: Option<AppDeploy>,
}

#[derive(Debug, Clone)]
pub struct EmailLoginToken {
    pub email: String,
    pub token: String,
}

/// Everything the app supervisor needs to deploy one finalized version.
#[derive(Debug, Clone)]
pub struct AppDeploy {
    pub site_id: String,
    pub version_id: String,
    pub bundle_sha256: String,
    pub start_command: String,
    pub port: u16,
}

/// A manifest entry resolved for serving.
#[derive(Debug, Clone)]
pub struct FoundFile {
    pub path: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ViewAccess {
    /// Viewer may see the site content.
    Allowed,
    /// Viewer must authenticate via magic link (or never can, for private).
    NeedsLogin,
}

#[derive(Debug)]
pub struct LoginLink {
    pub site_name: String,
    pub email: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct GitCredentialAuth {
    pub project_id: String,
    pub project_slug: String,
    pub principal_id: String,
    pub actor_agent_key_id: Option<String>,
    pub git_credential_id: String,
    pub can_push: bool,
}

pub struct Engine {
    store: Store,
    blobs: BlobStore,
    cookie_secret: [u8; 32],
    config: EngineConfig,
}

impl Engine {
    pub fn new(
        store: Store,
        blobs: BlobStore,
        cookie_secret: [u8; 32],
        config: EngineConfig,
    ) -> Engine {
        assert!(!config.base_domain.is_empty());
        Engine {
            store,
            blobs,
            cookie_secret,
            config,
        }
    }

    pub fn site_url(&self, name: &str) -> String {
        self.config.site_url(name)
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    pub fn store_mut(&mut self) -> &mut Store {
        &mut self.store
    }

    // ---- projects ----------------------------------------------------------

    pub fn apply_project(
        &mut self,
        owner_pubkey: &str,
        request: &ProjectApplyRequest,
        git_remote_url: String,
        now: u64,
    ) -> Result<finitesites_proto::dto::ProjectApplyResponse, EngineError> {
        assert!(hex::is_hex32(owner_pubkey));
        request.config.validate()?;
        if !self.store.has_publish_access(owner_pubkey, now)? {
            return Err(EngineError::NotAllowlisted);
        }
        let finite_toml = request.config.to_toml_string()?;
        let outputs = output_apply_inputs(request);
        let collaborators = collaborator_apply_inputs(request)?;
        if request.dry_run {
            return self.dry_run_project_apply(
                owner_pubkey,
                request,
                &outputs,
                &collaborators,
                git_remote_url,
                finite_toml,
            );
        }

        let outcome = match self.store.apply_project(
            owner_pubkey,
            &request.config.project.slug,
            &outputs,
            &collaborators,
            now,
        ) {
            Ok(outcome) => outcome,
            Err(StoreError::Conflict("site name already claimed")) => {
                return Err(EngineError::NameTaken);
            }
            Err(error) => return Err(error.into()),
        };
        Ok(self.project_apply_response_from_store(
            request.dry_run,
            git_remote_url,
            finite_toml,
            outcome,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn dry_run_project_apply(
        &self,
        owner_pubkey: &str,
        request: &ProjectApplyRequest,
        outputs: &[ProjectOutputApply],
        collaborators: &[ProjectCollaboratorApply],
        git_remote_url: String,
        finite_toml: String,
    ) -> Result<finitesites_proto::dto::ProjectApplyResponse, EngineError> {
        let existing_project = self.store.project_by_slug(&request.config.project.slug)?;
        let project_id = existing_project.as_ref().map(|project| project.id.clone());
        if let Some(project) = &existing_project {
            let owner_principal = self
                .store
                .principal_by_pubkey(owner_pubkey)?
                .ok_or(StoreError::CorruptState("project owner principal missing"))?;
            if project.owner_principal_id != owner_principal.id {
                return Err(EngineError::Conflict("project slug already exists"));
            }
        }
        let existing_outputs = match &existing_project {
            Some(project) => self.store.project_outputs(&project.id)?,
            None => Vec::new(),
        };

        let mut output_summaries = Vec::with_capacity(outputs.len());
        // Bounded by MAX_PROJECT_OUTPUTS, validated in Project Config.
        for output in outputs {
            let existing = existing_outputs
                .iter()
                .find(|record| record.output_id == output.output_id);
            if let Some(record) = existing {
                if record.kind != output.kind || record.site_name != output.site_name {
                    return Err(EngineError::Conflict(
                        "project output kind or site name cannot change",
                    ));
                }
                output_summaries.push(project_output_summary(record, false, &self.config));
                continue;
            }
            if self.store.site_by_name(&output.site_name)?.is_some() {
                return Err(EngineError::NameTaken);
            }
            output_summaries.push(ProjectOutputSummary {
                output_id: output.output_id.clone(),
                kind: output.kind.clone(),
                site_name: output.site_name.clone(),
                site_id: None,
                site_url: self.config.site_url(&output.site_name),
                branch: output.branch.clone(),
                path: output.path.clone(),
                spa: output.spa,
                created: true,
            });
        }

        let mut collaborator_summaries = Vec::with_capacity(collaborators.len());
        // Bounded by MAX_PROJECT_COLLABORATORS, checked in collaborator_apply_inputs.
        for collaborator in collaborators {
            let principal = self.store.principal_by_email(&collaborator.email)?;
            collaborator_summaries.push(ProjectCollaboratorSummary {
                principal_id: principal.map(|record| record.id),
                email: collaborator.email.clone(),
                role: collaborator.role.as_str().to_string(),
                created: true,
            });
        }

        Ok(finitesites_proto::dto::ProjectApplyResponse {
            dry_run: true,
            project_id,
            slug: request.config.project.slug.clone(),
            created: existing_project.is_none(),
            git_remote_url,
            finite_toml,
            outputs: output_summaries,
            collaborators: collaborator_summaries,
            invited_emails: Vec::new(),
        })
    }

    fn project_apply_response_from_store(
        &self,
        dry_run: bool,
        git_remote_url: String,
        finite_toml: String,
        outcome: ProjectApplyStoreOutcome,
    ) -> finitesites_proto::dto::ProjectApplyResponse {
        let outputs = outcome
            .outputs
            .iter()
            .map(|output| project_output_summary(&output.record, output.created, &self.config))
            .collect();
        let collaborators = outcome
            .collaborators
            .iter()
            .map(|collaborator| ProjectCollaboratorSummary {
                principal_id: Some(collaborator.record.principal_id.clone()),
                email: collaborator
                    .record
                    .email
                    .clone()
                    .unwrap_or_else(|| String::from("")),
                role: collaborator.record.role.as_str().to_string(),
                created: collaborator.created,
            })
            .collect();
        finitesites_proto::dto::ProjectApplyResponse {
            dry_run,
            project_id: Some(outcome.project.id),
            slug: outcome.project.slug,
            created: outcome.created,
            git_remote_url,
            finite_toml,
            outputs,
            collaborators,
            invited_emails: Vec::new(),
        }
    }

    pub fn remove_project_collaborator(
        &mut self,
        owner_pubkey: &str,
        project_slug: &str,
        collaborator_email: &str,
        now: u64,
    ) -> Result<ProjectCollaboratorRemoveResponse, EngineError> {
        assert!(hex::is_hex32(owner_pubkey));
        finitesites_proto::project_config::validate_project_slug(project_slug)?;
        let email = validate_email(collaborator_email)?;
        let project = self
            .store
            .project_by_slug(project_slug)?
            .ok_or(EngineError::ProjectNotFound)?;
        let owner_principal = self
            .store
            .principal_by_pubkey(owner_pubkey)?
            .ok_or(EngineError::NotAuthorized)?;
        if project.owner_principal_id != owner_principal.id {
            return Err(EngineError::NotAuthorized);
        }
        let removed = self.store.remove_project_collaborator(
            &project.id,
            &owner_principal.id,
            &email,
            now,
        )?;
        Ok(ProjectCollaboratorRemoveResponse {
            project_slug: project.slug,
            email: removed.email,
            removed: removed.removed,
            revoked_git_credentials: removed.revoked_git_credentials,
        })
    }

    pub fn mint_git_credential(
        &mut self,
        actor_pubkey: &str,
        project_slug: &str,
        actor_email: &str,
        git_remote_url: String,
        now: u64,
    ) -> Result<GitAuthResponse, EngineError> {
        assert!(hex::is_hex32(actor_pubkey));
        let email = validate_email(actor_email)?;
        let project = self
            .store
            .project_by_slug(project_slug)?
            .ok_or(EngineError::ProjectNotFound)?;
        if !self.store.has_email_key(&email, actor_pubkey)? {
            return Err(EngineError::NotAuthorized);
        }
        let collaborator = self
            .store
            .active_project_collaborator_by_email(&project.id, &email)?
            .ok_or(EngineError::NotAuthorized)?;
        if collaborator.role == ProjectCollaboratorRole::Viewer {
            return Err(EngineError::NotAuthorized);
        }

        let credential_id = ids::new_id(ids::GIT_CREDENTIAL_ID_PREFIX);
        let password = hex::encode(&ids::random_32());
        let token_hash = hex::encode(&Sha256::digest(password.as_bytes()));
        self.store.create_git_credential(
            &credential_id,
            &project.id,
            &collaborator.principal_id,
            &token_hash,
            None,
            now,
        )?;
        Ok(GitAuthResponse {
            project_slug: project.slug,
            git_remote_url,
            credential_id: credential_id.clone(),
            username: credential_id,
            password,
            expires_at: None,
        })
    }

    pub fn authenticate_git_credential(
        &self,
        username: &str,
        password: &str,
        project_slug: &str,
        now: u64,
    ) -> Result<GitCredentialAuth, EngineError> {
        let credential = self
            .store
            .git_credential_by_id(username)?
            .ok_or(EngineError::NotAuthorized)?;
        let token_hash = hex::encode(&Sha256::digest(password.as_bytes()));
        if credential.token_hash != token_hash {
            return Err(EngineError::NotAuthorized);
        }
        if credential.revoked_at.is_some() {
            return Err(EngineError::NotAuthorized);
        }
        if let Some(expires_at) = credential.expires_at
            && now >= expires_at
        {
            return Err(EngineError::NotAuthorized);
        }
        let project =
            self.store
                .project_by_id(&credential.project_id)?
                .ok_or(StoreError::CorruptState(
                    "git credential references missing project",
                ))?;
        if project.slug != project_slug {
            return Err(EngineError::NotAuthorized);
        }
        let collaborator = self
            .store
            .active_project_collaborator_by_principal(&project.id, &credential.principal_id)?
            .ok_or(EngineError::NotAuthorized)?;
        Ok(GitCredentialAuth {
            project_id: project.id,
            project_slug: project.slug,
            principal_id: collaborator.principal_id,
            actor_agent_key_id: None,
            git_credential_id: credential.id,
            can_push: collaborator.role != ProjectCollaboratorRole::Viewer,
        })
    }

    pub fn record_git_ref_event(
        &mut self,
        auth: &GitCredentialAuth,
        ref_name: &str,
        old_sha: &str,
        new_sha: &str,
        now: u64,
    ) -> Result<(GitRefEventRecord, bool), EngineError> {
        Ok(self.store.record_git_ref_event(
            &auth.project_id,
            ref_name,
            old_sha,
            new_sha,
            &auth.principal_id,
            None,
            &auth.git_credential_id,
            now,
        )?)
    }

    pub fn mark_git_ref_event_deployed(
        &mut self,
        event_id: i64,
        project_output_id: &str,
        version_id: &str,
        now: u64,
    ) -> Result<(), EngineError> {
        Ok(self
            .store
            .mark_git_ref_event_deployed(event_id, project_output_id, version_id, now)?)
    }

    pub fn mark_git_ref_event_ignored(
        &mut self,
        event_id: i64,
        now: u64,
    ) -> Result<(), EngineError> {
        Ok(self.store.mark_git_ref_event_ignored(event_id, now)?)
    }

    pub fn mark_git_ref_event_failed(
        &mut self,
        event_id: i64,
        error: &str,
        now: u64,
    ) -> Result<(), EngineError> {
        Ok(self.store.mark_git_ref_event_failed(event_id, error, now)?)
    }

    pub fn pending_git_ref_events(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<GitRefEventRecord>, EngineError> {
        Ok(self.store.pending_git_ref_events(project_id)?)
    }

    // ---- project output deployment -----------------------------------------

    pub fn commit_project_output_version(
        &mut self,
        site_id: &str,
        files: Vec<(ManifestFile, Vec<u8>)>,
        spa_fallback: bool,
        now: u64,
    ) -> Result<FinalizeOutcome, EngineError> {
        self.commit_project_output_version_for_git_event(site_id, None, files, spa_fallback, now)
    }

    pub fn commit_project_output_version_for_git_event(
        &mut self,
        site_id: &str,
        git_ref_event_id: Option<i64>,
        files: Vec<(ManifestFile, Vec<u8>)>,
        spa_fallback: bool,
        now: u64,
    ) -> Result<FinalizeOutcome, EngineError> {
        let site = self
            .store
            .site_by_id(site_id)?
            .ok_or(EngineError::SiteNotFound)?;
        if let Some(event_id) = git_ref_event_id
            && let Some(version) = self.store.version_by_git_ref_event_id(event_id)?
        {
            return self.finalize_outcome(
                &site.id,
                &version.version_id,
                version.version_number,
                version.path_count,
                version.total_bytes,
            );
        }
        if site.status == SiteStatus::Disabled || site.status == SiteStatus::Deleted {
            return Err(EngineError::Conflict("site is disabled"));
        }
        if !self.store.has_publish_access(&site.owner_pubkey, now)? {
            return Err(EngineError::NotAllowlisted);
        }
        if site.kind == SiteKind::App {
            return Err(EngineError::Conflict("project output site is an app"));
        }
        let manifest = PublishManifest {
            files: files.iter().map(|(file, _)| file.clone()).collect(),
        };
        manifest.validate()?;
        if spa_fallback {
            let has_index = manifest.files.iter().any(|file| file.path == "/index.html");
            if !has_index {
                return Err(EngineError::Validation(
                    "spa manifests must include /index.html",
                ));
            }
        }

        let publish_id = ids::new_id(ids::PUBLISH_ID_PREFIX);
        self.store.create_publish(
            &publish_id,
            &site.id,
            &manifest.files,
            spa_fallback,
            None,
            now,
        )?;
        // Bounded by MAX_MANIFEST_FILES, validated above.
        for (file, bytes) in &files {
            if bytes.len() as u64 != file.size {
                return Err(EngineError::Validation("blob size does not match manifest"));
            }
            let actual = hex::encode(&Sha256::digest(bytes));
            if actual != file.sha256 {
                return Err(EngineError::Validation("blob hash does not match manifest"));
            }
            self.blobs.put(&file.sha256, bytes, MAX_FILE_BYTES)?;
            self.store.record_blob(&file.sha256, file.size, now)?;
        }
        let manifest_sha256 = manifest.digest();
        let version_id = ids::new_id(ids::VERSION_ID_PREFIX);
        let finalized = match self.store.finalize_publish_for_git_event(
            &publish_id,
            &version_id,
            &manifest_sha256,
            git_ref_event_id,
            now,
        ) {
            Ok(finalized) => finalized,
            Err(StoreError::Conflict("publish has missing blobs")) => {
                return Err(EngineError::Conflict("publish has missing blobs"));
            }
            Err(other) => return Err(other.into()),
        };
        self.finalize_outcome(
            &site.id,
            &version_id,
            finalized.version_number,
            finalized.path_count,
            finalized.total_bytes,
        )
    }

    /// Build the outcome from committed state. Re-reads the site so app
    /// fields (kind, port) reflect what finalize just wrote.
    fn finalize_outcome(
        &self,
        site_id: &str,
        version_id: &str,
        version_number: u32,
        path_count: u32,
        total_bytes: u64,
    ) -> Result<FinalizeOutcome, EngineError> {
        let site = self
            .store
            .site_by_id(site_id)?
            .ok_or(StoreError::CorruptState("site missing after finalize"))?;
        let app = if site.kind == SiteKind::App {
            let port = site
                .app_port
                .ok_or(StoreError::CorruptState("app site has no port"))?;
            let start_command = site
                .active_version_start
                .clone()
                .ok_or(StoreError::CorruptState("app version has no start command"))?;
            let (bundle_sha256, _size) = self
                .store
                .version_file(version_id, APP_BUNDLE_PATH)?
                .ok_or(StoreError::CorruptState("app version has no bundle"))?;
            Some(AppDeploy {
                site_id: site.id.clone(),
                version_id: version_id.to_string(),
                bundle_sha256,
                start_command,
                port,
            })
        } else {
            None
        };
        Ok(FinalizeOutcome {
            site_id: site.id.clone(),
            name: site.name.clone(),
            url: self.config.site_url(&site.name),
            version_id: version_id.to_string(),
            version_number,
            path_count,
            total_bytes,
            app,
        })
    }

    // ---- sharing -------------------------------------------------------------

    /// Update visibility and the shared-email ACL. Project collaborators edit
    /// content through git; output visibility remains owner-controlled.
    pub fn set_sharing(
        &mut self,
        actor_pubkey: &str,
        name: &str,
        request: &SharingRequest,
        now: u64,
    ) -> Result<SharingResponse, EngineError> {
        let site = self
            .store
            .site_by_name(name)?
            .ok_or(EngineError::SiteNotFound)?;
        if actor_pubkey != site.owner_pubkey {
            return Err(EngineError::NotAuthorized);
        }
        let adds = request.add_emails.len() + request.remove_emails.len();
        if adds > MAX_EMAILS_PER_SHARING_REQUEST as usize {
            return Err(EngineError::Validation("too many emails in one request"));
        }

        let target_visibility = match request.visibility.as_deref() {
            None => None,
            Some(raw) => {
                let parsed =
                    Visibility::parse(raw).ok_or(EngineError::Validation("unknown visibility"))?;
                if parsed == Visibility::Public && !request.confirm_public {
                    // The agent must surface the public-site warning to the
                    // human before the server will make anything public.
                    return Err(EngineError::Validation(
                        "making a site public requires confirm_public",
                    ));
                }
                Some(parsed)
            }
        };

        // Bounded by MAX_EMAILS_PER_SHARING_REQUEST, checked above.
        for email in &request.remove_emails {
            let normalized = validate_email(email)?;
            self.store.remove_share(&site.id, &normalized)?;
        }
        for email in &request.add_emails {
            let normalized = validate_email(email)?;
            if self.store.count_shares(&site.id)? >= MAX_SHARES_PER_SITE {
                return Err(EngineError::TooManyShares);
            }
            self.store.add_share(&site.id, &normalized, now)?;
        }
        if let Some(visibility) = target_visibility {
            self.store.set_visibility(&site.id, visibility, now)?;
        }
        self.store
            .record_event(Some(&site.id), "sharing_updated", Some(actor_pubkey), now)?;

        let refreshed = self
            .store
            .site_by_id(&site.id)?
            .ok_or(StoreError::CorruptState(
                "site missing after sharing update",
            ))?;
        Ok(SharingResponse {
            visibility: refreshed.visibility.as_str().to_string(),
            shared_emails: self.store.shares(&site.id)?,
            invited_emails: Vec::new(),
        })
    }

    // ---- email-keyed publishing ------------------------------------------

    pub fn request_email_login(
        &mut self,
        email: &str,
        now: u64,
    ) -> Result<EmailLoginToken, EngineError> {
        let normalized = validate_email(email)?;
        let token = hex::encode(&ids::random_32());
        let token_hash = hex::encode(&Sha256::digest(token.as_bytes()));
        self.store.create_email_login_token(
            &token_hash,
            &normalized,
            now + LOGIN_TOKEN_TTL_SECONDS,
            now,
        )?;
        Ok(EmailLoginToken {
            email: normalized,
            token,
        })
    }

    pub fn redeem_email_login(
        &mut self,
        actor_pubkey: &str,
        email: &str,
        token: &str,
        now: u64,
    ) -> Result<String, EngineError> {
        if !hex::is_hex32(actor_pubkey) {
            return Err(EngineError::NotAuthorized);
        }
        if !hex::is_hex32(token) {
            return Err(EngineError::Validation("malformed token"));
        }
        let normalized = validate_email(email)?;
        let token_hash = hex::encode(&Sha256::digest(token.as_bytes()));
        let token_email = match self.store.redeem_email_login_token(&token_hash, now) {
            Ok(email) => email,
            Err(StoreError::NotFound(_)) => {
                return Err(EngineError::Validation("unknown or expired email token"));
            }
            Err(StoreError::Conflict(_)) => {
                return Err(EngineError::Validation("unknown or expired email token"));
            }
            Err(other) => return Err(other.into()),
        };
        if token_email != normalized {
            return Err(EngineError::Validation("email token does not match email"));
        }
        let already_present = self.store.has_email_key(&normalized, actor_pubkey)?;
        if !already_present && self.store.count_email_keys(&normalized)? >= MAX_EMAIL_KEYS_PER_EMAIL
        {
            return Err(EngineError::TooManyEmailKeys);
        }
        self.store.add_email_key(&normalized, actor_pubkey, now)?;
        Ok(normalized)
    }

    // ---- listing / status ------------------------------------------------------

    pub fn list_sites(&self, owner_pubkey: &str) -> Result<Vec<SiteSummary>, EngineError> {
        let sites = self.store.sites_by_owner(owner_pubkey)?;
        let mut out = Vec::with_capacity(sites.len());
        // Bounded by MAX_SITES_PER_OWNER.
        for site in &sites {
            out.push(self.site_summary(site)?);
        }
        Ok(out)
    }

    pub fn site_status(&self, actor_pubkey: &str, name: &str) -> Result<SiteSummary, EngineError> {
        let site = self
            .store
            .site_by_name(name)?
            .ok_or(EngineError::SiteNotFound)?;
        if actor_pubkey != site.owner_pubkey {
            return Err(EngineError::NotAuthorized);
        }
        self.site_summary(&site)
    }

    fn site_summary(&self, site: &SiteRecord) -> Result<SiteSummary, EngineError> {
        Ok(SiteSummary {
            site_id: site.id.clone(),
            name: site.name.clone(),
            url: self.config.site_url(&site.name),
            status: site.status.as_str().to_string(),
            visibility: site.visibility.as_str().to_string(),
            kind: site.kind.as_str().to_string(),
            active_version: site.active_version_number,
            shared_emails: self.store.shares(&site.id)?,
        })
    }

    // ---- serving ---------------------------------------------------------------

    pub fn resolve_site(&self, name: &str) -> Result<Option<SiteRecord>, EngineError> {
        if names::validate_site_name(name).is_err() {
            return Ok(None);
        }
        Ok(self.store.site_by_name(name)?)
    }

    /// May this request see the site content? Re-checks the share table on
    /// every request so revoking an email takes effect immediately.
    pub fn view_access(
        &self,
        site: &SiteRecord,
        cookie_value: Option<&str>,
        now: u64,
    ) -> Result<ViewAccess, EngineError> {
        if site.status != SiteStatus::Published {
            // Unpublished/disabled sites have no content; the caller renders
            // a placeholder regardless of access.
            return Ok(ViewAccess::NeedsLogin);
        }
        match site.visibility {
            Visibility::Public => Ok(ViewAccess::Allowed),
            Visibility::Shared | Visibility::Private => {
                let Some(raw_cookie) = cookie_value else {
                    return Ok(ViewAccess::NeedsLogin);
                };
                let Some(cookie) =
                    ViewerCookie::verify(&self.cookie_secret, raw_cookie, &site.id, now)
                else {
                    return Ok(ViewAccess::NeedsLogin);
                };
                if self.store.is_email_shared(&site.id, &cookie.email)? {
                    Ok(ViewAccess::Allowed)
                } else {
                    Ok(ViewAccess::NeedsLogin)
                }
            }
        }
    }

    /// Look up the blob for a request path in the site's active version.
    /// `/` and directory-style paths fall back to `index.html`. The returned
    /// path is the manifest path that matched (callers derive content types
    /// from it, not from the request).
    pub fn lookup_file(
        &self,
        site: &SiteRecord,
        request_path: &str,
    ) -> Result<Option<FoundFile>, EngineError> {
        assert!(request_path.starts_with('/'));
        let Some(version_id) = site.active_version_id.as_deref() else {
            return Ok(None);
        };
        let candidate = if request_path.ends_with('/') {
            format!("{request_path}index.html")
        } else {
            request_path.to_string()
        };
        if let Some((sha256, size)) = self.store.version_file(version_id, &candidate)? {
            return Ok(Some(FoundFile {
                path: candidate,
                sha256,
                size,
            }));
        }
        // `/docs` also tries `/docs/index.html` so folder links work.
        if !request_path.ends_with('/') {
            let with_index = format!("{request_path}/index.html");
            if let Some((sha256, size)) = self.store.version_file(version_id, &with_index)? {
                return Ok(Some(FoundFile {
                    path: with_index,
                    sha256,
                    size,
                }));
            }
        }
        // SPA versions route unknown paths to the app shell so client-side
        // routers handle deep links and refreshes.
        if site.active_version_spa
            && let Some((sha256, size)) = self.store.version_file(version_id, "/index.html")?
        {
            return Ok(Some(FoundFile {
                path: "/index.html".to_string(),
                sha256,
                size,
            }));
        }
        Ok(None)
    }

    /// Exact active-version lookup with no folder or SPA fallback. Use this
    /// when the distinction between a path the user authored and a path the
    /// platform can synthesize matters.
    pub fn lookup_exact_file(
        &self,
        site: &SiteRecord,
        request_path: &str,
    ) -> Result<Option<FoundFile>, EngineError> {
        assert!(request_path.starts_with('/'));
        let Some(version_id) = site.active_version_id.as_deref() else {
            return Ok(None);
        };
        Ok(self
            .store
            .version_file(version_id, request_path)?
            .map(|(sha256, size)| FoundFile {
                path: request_path.to_string(),
                sha256,
                size,
            }))
    }

    /// A site gets platform-authored agent instructions only when it is a
    /// Project Output and the user did not publish their own `/llms.txt`.
    pub fn should_generate_llms_txt(&self, site: &SiteRecord) -> Result<bool, EngineError> {
        if site.status != SiteStatus::Published {
            return Ok(false);
        }
        if site.kind != SiteKind::Static {
            return Ok(false);
        }
        if self.lookup_exact_file(site, "/llms.txt")?.is_some() {
            return Ok(false);
        }
        Ok(self.store.project_output_by_site_id(&site.id)?.is_some())
    }

    pub fn project_output_for_site(
        &self,
        site: &SiteRecord,
    ) -> Result<Option<(ProjectRecord, ProjectOutputRecord)>, EngineError> {
        Ok(self.store.project_output_by_site_id(&site.id)?)
    }

    pub fn project_outputs(
        &self,
        project_id: &str,
    ) -> Result<Vec<ProjectOutputRecord>, EngineError> {
        Ok(self.store.project_outputs(project_id)?)
    }

    /// The site's custom 404 page, if it published one.
    pub fn lookup_not_found_page(
        &self,
        site: &SiteRecord,
    ) -> Result<Option<FoundFile>, EngineError> {
        let Some(version_id) = site.active_version_id.as_deref() else {
            return Ok(None);
        };
        Ok(self
            .store
            .version_file(version_id, "/404.html")?
            .map(|(sha256, size)| FoundFile {
                path: "/404.html".to_string(),
                sha256,
                size,
            }))
    }

    pub fn read_blob(&self, sha256: &str) -> Result<Vec<u8>, EngineError> {
        Ok(self.blobs.get(sha256)?)
    }

    /// Filesystem path of a blob, for streaming large bundles.
    pub fn blob_file_path(&self, sha256: &str) -> std::path::PathBuf {
        self.blobs.file_path(sha256)
    }

    /// All app sites with an active version, for supervisor reconciliation
    /// at startup. Returns the deploy info for each.
    pub fn app_deploys(&self) -> Result<Vec<AppDeploy>, EngineError> {
        let sites = self.store.app_sites()?;
        let mut out = Vec::with_capacity(sites.len());
        // Bounded by the number of app sites, which is bounded by the port range.
        for site in sites {
            if let Some(deploy) = self.deploy_for_site(&site)? {
                out.push(deploy);
            }
        }
        Ok(out)
    }

    /// Deploy info for one app site by id, for waking it on a request.
    pub fn app_deploy_for(&self, site_id: &str) -> Result<Option<AppDeploy>, EngineError> {
        let Some(site) = self.store.site_by_id(site_id)? else {
            return Ok(None);
        };
        self.deploy_for_site(&site)
    }

    fn deploy_for_site(&self, site: &SiteRecord) -> Result<Option<AppDeploy>, EngineError> {
        if site.kind != SiteKind::App {
            return Ok(None);
        }
        let Some(version_id) = site.active_version_id.as_deref() else {
            return Ok(None);
        };
        let (port, start_command) = match (site.app_port, site.active_version_start.clone()) {
            (Some(port), Some(start)) => (port, start),
            _ => return Err(StoreError::CorruptState("app site missing port or start").into()),
        };
        let (bundle_sha256, _size) = self
            .store
            .version_file(version_id, APP_BUNDLE_PATH)?
            .ok_or(StoreError::CorruptState("app version has no bundle"))?;
        Ok(Some(AppDeploy {
            site_id: site.id.clone(),
            version_id: version_id.to_string(),
            bundle_sha256,
            start_command,
            port,
        }))
    }

    // ---- magic-link login --------------------------------------------------------

    /// Issue a login token if (and only if) the email is shared on the site.
    /// Returns `None` otherwise so callers can answer generically and not
    /// leak which emails have access.
    pub fn request_login(
        &mut self,
        name: &str,
        email: &str,
        now: u64,
    ) -> Result<Option<LoginLink>, EngineError> {
        let Some(site) = self.store.site_by_name(name)? else {
            return Ok(None);
        };
        let normalized = match validate_email(email) {
            Ok(normalized) => normalized,
            Err(_) => return Ok(None),
        };
        if site.visibility != Visibility::Shared {
            return Ok(None);
        }
        if !self.store.is_email_shared(&site.id, &normalized)? {
            return Ok(None);
        }

        let token = hex::encode(&ids::random_32());
        let token_hash = hex::encode(&Sha256::digest(token.as_bytes()));
        self.store.create_login_token(
            &token_hash,
            &site.id,
            &normalized,
            now + LOGIN_TOKEN_TTL_SECONDS,
            now,
        )?;
        let url = format!(
            "{}_finite/auth?token={token}",
            self.config.site_url(&site.name)
        );
        Ok(Some(LoginLink {
            site_name: site.name,
            email: normalized,
            url,
        }))
    }

    /// Redeem a magic-link token; returns the site and a viewer cookie value.
    pub fn redeem_login(
        &mut self,
        token: &str,
        now: u64,
    ) -> Result<(SiteRecord, String), EngineError> {
        if !hex::is_hex32(token) {
            return Err(EngineError::Validation("malformed token"));
        }
        let token_hash = hex::encode(&Sha256::digest(token.as_bytes()));
        let (site_id, email) = match self.store.redeem_login_token(&token_hash, now) {
            Ok(redeemed) => redeemed,
            Err(StoreError::NotFound(_)) => {
                return Err(EngineError::Validation("unknown or expired link"));
            }
            Err(StoreError::Conflict(_)) => {
                return Err(EngineError::Validation("unknown or expired link"));
            }
            Err(other) => return Err(other.into()),
        };
        let site = self
            .store
            .site_by_id(&site_id)?
            .ok_or(StoreError::CorruptState(
                "login token references missing site",
            ))?;
        let cookie = ViewerCookie {
            site_id,
            email,
            expires_at: now + VIEWER_COOKIE_TTL_SECONDS,
        }
        .sign(&self.cookie_secret);
        Ok((site, cookie))
    }
}

fn output_apply_inputs(request: &ProjectApplyRequest) -> Vec<ProjectOutputApply> {
    let mut outputs = Vec::with_capacity(request.config.outputs.len());
    // Bounded by ProjectConfig::validate.
    for (output_id, output) in &request.config.outputs {
        outputs.push(ProjectOutputApply {
            output_id: output_id.clone(),
            kind: output.kind.as_str().to_string(),
            site_name: output.site_name.clone(),
            branch: output.branch.clone(),
            path: output.path.clone(),
            spa: output.spa,
        });
    }
    outputs
}

fn collaborator_apply_inputs(
    request: &ProjectApplyRequest,
) -> Result<Vec<ProjectCollaboratorApply>, EngineError> {
    if request.collaborators.len() > MAX_PROJECT_COLLABORATORS as usize {
        return Err(EngineError::TooManyProjectCollaborators);
    }
    let mut collaborators = Vec::with_capacity(request.collaborators.len());
    // Bounded by MAX_PROJECT_COLLABORATORS above.
    for collaborator in &request.collaborators {
        let email = validate_email(&collaborator.email)?;
        let role = match ProjectCollaboratorRole::parse(&collaborator.role) {
            Ok(ProjectCollaboratorRole::Owner) => {
                return Err(EngineError::Validation(
                    "owner role is assigned by project ownership",
                ));
            }
            Ok(role) => role,
            Err(StoreError::Conflict(_)) => {
                return Err(EngineError::Validation(
                    "project collaborator role must be editor or viewer",
                ));
            }
            Err(error) => return Err(EngineError::Store(error)),
        };
        collaborators.push(ProjectCollaboratorApply { email, role });
    }
    Ok(collaborators)
}

fn project_output_summary(
    record: &ProjectOutputRecord,
    created: bool,
    config: &EngineConfig,
) -> ProjectOutputSummary {
    ProjectOutputSummary {
        output_id: record.output_id.clone(),
        kind: record.kind.clone(),
        site_name: record.site_name.clone(),
        site_id: Some(record.site_id.clone()),
        site_url: config.site_url(&record.site_name),
        branch: record.branch.clone(),
        path: record.path.clone(),
        spa: record.spa,
        created,
    }
}

#[cfg(test)]
mod tests;
