use sha2::{Digest, Sha256};

use finitesites_blob::BlobStore;
use std::collections::BTreeMap;

use finitesites_proto::dto::{ProjectApplyRequest, ProjectCollaboratorSpec, SharingRequest};
use finitesites_proto::limits::{LOGIN_TOKEN_TTL_SECONDS, MAX_SHARES_PER_SITE};
use finitesites_proto::project_config::{
    ProjectConfig, ProjectOutputConfig, ProjectOutputKind, ProjectSection,
};
use finitesites_proto::{ManifestFile, hex};
use finitesites_store::{PublishGrantSource, SiteStatus, Store, Visibility};

use crate::{Engine, EngineConfig, EngineError, ViewAccess};

const OWNER: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const OTHER_OWNER: &str = "9999999999999999999999999999999999999999999999999999999999999999";
const NOW: u64 = 1_750_000_000;

struct Fixture {
    engine: Engine,
    _blob_dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let blob_dir = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    let blobs = BlobStore::open(blob_dir.path()).unwrap();
    let config = EngineConfig {
        base_domain: "sites.test".into(),
        site_url_scheme: "http".into(),
        site_url_port: None,
    };
    let mut engine = Engine::new(store, blobs, [42u8; 32], config);
    engine
        .store_mut()
        .allow_pubkey(OWNER, "test owner", NOW)
        .unwrap();
    Fixture {
        engine,
        _blob_dir: blob_dir,
    }
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(&Sha256::digest(bytes))
}

fn output_file(path: &str, bytes: &[u8]) -> (ManifestFile, Vec<u8>) {
    (
        ManifestFile {
            path: path.to_string(),
            sha256: sha(bytes),
            size: bytes.len() as u64,
        },
        bytes.to_vec(),
    )
}

fn project_request(slug: &str, site_name: &str, spa: bool, dry_run: bool) -> ProjectApplyRequest {
    let mut outputs = BTreeMap::new();
    outputs.insert(
        "site".to_string(),
        ProjectOutputConfig {
            kind: ProjectOutputKind::Site,
            site_name: site_name.to_string(),
            branch: "main".to_string(),
            path: ".".to_string(),
            spa,
        },
    );
    ProjectApplyRequest {
        config: ProjectConfig {
            project: ProjectSection {
                slug: slug.to_string(),
            },
            outputs,
        },
        dry_run,
        collaborators: vec![ProjectCollaboratorSpec {
            email: "skyler@example.com".to_string(),
            role: "editor".to_string(),
        }],
    }
}

fn remote(slug: &str) -> String {
    format!("https://git.finite.chat/{slug}.git")
}

fn apply_project_site(engine: &mut Engine, slug: &str, site_name: &str, spa: bool) -> String {
    let response = engine
        .apply_project(
            OWNER,
            &project_request(slug, site_name, spa, false),
            remote(slug),
            NOW,
        )
        .unwrap();
    response.outputs[0].site_id.as_ref().unwrap().clone()
}

fn publish_project_site(
    engine: &mut Engine,
    slug: &str,
    site_name: &str,
    spa: bool,
) -> crate::FinalizeOutcome {
    let site_id = apply_project_site(engine, slug, site_name, spa);
    let index: &[u8] = b"<h1>hello</h1>";
    let style: &[u8] = b"body { color: red }";
    engine
        .commit_project_output_version(
            &site_id,
            vec![
                output_file("/index.html", index),
                output_file("/css/style.css", style),
            ],
            spa,
            NOW + 1,
        )
        .unwrap()
}

fn verify_email_key(engine: &mut Engine, email: &str, pubkey: &str) {
    let token = engine.request_email_login(email, NOW).unwrap();
    engine
        .redeem_email_login(pubkey, email, &token.token, NOW + 1)
        .unwrap();
}

// ---- projects -------------------------------------------------------------

