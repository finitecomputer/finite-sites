//! Control-plane and serving logic for Finite Sites.
//!
//! The engine owns every decision: who may claim, what a publish may
//! contain, who may view a site. The store persists, the blob store holds
//! bytes, and the HTTP layer above translates outcomes into responses.

mod cookie;
mod email;

pub use cookie::ViewerCookie;
pub use email::validate_email;

use thiserror::Error;

use finitesites_blob::{BlobError, BlobStore};
use finitesites_proto::dto::{
    EditorsRequest, EditorsResponse, SharingRequest, SharingResponse, SiteSummary,
    SourceSnapshotInfo, SourceSnapshotRequest,
};
use finitesites_proto::limits::{
    LOGIN_TOKEN_TTL_SECONDS, MAX_APP_BUNDLE_BYTES, MAX_EDITORS_PER_SITE, MAX_EMAIL_KEYS_PER_EMAIL,
    MAX_EMAILS_PER_SHARING_REQUEST, MAX_FILE_BYTES, MAX_SHARES_PER_SITE, MAX_SITES_PER_OWNER,
    MAX_SOURCE_SNAPSHOT_BYTES, MAX_START_COMMAND_BYTES, VIEWER_COOKIE_TTL_SECONDS,
};
use finitesites_proto::manifest::APP_BUNDLE_PATH;
use finitesites_proto::{ProtoError, PublishManifest, hex, ids, names};
use finitesites_store::{
    PublishRecord, PublishStatus, SiteKind, SiteRecord, SiteStatus, SourceSnapshotRecord, Store,
    StoreError, Visibility,
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
    #[error("publish not found")]
    PublishNotFound,
    #[error("signer is not authorized for this site")]
    NotAuthorized,
    #[error("too many sites for this owner")]
    TooManySites,
    #[error("too many shared emails for this site")]
    TooManyShares,
    #[error("too many editors for this site")]
    TooManyEditors,
    #[error("too many active keys for this email")]
    TooManyEmailKeys,
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
pub struct ClaimOutcome {
    pub site: SiteRecord,
    pub url: String,
    pub already_claimed: bool,
}

#[derive(Debug)]
pub struct BeginPublishOutcome {
    pub publish_id: String,
    pub missing: Vec<String>,
}

#[derive(Debug)]
pub struct FinalizeOutcome {
    pub site_id: String,
    pub name: String,
    pub url: String,
    pub version_number: u32,
    pub path_count: u32,
    pub total_bytes: u64,
    pub source: Option<SourceSnapshotInfo>,
    /// Set for app sites: what the supervisor needs to (re)deploy.
    pub app: Option<AppDeploy>,
}

#[derive(Debug, Clone)]
pub struct EmailLoginToken {
    pub email: String,
    pub token: String,
}

#[derive(Debug, Clone)]
pub struct SourceSnapshotDownload {
    pub version_number: u32,
    pub sha256: String,
    pub size: u64,
    pub bytes: Vec<u8>,
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

    // ---- claims ------------------------------------------------------------

    /// Claim a site name for a publishing-granted owner, registering the per-site
    /// signing key. Idempotent for the same (owner, name, site key) triple.
    pub fn claim(
        &mut self,
        owner_pubkey: &str,
        name: &str,
        site_pubkey: &str,
        now: u64,
    ) -> Result<ClaimOutcome, EngineError> {
        self.claim_with_owner_email(owner_pubkey, name, site_pubkey, None, now)
    }

    pub fn claim_with_owner_email(
        &mut self,
        owner_pubkey: &str,
        name: &str,
        site_pubkey: &str,
        owner_email: Option<&str>,
        now: u64,
    ) -> Result<ClaimOutcome, EngineError> {
        assert!(hex::is_hex32(owner_pubkey));
        if !hex::is_hex32(site_pubkey) {
            return Err(EngineError::Validation("site_pubkey must be 32-byte hex"));
        }
        let normalized_owner_email = match owner_email {
            Some(email) => Some(validate_email(email)?),
            None => None,
        };
        if !self.store.has_publish_access(owner_pubkey, now)? {
            return Err(EngineError::NotAllowlisted);
        }
        names::validate_site_name(name)?;

        if let Some(existing) = self.store.site_by_name(name)? {
            let same_owner_and_key =
                existing.owner_pubkey == owner_pubkey && existing.site_pubkey == site_pubkey;
            if same_owner_and_key {
                if let Some(email) = normalized_owner_email.as_deref() {
                    self.set_owner_email_for_site(&existing, owner_pubkey, email, now)?;
                }
                let refreshed =
                    self.store
                        .site_by_id(&existing.id)?
                        .ok_or(StoreError::CorruptState(
                            "claimed site missing after owner email update",
                        ))?;
                let url = self.config.site_url(name);
                return Ok(ClaimOutcome {
                    site: refreshed,
                    url,
                    already_claimed: true,
                });
            }
            return Err(EngineError::NameTaken);
        }
        if let Some(existing) = self.store.site_by_site_pubkey(site_pubkey)? {
            // One signing key per site keeps revocation meaningful.
            let _ = existing;
            return Err(EngineError::Conflict(
                "site key already used by another site",
            ));
        }
        if self.store.count_sites_by_owner(owner_pubkey)? >= MAX_SITES_PER_OWNER {
            return Err(EngineError::TooManySites);
        }

        let site_id = ids::new_id(ids::SITE_ID_PREFIX);
        let claim_id = ids::new_id(ids::CLAIM_ID_PREFIX);
        let created = self.store.create_site_with_claim_and_owner_email(
            &site_id,
            &claim_id,
            name,
            owner_pubkey,
            site_pubkey,
            normalized_owner_email.as_deref(),
            now,
        );
        match created {
            Ok(()) => {}
            Err(StoreError::Conflict("name already claimed")) => {
                return Err(EngineError::NameTaken);
            }
            Err(other) => return Err(other.into()),
        }

        let site = self
            .store
            .site_by_id(&site_id)?
            .ok_or(StoreError::CorruptState(
                "claimed site missing after insert",
            ))?;
        assert!(site.name == name && site.owner_pubkey == owner_pubkey);
        Ok(ClaimOutcome {
            url: self.config.site_url(&site.name),
            site,
            already_claimed: false,
        })
    }

    fn set_owner_email_for_site(
        &mut self,
        site: &SiteRecord,
        actor_pubkey: &str,
        owner_email: &str,
        now: u64,
    ) -> Result<(), EngineError> {
        if actor_pubkey != site.owner_pubkey && actor_pubkey != site.site_pubkey {
            return Err(EngineError::NotAuthorized);
        }
        match site.owner_email.as_deref() {
            Some(existing) if existing != owner_email => {
                return Err(EngineError::Conflict("owner email already set"));
            }
            Some(_) => return Ok(()),
            None => {}
        }
        self.store.set_owner_email(&site.id, owner_email, now)?;
        self.store
            .record_event(Some(&site.id), "owner_email_set", Some(actor_pubkey), now)?;
        Ok(())
    }

    // ---- publishing ----------------------------------------------------------

    /// Start a publish session: validate the manifest and report which blobs
    /// the caller must upload. `spa_fallback` marks the resulting version as
    /// a single-page app (lookup misses serve `/index.html`).
    pub fn begin_publish(
        &mut self,
        site_pubkey: &str,
        manifest: &PublishManifest,
        spa_fallback: bool,
        start_command: Option<&str>,
        now: u64,
    ) -> Result<BeginPublishOutcome, EngineError> {
        self.begin_publish_with_source(
            site_pubkey,
            manifest,
            spa_fallback,
            start_command,
            None,
            now,
        )
    }

    pub fn begin_publish_with_source(
        &mut self,
        site_pubkey: &str,
        manifest: &PublishManifest,
        spa_fallback: bool,
        start_command: Option<&str>,
        source: Option<&SourceSnapshotRequest>,
        now: u64,
    ) -> Result<BeginPublishOutcome, EngineError> {
        let site = self.authorize_site_key(site_pubkey, now)?;
        self.begin_publish_for_site(
            &site,
            site_pubkey,
            None,
            manifest,
            spa_fallback,
            start_command,
            source,
            now,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn begin_publish_as_email(
        &mut self,
        actor_pubkey: &str,
        actor_email: &str,
        name: &str,
        manifest: &PublishManifest,
        spa_fallback: bool,
        start_command: Option<&str>,
        source: Option<&SourceSnapshotRequest>,
        now: u64,
    ) -> Result<BeginPublishOutcome, EngineError> {
        let (site, email) = self.authorize_email_publish(name, actor_pubkey, actor_email, now)?;
        self.begin_publish_for_site(
            &site,
            actor_pubkey,
            Some(&email),
            manifest,
            spa_fallback,
            start_command,
            source,
            now,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn begin_publish_for_site(
        &mut self,
        site: &SiteRecord,
        actor_pubkey: &str,
        actor_email: Option<&str>,
        manifest: &PublishManifest,
        spa_fallback: bool,
        start_command: Option<&str>,
        source: Option<&SourceSnapshotRequest>,
        now: u64,
    ) -> Result<BeginPublishOutcome, EngineError> {
        let source_record = match source {
            Some(source) => Some(validate_source_snapshot(source)?),
            None => None,
        };
        if let Some(command) = start_command {
            validate_start_command(command)?;
            if spa_fallback {
                return Err(EngineError::Validation("apps do not take the spa flag"));
            }
            let is_single_bundle =
                manifest.files.len() == 1 && manifest.files[0].path == APP_BUNDLE_PATH;
            if !is_single_bundle {
                return Err(EngineError::Validation(
                    "app manifests must contain exactly /app.tar.gz",
                ));
            }
            // App bundles are one tar.gz far larger than any static asset;
            // validate against the bundle ceiling, not the static one.
            manifest.validate_with_max_file(MAX_APP_BUNDLE_BYTES)?;
        } else {
            manifest.validate()?;
        }
        // A site's kind is fixed by its first publish: the supervisor and
        // router state machines must not flip shape under a live site.
        if site.active_version_id.is_some() {
            let publishing_app = start_command.is_some();
            let site_is_app = site.kind == SiteKind::App;
            if publishing_app != site_is_app {
                return Err(EngineError::Conflict(
                    "site kind cannot change between static and app",
                ));
            }
        }
        if spa_fallback {
            let has_index = manifest.files.iter().any(|f| f.path == "/index.html");
            if !has_index {
                // An SPA without a root index has nothing to fall back to.
                return Err(EngineError::Validation(
                    "spa manifests must include /index.html",
                ));
            }
        }

        let publish_id = ids::new_id(ids::PUBLISH_ID_PREFIX);
        self.store.create_publish_with_actor(
            &publish_id,
            &site.id,
            &manifest.files,
            spa_fallback,
            start_command,
            Some(actor_pubkey),
            actor_email,
            source_record.as_ref(),
            now,
        )?;
        let mut hashes: Vec<&str> = manifest.files.iter().map(|f| f.sha256.as_str()).collect();
        if let Some(source) = source_record.as_ref() {
            hashes.push(source.sha256.as_str());
        }
        let missing = self.store.missing_blobs(&hashes)?;
        assert!(missing.len() <= hashes.len());
        Ok(BeginPublishOutcome {
            publish_id,
            missing,
        })
    }

    /// Store one blob for a pending publish. Idempotent per blob.
    pub fn upload_blob(
        &mut self,
        site_pubkey: &str,
        publish_id: &str,
        sha256: &str,
        bytes: &[u8],
        now: u64,
    ) -> Result<(), EngineError> {
        self.upload_blob_as_actor(site_pubkey, publish_id, sha256, bytes, now)
    }

    pub fn upload_blob_as_actor(
        &mut self,
        actor_pubkey: &str,
        publish_id: &str,
        sha256: &str,
        bytes: &[u8],
        now: u64,
    ) -> Result<(), EngineError> {
        if !hex::is_hex32(sha256) {
            return Err(EngineError::Validation(
                "sha256 must be 64 lowercase hex chars",
            ));
        }
        let (_site, publish) = self.authorize_publish_actor(actor_pubkey, publish_id, now)?;
        if publish.status != PublishStatus::Pending {
            return Err(EngineError::Conflict("publish is not pending"));
        }
        let expected_size = match self.store.publish_file_by_hash(publish_id, sha256)? {
            Some(size) => size,
            None => self
                .store
                .publish_source_by_hash(publish_id, sha256)?
                .ok_or(EngineError::Validation("blob is not part of this publish"))?,
        };
        if bytes.len() as u64 != expected_size {
            return Err(EngineError::Validation("blob size does not match manifest"));
        }

        // App bundles are the one blob class allowed past the static file
        // ceiling; everything else keeps the 25 MiB cap.
        let is_source_blob = self
            .store
            .publish_source_by_hash(publish_id, sha256)?
            .is_some();
        let max_bytes = if is_source_blob {
            MAX_SOURCE_SNAPSHOT_BYTES
        } else if expected_size > finitesites_proto::limits::MAX_FILE_BYTES {
            MAX_APP_BUNDLE_BYTES
        } else {
            MAX_FILE_BYTES
        };
        // Write bytes first, then record the blob row: a crash between the
        // two leaves an unreferenced file, never a row pointing at nothing.
        self.blobs.put(sha256, bytes, max_bytes)?;
        self.store.record_blob(sha256, bytes.len() as u64, now)?;
        assert!(self.blobs.has(sha256));
        Ok(())
    }

    /// Finalize a publish into an immutable version. Replaying a finalize on
    /// an already-finalized publish returns the same version.
    pub fn finalize_publish(
        &mut self,
        site_pubkey: &str,
        publish_id: &str,
        now: u64,
    ) -> Result<FinalizeOutcome, EngineError> {
        self.finalize_publish_as_actor(site_pubkey, publish_id, now)
    }

    pub fn finalize_publish_as_actor(
        &mut self,
        actor_pubkey: &str,
        publish_id: &str,
        now: u64,
    ) -> Result<FinalizeOutcome, EngineError> {
        let (site, publish) = self.authorize_publish_actor(actor_pubkey, publish_id, now)?;

        if publish.status == PublishStatus::Finalized {
            let version_id = publish
                .version_id
                .as_deref()
                .ok_or(StoreError::CorruptState("finalized publish has no version"))?;
            let version = self
                .store
                .version_by_id(version_id)?
                .ok_or(StoreError::CorruptState(
                    "finalized publish version missing",
                ))?;
            return self.finalize_outcome(
                &site.site_pubkey,
                version_id,
                version.version_number,
                version.path_count,
                version.total_bytes,
                version.source,
            );
        }
        if publish.status == PublishStatus::Aborted {
            return Err(EngineError::Conflict("publish was aborted"));
        }

        let files = self.store.publish_files(publish_id)?;
        let manifest = PublishManifest { files };
        let manifest_sha256 = manifest.digest();
        let version_id = ids::new_id(ids::VERSION_ID_PREFIX);
        let finalized =
            match self
                .store
                .finalize_publish(publish_id, &version_id, &manifest_sha256, now)
            {
                Ok(finalized) => finalized,
                Err(StoreError::Conflict("publish has missing blobs")) => {
                    return Err(EngineError::Conflict("publish has missing blobs"));
                }
                Err(StoreError::Conflict("publish has missing source blob")) => {
                    return Err(EngineError::Conflict("publish has missing source blob"));
                }
                Err(other) => return Err(other.into()),
            };
        assert!(finalized.version_number >= 1);
        self.finalize_outcome(
            &site.site_pubkey,
            &version_id,
            finalized.version_number,
            finalized.path_count,
            finalized.total_bytes,
            finalized.source,
        )
    }

    /// Build the outcome from committed state. Re-reads the site so app
    /// fields (kind, port) reflect what finalize just wrote.
    fn finalize_outcome(
        &self,
        site_pubkey: &str,
        version_id: &str,
        version_number: u32,
        path_count: u32,
        total_bytes: u64,
        source: Option<SourceSnapshotRecord>,
    ) -> Result<FinalizeOutcome, EngineError> {
        let site = self
            .store
            .site_by_site_pubkey(site_pubkey)?
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
            version_number,
            path_count,
            total_bytes,
            source: source.map(|source| SourceSnapshotInfo {
                version_number,
                sha256: source.sha256,
                size: source.size,
            }),
            app,
        })
    }

    // ---- sharing -------------------------------------------------------------

    /// Update visibility and the shared-email ACL. The owner key and the
    /// site key are both authorized: sharing is an agent-driven action.
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
        let actor_is_authorized =
            actor_pubkey == site.owner_pubkey || actor_pubkey == site.site_pubkey;
        if !actor_is_authorized {
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

    pub fn set_owner_email(
        &mut self,
        actor_pubkey: &str,
        name: &str,
        owner_email: &str,
        now: u64,
    ) -> Result<EditorsResponse, EngineError> {
        let normalized = validate_email(owner_email)?;
        let site = self
            .store
            .site_by_name(name)?
            .ok_or(EngineError::SiteNotFound)?;
        self.set_owner_email_for_site(&site, actor_pubkey, &normalized, now)?;
        let refreshed = self
            .store
            .site_by_id(&site.id)?
            .ok_or(StoreError::CorruptState(
                "site missing after owner email update",
            ))?;
        Ok(EditorsResponse {
            owner_email: refreshed.owner_email,
            editor_emails: self.store.editors(&site.id)?,
        })
    }

    pub fn update_editors(
        &mut self,
        actor_pubkey: &str,
        name: &str,
        request: &EditorsRequest,
        now: u64,
    ) -> Result<EditorsResponse, EngineError> {
        self.update_editors_with_actor_email(actor_pubkey, None, name, request, now)
    }

    pub fn update_editors_with_actor_email(
        &mut self,
        actor_pubkey: &str,
        actor_email: Option<&str>,
        name: &str,
        request: &EditorsRequest,
        now: u64,
    ) -> Result<EditorsResponse, EngineError> {
        let site = self
            .store
            .site_by_name(name)?
            .ok_or(EngineError::SiteNotFound)?;
        if !self.actor_can_manage_editors(&site, actor_pubkey, actor_email, now)? {
            return Err(EngineError::NotAuthorized);
        }
        let changes = request.add_emails.len() + request.remove_emails.len();
        if changes > MAX_EMAILS_PER_SHARING_REQUEST as usize {
            return Err(EngineError::Validation("too many emails in one request"));
        }

        // Bounded by MAX_EMAILS_PER_SHARING_REQUEST, checked above.
        for email in &request.remove_emails {
            let normalized = validate_email(email)?;
            if site.owner_email.as_deref() == Some(normalized.as_str()) {
                return Err(EngineError::Validation(
                    "owner email cannot be removed as editor",
                ));
            }
            self.store.remove_editor(&site.id, &normalized, now)?;
        }
        for email in &request.add_emails {
            let normalized = validate_email(email)?;
            if site.owner_email.as_deref() == Some(normalized.as_str()) {
                continue;
            }
            if !self.store.is_email_editor(&site.id, &normalized)?
                && self.store.count_editors(&site.id)? >= MAX_EDITORS_PER_SITE
            {
                return Err(EngineError::TooManyEditors);
            }
            self.store
                .add_editor(&site.id, &normalized, actor_pubkey, now)?;
        }
        self.store
            .record_event(Some(&site.id), "editors_updated", Some(actor_pubkey), now)?;
        Ok(EditorsResponse {
            owner_email: site.owner_email,
            editor_emails: self.store.editors(&site.id)?,
        })
    }

    pub fn list_editors(
        &self,
        actor_pubkey: &str,
        name: &str,
    ) -> Result<EditorsResponse, EngineError> {
        self.list_editors_with_actor_email(actor_pubkey, None, name, 0)
    }

    pub fn list_editors_with_actor_email(
        &self,
        actor_pubkey: &str,
        actor_email: Option<&str>,
        name: &str,
        now: u64,
    ) -> Result<EditorsResponse, EngineError> {
        let site = self
            .store
            .site_by_name(name)?
            .ok_or(EngineError::SiteNotFound)?;
        if !self.actor_can_manage_editors(&site, actor_pubkey, actor_email, now)? {
            return Err(EngineError::NotAuthorized);
        }
        Ok(EditorsResponse {
            owner_email: site.owner_email,
            editor_emails: self.store.editors(&site.id)?,
        })
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
        let actor_is_authorized =
            actor_pubkey == site.owner_pubkey || actor_pubkey == site.site_pubkey;
        if !actor_is_authorized {
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
            owner_email: site.owner_email.clone(),
            active_version: site.active_version_number,
            shared_emails: self.store.shares(&site.id)?,
            editor_emails: self.store.editors(&site.id)?,
            source: self.site_source_info(site)?,
        })
    }

    fn site_source_info(
        &self,
        site: &SiteRecord,
    ) -> Result<Option<SourceSnapshotInfo>, EngineError> {
        let Some(version_id) = site.active_version_id.as_deref() else {
            return Ok(None);
        };
        let Some(version_number) = site.active_version_number else {
            return Err(StoreError::CorruptState("active version missing number").into());
        };
        Ok(self
            .store
            .version_source(version_id)?
            .map(|source| SourceSnapshotInfo {
                version_number,
                sha256: source.sha256,
                size: source.size,
            }))
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

    /// A site gets platform-authored agent instructions only when another
    /// human can edit it, the current Version has source, and the user did
    /// not publish their own `/llms.txt`.
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
        if self.store.editors(&site.id)?.is_empty() {
            return Ok(false);
        }
        Ok(self.site_source_info(site)?.is_some())
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

    pub fn source_snapshot(
        &self,
        actor_pubkey: &str,
        actor_email: Option<&str>,
        name: &str,
        now: u64,
    ) -> Result<SourceSnapshotDownload, EngineError> {
        let site = match actor_email {
            Some(email) => {
                let (site, _) = self.authorize_email_publish(name, actor_pubkey, email, now)?;
                site
            }
            None => {
                let site = self
                    .store
                    .site_by_name(name)?
                    .ok_or(EngineError::SiteNotFound)?;
                if actor_pubkey != site.owner_pubkey && actor_pubkey != site.site_pubkey {
                    return Err(EngineError::NotAuthorized);
                }
                site
            }
        };
        let Some((version_number, source)) = self.store.active_version_source(&site.id)? else {
            return Err(EngineError::SiteNotFound);
        };
        let bytes = self.blobs.get(&source.sha256)?;
        Ok(SourceSnapshotDownload {
            version_number,
            sha256: source.sha256,
            size: source.size,
            bytes,
        })
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

    // ---- internal helpers ----------------------------------------------------------

    fn authorize_site_key(&self, site_pubkey: &str, now: u64) -> Result<SiteRecord, EngineError> {
        if !hex::is_hex32(site_pubkey) {
            return Err(EngineError::NotAuthorized);
        }
        let site = self
            .store
            .site_by_site_pubkey(site_pubkey)?
            .ok_or(EngineError::SiteNotFound)?;
        self.ensure_site_can_publish(&site, now)?;
        Ok(site)
    }

    fn authorize_email_publish(
        &self,
        name: &str,
        actor_pubkey: &str,
        actor_email: &str,
        now: u64,
    ) -> Result<(SiteRecord, String), EngineError> {
        if !hex::is_hex32(actor_pubkey) {
            return Err(EngineError::NotAuthorized);
        }
        let normalized = validate_email(actor_email)?;
        let site = self
            .store
            .site_by_name(name)?
            .ok_or(EngineError::SiteNotFound)?;
        self.authorize_email_for_site(&site, actor_pubkey, &normalized, now)?;
        Ok((site, normalized))
    }

    fn authorize_email_for_site(
        &self,
        site: &SiteRecord,
        actor_pubkey: &str,
        email: &str,
        now: u64,
    ) -> Result<(), EngineError> {
        self.ensure_site_can_publish(site, now)?;
        if !self.store.has_email_key(email, actor_pubkey)? {
            return Err(EngineError::NotAuthorized);
        }
        let is_owner_email = site.owner_email.as_deref() == Some(email);
        let is_editor = self.store.is_email_editor(&site.id, email)?;
        if !is_owner_email && !is_editor {
            return Err(EngineError::NotAuthorized);
        }
        Ok(())
    }

    fn actor_can_manage_editors(
        &self,
        site: &SiteRecord,
        actor_pubkey: &str,
        actor_email: Option<&str>,
        now: u64,
    ) -> Result<bool, EngineError> {
        if actor_pubkey == site.owner_pubkey || actor_pubkey == site.site_pubkey {
            return Ok(true);
        }
        let Some(email) = actor_email else {
            return Ok(false);
        };
        let normalized = validate_email(email)?;
        if site.owner_email.as_deref() != Some(normalized.as_str()) {
            return Ok(false);
        }
        self.ensure_site_can_publish(site, now)?;
        Ok(self.store.has_email_key(&normalized, actor_pubkey)?)
    }

    fn authorize_publish_actor(
        &self,
        actor_pubkey: &str,
        publish_id: &str,
        now: u64,
    ) -> Result<(SiteRecord, PublishRecord), EngineError> {
        if !hex::is_hex32(actor_pubkey) {
            return Err(EngineError::NotAuthorized);
        }
        let publish = self
            .store
            .publish_by_id(publish_id)?
            .ok_or(EngineError::PublishNotFound)?;
        let site = self
            .store
            .site_by_id(&publish.site_id)?
            .ok_or(StoreError::CorruptState("publish references missing site"))?;
        if let Some(stored_actor) = publish.actor_pubkey.as_deref()
            && stored_actor != actor_pubkey
        {
            return Err(EngineError::NotAuthorized);
        }
        if let Some(email) = publish.actor_email.as_deref() {
            self.authorize_email_for_site(&site, actor_pubkey, email, now)?;
            return Ok((site, publish));
        }
        if actor_pubkey != site.site_pubkey {
            return Err(EngineError::NotAuthorized);
        }
        self.ensure_site_can_publish(&site, now)?;
        Ok((site, publish))
    }

    fn ensure_site_can_publish(&self, site: &SiteRecord, now: u64) -> Result<(), EngineError> {
        if site.status == SiteStatus::Disabled || site.status == SiteStatus::Deleted {
            return Err(EngineError::Conflict("site is disabled"));
        }
        // Revoking all of an owner's grants stops publishing for their sites,
        // including email-keyed owner/editor publishes.
        if !self.store.has_publish_access(&site.owner_pubkey, now)? {
            return Err(EngineError::NotAllowlisted);
        }
        Ok(())
    }
}

/// Start commands are one printable shell line. The command runs inside
/// the app sandbox as an unprivileged dynamic user; this validation is
/// about registry hygiene, not command safety.
fn validate_start_command(command: &str) -> Result<(), EngineError> {
    let trimmed = command.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_START_COMMAND_BYTES as usize {
        return Err(EngineError::Validation("start command empty or too long"));
    }
    let printable = trimmed.bytes().all(|b| (0x20..0x7f).contains(&b));
    if !printable {
        return Err(EngineError::Validation(
            "start command must be printable ascii",
        ));
    }
    Ok(())
}

fn validate_source_snapshot(
    source: &SourceSnapshotRequest,
) -> Result<SourceSnapshotRecord, EngineError> {
    if !hex::is_hex32(&source.sha256) {
        return Err(EngineError::Validation(
            "source sha256 must be 64 lowercase hex chars",
        ));
    }
    if source.size == 0 || source.size > MAX_SOURCE_SNAPSHOT_BYTES {
        return Err(EngineError::Validation("source snapshot size is invalid"));
    }
    Ok(SourceSnapshotRecord {
        sha256: source.sha256.clone(),
        size: source.size,
    })
}

#[cfg(test)]
mod tests;