#[test]
fn project_apply_dry_run_create_and_replay() {
    let mut fx = fixture();
    let dry_run = fx
        .engine
        .apply_project(
            OWNER,
            &project_request("finitechat-native", "finitechat-native-mockup", false, true),
            remote("finitechat-native"),
            NOW,
        )
        .unwrap();
    assert!(dry_run.dry_run);
    assert!(dry_run.created);
    assert_eq!(dry_run.project_id, None);
    assert_eq!(dry_run.outputs[0].site_id, None);
    assert!(
        fx.engine
            .resolve_site("finitechat-native-mockup")
            .unwrap()
            .is_none()
    );

    let created = fx
        .engine
        .apply_project(
            OWNER,
            &project_request(
                "finitechat-native",
                "finitechat-native-mockup",
                false,
                false,
            ),
            remote("finitechat-native"),
            NOW + 1,
        )
        .unwrap();
    assert!(!created.dry_run);
    assert!(created.created);
    assert!(created.project_id.is_some());
    assert!(created.outputs[0].created);
    assert_eq!(created.collaborators[0].email, "skyler@example.com");
    let site = fx
        .engine
        .resolve_site("finitechat-native-mockup")
        .unwrap()
        .unwrap();
    assert_eq!(site.status, SiteStatus::ClaimedUnpublished);
    assert_eq!(site.visibility, Visibility::Private);

    let replay = fx
        .engine
        .apply_project(
            OWNER,
            &project_request(
                "finitechat-native",
                "finitechat-native-mockup",
                false,
                false,
            ),
            remote("finitechat-native"),
            NOW + 2,
        )
        .unwrap();
    assert!(!replay.created);
    assert!(!replay.outputs[0].created);
    assert_eq!(replay.project_id, created.project_id);
}

#[test]
fn project_apply_rejects_ungranted_owner_taken_name_and_bad_role() {
    let mut fx = fixture();
    let ungranted = fx.engine.apply_project(
        OTHER_OWNER,
        &project_request("other", "other-site", false, false),
        remote("other"),
        NOW,
    );
    assert!(matches!(ungranted, Err(EngineError::NotAllowlisted)));

    apply_project_site(&mut fx.engine, "first", "shared-name", false);
    fx.engine
        .store_mut()
        .allow_pubkey(OTHER_OWNER, "other", NOW)
        .unwrap();
    let taken = fx.engine.apply_project(
        OTHER_OWNER,
        &project_request("second", "shared-name", false, false),
        remote("second"),
        NOW + 1,
    );
    assert!(matches!(taken, Err(EngineError::NameTaken)));

    let mut bad_role = project_request("bad-role", "bad-role-site", false, false);
    bad_role.collaborators[0].role = "owner".to_string();
    let result = fx
        .engine
        .apply_project(OWNER, &bad_role, remote("bad-role"), NOW + 2);
    assert!(matches!(result, Err(EngineError::Validation(_))));
}

#[test]
fn git_credential_requires_verified_editor_and_honors_revocation() {
    let mut fx = fixture();
    fx.engine
        .apply_project(
            OWNER,
            &project_request(
                "finitechat-native",
                "finitechat-native-mockup",
                false,
                false,
            ),
            remote("finitechat-native"),
            NOW,
        )
        .unwrap();

    let unverified = fx.engine.mint_git_credential(
        OTHER_OWNER,
        "finitechat-native",
        Some("skyler@example.com"),
        remote("finitechat-native"),
        NOW + 1,
    );
    assert!(matches!(unverified, Err(EngineError::NotAuthorized)));

    let owner_credential = fx
        .engine
        .mint_git_credential(
            OWNER,
            "finitechat-native",
            None,
            remote("finitechat-native"),
            NOW + 1,
        )
        .unwrap();
    let owner_auth = fx
        .engine
        .authenticate_git_credential(
            &owner_credential.username,
            &owner_credential.password,
            "finitechat-native",
            NOW + 2,
        )
        .unwrap();
    assert!(owner_auth.can_push);

    let stranger_native = fx.engine.mint_git_credential(
        OTHER_OWNER,
        "finitechat-native",
        None,
        remote("finitechat-native"),
        NOW + 2,
    );
    assert!(matches!(stranger_native, Err(EngineError::NotAuthorized)));

    verify_email_key(&mut fx.engine, "skyler@example.com", OTHER_OWNER);
    let credential = fx
        .engine
        .mint_git_credential(
            OTHER_OWNER,
            "finitechat-native",
            Some("skyler@example.com"),
            remote("finitechat-native"),
            NOW + 3,
        )
        .unwrap();
    assert_eq!(credential.project_slug, "finitechat-native");
    assert_eq!(credential.username, credential.credential_id);
    assert_eq!(credential.password.len(), 64);

    let auth = fx
        .engine
        .authenticate_git_credential(
            &credential.username,
            &credential.password,
            "finitechat-native",
            NOW + 4,
        )
        .unwrap();
    assert!(auth.can_push);
    assert_eq!(auth.project_slug, "finitechat-native");

    let wrong_password = fx.engine.authenticate_git_credential(
        &credential.username,
        "wrong",
        "finitechat-native",
        NOW + 5,
    );
    assert!(matches!(wrong_password, Err(EngineError::NotAuthorized)));

    let wrong_project = fx.engine.authenticate_git_credential(
        &credential.username,
        &credential.password,
        "other-project",
        NOW + 5,
    );
    assert!(matches!(wrong_project, Err(EngineError::NotAuthorized)));

    let removed = fx
        .engine
        .remove_project_collaborator(OWNER, "finitechat-native", "skyler@example.com", NOW + 6)
        .unwrap();
    assert!(removed.removed);
    assert_eq!(removed.revoked_git_credentials, 1);

    let revoked = fx.engine.authenticate_git_credential(
        &credential.username,
        &credential.password,
        "finitechat-native",
        NOW + 7,
    );
    assert!(matches!(revoked, Err(EngineError::NotAuthorized)));

    let replay = fx
        .engine
        .remove_project_collaborator(OWNER, "finitechat-native", "skyler@example.com", NOW + 8)
        .unwrap();
    assert!(!replay.removed);
    assert_eq!(replay.revoked_git_credentials, 0);
}

// ---- project output deployment -------------------------------------------

#[test]
fn project_output_version_publishes_and_serves_content() {
    let mut fx = fixture();
    let outcome = publish_project_site(&mut fx.engine, "hello-project", "hello", false);
    assert_eq!(outcome.version_number, 1);
    assert_eq!(outcome.path_count, 2);
    assert_eq!(outcome.url, "http://hello.sites.test/");
    assert!(outcome.app.is_none());

    let site = fx.engine.resolve_site("hello").unwrap().unwrap();
    assert_eq!(site.status, SiteStatus::Published);

    let found = fx
        .engine
        .lookup_file(&site, "/index.html")
        .unwrap()
        .unwrap();
    assert_eq!(found.size, b"<h1>hello</h1>".len() as u64);
    assert_eq!(found.path, "/index.html");
    assert_eq!(
        fx.engine.read_blob(&found.sha256).unwrap(),
        b"<h1>hello</h1>"
    );

    let root = fx.engine.lookup_file(&site, "/").unwrap().unwrap();
    assert_eq!(root.path, "/index.html");
    assert!(
        fx.engine
            .lookup_file(&site, "/css/style.css")
            .unwrap()
            .is_some()
    );
    assert!(
        fx.engine
            .lookup_file(&site, "/missing.html")
            .unwrap()
            .is_none()
    );
}

#[test]
fn project_output_second_version_replaces_active_snapshot() {
    let mut fx = fixture();
    let site_id = apply_project_site(&mut fx.engine, "hello-project", "hello", false);
    fx.engine
        .commit_project_output_version(
            &site_id,
            vec![
                output_file("/index.html", b"<h1>first</h1>"),
                output_file("/old.html", b"old"),
            ],
            false,
            NOW + 1,
        )
        .unwrap();

    let second = fx
        .engine
        .commit_project_output_version(
            &site_id,
            vec![
                output_file("/index.html", b"<h1>second</h1>"),
                output_file("/new.html", b"new"),
            ],
            false,
            NOW + 2,
        )
        .unwrap();
    assert_eq!(second.version_number, 2);

    let site = fx.engine.resolve_site("hello").unwrap().unwrap();
    assert!(fx.engine.lookup_file(&site, "/new.html").unwrap().is_some());
    assert!(fx.engine.lookup_file(&site, "/old.html").unwrap().is_none());
    let index = fx
        .engine
        .lookup_file(&site, "/index.html")
        .unwrap()
        .unwrap();
    assert_eq!(
        fx.engine.read_blob(&index.sha256).unwrap(),
        b"<h1>second</h1>"
    );
}

#[test]
fn project_output_version_replays_by_git_ref_event_id_after_ack_crash() {
    let mut fx = fixture();
    let created = fx
        .engine
        .apply_project(
            OWNER,
            &project_request(
                "finitechat-native",
                "finitechat-native-mockup",
                false,
                false,
            ),
            remote("finitechat-native"),
            NOW,
        )
        .unwrap();
    let site_id = created.outputs[0].site_id.as_ref().unwrap().clone();
    let project = fx
        .engine
        .store_mut()
        .project_by_slug("finitechat-native")
        .unwrap()
        .unwrap();
    let credential_id = "gcred_11111111111111111111111111111111";
    fx.engine
        .store_mut()
        .create_git_credential(
            credential_id,
            &project.id,
            &project.owner_principal_id,
            &"a".repeat(64),
            None,
            NOW + 1,
        )
        .unwrap();
    let (event, inserted) = fx
        .engine
        .store_mut()
        .record_git_ref_event(
            &project.id,
            "refs/heads/main",
            "0000000000000000000000000000000000000000",
            "1111111111111111111111111111111111111111",
            &project.owner_principal_id,
            None,
            credential_id,
            NOW + 2,
        )
        .unwrap();
    assert!(inserted);

    let first = fx
        .engine
        .commit_project_output_version_for_git_event(
            &site_id,
            Some(event.id),
            vec![output_file("/index.html", b"<h1>git version</h1>")],
            false,
            NOW + 3,
        )
        .unwrap();
    assert_eq!(first.version_number, 1);

    let replay = fx
        .engine
        .commit_project_output_version_for_git_event(
            &site_id,
            Some(event.id),
            vec![output_file(
                "/index.html",
                b"<h1>must not become version two</h1>",
            )],
            false,
            NOW + 4,
        )
        .unwrap();
    assert_eq!(replay.version_id, first.version_id);
    assert_eq!(replay.version_number, 1);

    let site = fx
        .engine
        .resolve_site("finitechat-native-mockup")
        .unwrap()
        .unwrap();
    let found = fx
        .engine
        .lookup_file(&site, "/index.html")
        .unwrap()
        .unwrap();
    assert_eq!(
        fx.engine.read_blob(&found.sha256).unwrap(),
        b"<h1>git version</h1>"
    );
}

#[test]
fn project_output_deploy_rejects_bad_or_unauthorized_bytes() {
    let mut fx = fixture();
    let site_id = apply_project_site(&mut fx.engine, "hello-project", "hello", false);

    fx.engine.store_mut().disallow_pubkey(OWNER).unwrap();
    let revoked = fx.engine.commit_project_output_version(
        &site_id,
        vec![output_file("/index.html", b"hello")],
        false,
        NOW + 1,
    );
    assert!(matches!(revoked, Err(EngineError::NotAllowlisted)));

    fx.engine
        .store_mut()
        .grant_publish_access(
            OWNER,
            PublishGrantSource::Core,
            "paid until cutoff",
            Some(NOW + 10),
            NOW + 2,
        )
        .unwrap();
    let valid_before_expiry = fx.engine.commit_project_output_version(
        &site_id,
        vec![output_file("/index.html", b"hello")],
        false,
        NOW + 9,
    );
    assert!(valid_before_expiry.is_ok());
    let expired = fx.engine.commit_project_output_version(
        &site_id,
        vec![output_file("/index.html", b"later")],
        false,
        NOW + 10,
    );
    assert!(matches!(expired, Err(EngineError::NotAllowlisted)));

    fx.engine
        .store_mut()
        .grant_publish_access(OWNER, PublishGrantSource::Core, "renewed", None, NOW + 11)
        .unwrap();
    let mut bad_hash = output_file("/index.html", b"hello");
    bad_hash.0.sha256 = "0".repeat(64);
    let rejected_hash =
        fx.engine
            .commit_project_output_version(&site_id, vec![bad_hash], false, NOW + 12);
    assert!(matches!(rejected_hash, Err(EngineError::Validation(_))));

    let mut bad_size = output_file("/index.html", b"hello");
    bad_size.0.size += 1;
    let rejected_size =
        fx.engine
            .commit_project_output_version(&site_id, vec![bad_size], false, NOW + 13);
    assert!(matches!(rejected_size, Err(EngineError::Validation(_))));

    let no_index = fx.engine.commit_project_output_version(
        &site_id,
        vec![output_file("/main.html", b"not an index")],
        true,
        NOW + 14,
    );
    assert!(matches!(no_index, Err(EngineError::Validation(_))));
}

// ---- sharing --------------------------------------------------------------

#[test]
fn sharing_is_owner_controlled_and_public_requires_confirmation() {
    let mut fx = fixture();
    publish_project_site(&mut fx.engine, "hello-project", "hello", false);

    let request = SharingRequest {
        visibility: Some("shared".into()),
        confirm_public: false,
        add_emails: vec!["Friend@Example.com".into()],
        remove_emails: vec![],
    };
    let response = fx
        .engine
        .set_sharing(OWNER, "hello", &request, NOW)
        .unwrap();
    assert_eq!(response.visibility, "shared");
    assert_eq!(response.shared_emails, vec!["friend@example.com"]);

    let collaborator_attempt = fx
        .engine
        .set_sharing(OTHER_OWNER, "hello", &request, NOW + 1);
    assert!(matches!(
        collaborator_attempt,
        Err(EngineError::NotAuthorized)
    ));

    let unconfirmed = SharingRequest {
        visibility: Some("public".into()),
        confirm_public: false,
        add_emails: vec![],
        remove_emails: vec![],
    };
    let rejected = fx.engine.set_sharing(OWNER, "hello", &unconfirmed, NOW + 2);
    assert!(matches!(rejected, Err(EngineError::Validation(_))));

    let confirmed = SharingRequest {
        confirm_public: true,
        ..unconfirmed
    };
    let public = fx
        .engine
        .set_sharing(OWNER, "hello", &confirmed, NOW + 3)
        .unwrap();
    assert_eq!(public.visibility, "public");
}

#[test]
fn sharing_validates_emails_and_limits() {
    let mut fx = fixture();
    publish_project_site(&mut fx.engine, "hello-project", "hello", false);

    let bad_email = SharingRequest {
        visibility: None,
        confirm_public: false,
        add_emails: vec!["not-an-email".into()],
        remove_emails: vec![],
    };
    assert!(matches!(
        fx.engine.set_sharing(OWNER, "hello", &bad_email, NOW),
        Err(EngineError::Validation(_))
    ));

    let too_many_at_once = SharingRequest {
        visibility: None,
        confirm_public: false,
        add_emails: (0..21).map(|i| format!("user{i}@example.com")).collect(),
        remove_emails: vec![],
    };
    assert!(matches!(
        fx.engine
            .set_sharing(OWNER, "hello", &too_many_at_once, NOW),
        Err(EngineError::Validation(_))
    ));

    let mut added: u32 = 0;
    while added < MAX_SHARES_PER_SITE {
        let upper = (added + 10).min(MAX_SHARES_PER_SITE);
        let batch = SharingRequest {
            visibility: None,
            confirm_public: false,
            add_emails: (added..upper)
                .map(|i| format!("user{i}@example.com"))
                .collect(),
            remove_emails: vec![],
        };
        fx.engine.set_sharing(OWNER, "hello", &batch, NOW).unwrap();
        added = upper;
    }
    let over_cap = SharingRequest {
        visibility: None,
        confirm_public: false,
        add_emails: vec!["overflow@example.com".into()],
        remove_emails: vec![],
    };
    assert!(matches!(
        fx.engine.set_sharing(OWNER, "hello", &over_cap, NOW),
        Err(EngineError::TooManyShares)
    ));
}

// ---- viewing and magic links ---------------------------------------------

#[test]
fn shared_site_full_magic_link_flow() {
    let mut fx = fixture();
    publish_project_site(&mut fx.engine, "hello-project", "hello", false);
    fx.engine
        .set_sharing(
            OWNER,
            "hello",
            &SharingRequest {
                visibility: Some("shared".into()),
                confirm_public: false,
                add_emails: vec!["friend@example.com".into()],
                remove_emails: vec![],
            },
            NOW,
        )
        .unwrap();
    let site = fx.engine.resolve_site("hello").unwrap().unwrap();

    assert_eq!(
        fx.engine.view_access(&site, None, NOW).unwrap(),
        ViewAccess::NeedsLogin
    );
    assert!(
        fx.engine
            .request_login("hello", "stranger@example.com", NOW)
            .unwrap()
            .is_none()
    );

    let link = fx
        .engine
        .request_login("hello", "Friend@Example.com", NOW)
        .unwrap()
        .unwrap();
    assert!(
        link.url
            .starts_with("http://hello.sites.test/_finite/auth?token=")
    );
    let token = link.url.split("token=").nth(1).unwrap().to_string();

    let (login_site, cookie) = fx.engine.redeem_login(&token, NOW + 60).unwrap();
    assert_eq!(login_site.id, site.id);
    assert_eq!(
        fx.engine
            .view_access(&site, Some(&cookie), NOW + 120)
            .unwrap(),
        ViewAccess::Allowed
    );
    assert!(matches!(
        fx.engine.redeem_login(&token, NOW + 61),
        Err(EngineError::Validation(_))
    ));

    fx.engine
        .set_sharing(
            OWNER,
            "hello",
            &SharingRequest {
                visibility: None,
                confirm_public: false,
                add_emails: vec![],
                remove_emails: vec!["friend@example.com".into()],
            },
            NOW,
        )
        .unwrap();
    assert_eq!(
        fx.engine
            .view_access(&site, Some(&cookie), NOW + 180)
            .unwrap(),
        ViewAccess::NeedsLogin
    );
}

#[test]
fn public_private_and_unpublished_view_paths() {
    let mut fx = fixture();
    let site_id = apply_project_site(&mut fx.engine, "hello-project", "hello", false);
    let unpublished = fx.engine.resolve_site("hello").unwrap().unwrap();
    assert_eq!(
        fx.engine.view_access(&unpublished, None, NOW).unwrap(),
        ViewAccess::NeedsLogin
    );
    assert!(fx.engine.lookup_file(&unpublished, "/").unwrap().is_none());

    fx.engine
        .commit_project_output_version(
            &site_id,
            vec![output_file("/index.html", b"<h1>hello</h1>")],
            false,
            NOW + 1,
        )
        .unwrap();
    let published_private = fx.engine.resolve_site("hello").unwrap().unwrap();
    assert_eq!(
        fx.engine
            .view_access(&published_private, None, NOW + 2)
            .unwrap(),
        ViewAccess::NeedsLogin
    );

    fx.engine
        .set_sharing(
            OWNER,
            "hello",
            &SharingRequest {
                visibility: Some("public".into()),
                confirm_public: true,
                add_emails: vec![],
                remove_emails: vec![],
            },
            NOW + 3,
        )
        .unwrap();
    let public = fx.engine.resolve_site("hello").unwrap().unwrap();
    assert_eq!(
        fx.engine.view_access(&public, None, NOW + 4).unwrap(),
        ViewAccess::Allowed
    );
}

#[test]
fn login_tokens_expire_and_reject_malformed_values() {
    let mut fx = fixture();
    publish_project_site(&mut fx.engine, "hello-project", "hello", false);
    fx.engine
        .set_sharing(
            OWNER,
            "hello",
            &SharingRequest {
                visibility: Some("shared".into()),
                confirm_public: false,
                add_emails: vec!["friend@example.com".into()],
                remove_emails: vec![],
            },
            NOW,
        )
        .unwrap();
    let link = fx
        .engine
        .request_login("hello", "friend@example.com", NOW)
        .unwrap()
        .unwrap();
    let token = link.url.split("token=").nth(1).unwrap().to_string();
    let expired = fx
        .engine
        .redeem_login(&token, NOW + LOGIN_TOKEN_TTL_SECONDS + 1);
    assert!(matches!(expired, Err(EngineError::Validation(_))));

    let garbage = fx.engine.redeem_login("zz", NOW);
    assert!(matches!(garbage, Err(EngineError::Validation(_))));
}

// ---- listing / status -----------------------------------------------------

#[test]
fn list_and_status_respect_ownership() {
    let mut fx = fixture();
    publish_project_site(&mut fx.engine, "hello-project", "hello", false);

    let sites = fx.engine.list_sites(OWNER).unwrap();
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].name, "hello");
    assert_eq!(sites[0].kind, "static");
    assert_eq!(sites[0].active_version, Some(1));
    assert!(fx.engine.list_sites(OTHER_OWNER).unwrap().is_empty());

    let by_owner = fx.engine.site_status(OWNER, "hello").unwrap();
    assert_eq!(by_owner.status, "published");
    let by_rando = fx.engine.site_status(OTHER_OWNER, "hello");
    assert!(matches!(by_rando, Err(EngineError::NotAuthorized)));
    let missing = fx.engine.site_status(OWNER, "ghost");
    assert!(matches!(missing, Err(EngineError::SiteNotFound)));
}

#[test]
fn resolve_rejects_invalid_labels() {
    let fx = fixture();
    assert!(fx.engine.resolve_site("Bad_Label").unwrap().is_none());
    assert!(fx.engine.resolve_site("api").unwrap().is_none());
}

// ---- spa fallback and agent handoff --------------------------------------

#[test]
fn spa_project_output_routes_unknown_paths_to_index() {
    let mut fx = fixture();
    let site_id = apply_project_site(&mut fx.engine, "spa-project", "spa-site", true);
    fx.engine
        .commit_project_output_version(
            &site_id,
            vec![
                output_file("/index.html", b"<div id=app></div>"),
                output_file("/assets/app.js", b"render()"),
            ],
            true,
            NOW + 1,
        )
        .unwrap();
    let site = fx.engine.resolve_site("spa-site").unwrap().unwrap();
    assert!(site.active_version_spa);

    let asset = fx
        .engine
        .lookup_file(&site, "/assets/app.js")
        .unwrap()
        .unwrap();
    assert_eq!(asset.path, "/assets/app.js");
    let virtual_route = fx
        .engine
        .lookup_file(&site, "/settings/profile")
        .unwrap()
        .unwrap();
    assert_eq!(virtual_route.path, "/index.html");
}

#[test]
fn generated_llms_txt_only_when_project_output_has_no_authored_file() {
    let mut fx = fixture();
    publish_project_site(&mut fx.engine, "hello-project", "hello", false);
    let site = fx.engine.resolve_site("hello").unwrap().unwrap();
    assert!(fx.engine.should_generate_llms_txt(&site).unwrap());

    let authored_site_id =
        apply_project_site(&mut fx.engine, "authored-project", "authored", false);
    fx.engine
        .commit_project_output_version(
            &authored_site_id,
            vec![
                output_file("/index.html", b"<h1>authored</h1>"),
                output_file("/llms.txt", b"user instructions"),
            ],
            false,
            NOW + 2,
        )
        .unwrap();
    let authored = fx.engine.resolve_site("authored").unwrap().unwrap();
    assert!(!fx.engine.should_generate_llms_txt(&authored).unwrap());
    let llms = fx
        .engine
        .lookup_exact_file(&authored, "/llms.txt")
        .unwrap()
        .unwrap();
    assert_eq!(
        fx.engine.read_blob(&llms.sha256).unwrap(),
        b"user instructions"
    );
}
