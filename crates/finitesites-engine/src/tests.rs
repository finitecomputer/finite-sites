use sha2::{Digest, Sha256};

use finitesites_blob::BlobStore;
use std::collections::BTreeMap;

use finitesites_proto::dto::{
    EditorsRequest, ProjectApplyRequest, ProjectCollaboratorSpec, SharingRequest,
    SourceSnapshotRequest,
};
use finitesites_proto::limits::{
    LOGIN_TOKEN_TTL_SECONDS, MAX_SHARES_PER_SITE, MAX_SITES_PER_OWNER,
};
use finitesites_proto::project_config::{
    ProjectConfig, ProjectOutputConfig, ProjectOutputKind, ProjectSection,
};
use finitesites_proto::{ManifestFile, PublishManifest, hex};
use finitesites_store::{PublishGrantSource, SiteStatus, Store, Visibility};

use crate::{Engine, EngineConfig, EngineError, ViewAccess};

const OWNER: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const OTHER_OWNER: &str = "9999999999999999999999999999999999999999999999999999999999999999";
const SITE_KEY: &str = "2222222222222222222222222222222222222222222222222222222222222222";
const OTHER_SITE_KEY: &str = "3333333333333333333333333333333333333333333333333333333333333333";
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

fn manifest_for(entries: &[(&str, &[u8])]) -> PublishManifest {
    PublishManifest {
        files: entries
            .iter()
            .map(|(path, bytes)| ManifestFile {
                path: (*path).to_string(),
                sha256: sha(bytes),
                size: bytes.len() as u64,
            })
            .collect(),
    }
}

fn project_request(dry_run: bool) -> ProjectApplyRequest {
    let mut outputs = BTreeMap::new();
    outputs.insert(
        "mockup".to_string(),
        ProjectOutputConfig {
            kind: ProjectOutputKind::Site,
            site_name: "finitechat-native-mockup".to_string(),
            branch: "main".to_string(),
            path: ".".to_string(),
            spa: false,
        },
    );
    ProjectApplyRequest {
        config: ProjectConfig {
            project: ProjectSection {
                slug: "finitechat-native".to_string(),
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

/// Claim + publish a two-file site, returning the publish outcome.
fn publish_site(engine: &mut Engine, name: &str, site_key: &str) -> crate::FinalizeOutcome {
    engine.claim(OWNER, name, site_key, NOW).unwrap();
    let index: &[u8] = b"<h1>hello</h1>";
    let style: &[u8] = b"body { color: red }";
    let manifest = manifest_for(&[("/index.html", index), ("/css/style.css", style)]);
    let begun = engine
        .begin_publish(site_key, &manifest, false, None, NOW)
        .unwrap();
    engine
        .upload_blob(site_key, &begun.publish_id, &sha(index), index, NOW)
        .unwrap();
    engine
        .upload_blob(site_key, &begun.publish_id, &sha(style), style, NOW)
        .unwrap();
    engine
        .finalize_publish(site_key, &begun.publish_id, NOW)
        .unwrap()
}

// ---- claim ----------------------------------------------------------------

#[test]
fn claim_succeeds_and_replays_idempotently() {
    let mut fx = fixture();
    let first = fx.engine.claim(OWNER, "hello", SITE_KEY, NOW).unwrap();
    assert!(!first.already_claimed);
    assert_eq!(first.url, "http://hello.sites.test/");
    assert_eq!(first.site.status, SiteStatus::ClaimedUnpublished);
    assert_eq!(first.site.visibility, Visibility::Private);

    let replay = fx.engine.claim(OWNER, "hello", SITE_KEY, NOW + 1).unwrap();
    assert!(replay.already_claimed);
    assert_eq!(replay.site.id, first.site.id);
}

#[test]
fn claim_rejects_owner_without_publish_grant() {
    let mut fx = fixture();
    let result = fx.engine.claim(OTHER_OWNER, "hello", SITE_KEY, NOW);
    assert!(matches!(result, Err(EngineError::NotAllowlisted)));
}

#[test]
fn claim_rejects_taken_and_invalid_names() {
    let mut fx = fixture();
    fx.engine
        .store_mut()
        .allow_pubkey(OTHER_OWNER, "other", NOW)
        .unwrap();
    fx.engine.claim(OWNER, "hello", SITE_KEY, NOW).unwrap();

    let taken = fx.engine.claim(OTHER_OWNER, "hello", OTHER_SITE_KEY, NOW);
    assert!(matches!(taken, Err(EngineError::NameTaken)));

    let reserved = fx.engine.claim(OWNER, "api", OTHER_SITE_KEY, NOW);
    assert!(matches!(reserved, Err(EngineError::Proto(_))));

    let invalid = fx.engine.claim(OWNER, "Bad_Name", OTHER_SITE_KEY, NOW);
    assert!(matches!(invalid, Err(EngineError::Proto(_))));

    let bad_key = fx.engine.claim(OWNER, "world", "not-hex", NOW);
    assert!(matches!(bad_key, Err(EngineError::Validation(_))));
}

#[test]
fn claim_rejects_reused_site_key() {
    let mut fx = fixture();
    fx.engine.claim(OWNER, "hello", SITE_KEY, NOW).unwrap();
    let reused = fx.engine.claim(OWNER, "world", SITE_KEY, NOW);
    assert!(matches!(reused, Err(EngineError::Conflict(_))));
}

#[test]
fn claim_enforces_per_owner_limit() {
    let mut fx = fixture();
    // Bounded loop: exactly MAX_SITES_PER_OWNER claims.
    for index in 0..MAX_SITES_PER_OWNER {
        let key = format!("{:064x}", 0x4000 + index);
        fx.engine
            .claim(OWNER, &format!("site-{index}"), &key, NOW)
            .unwrap();
    }
    let over = fx
        .engine
        .claim(OWNER, "one-too-many", &format!("{:064x}", 0x9000), NOW);
    assert!(matches!(over, Err(EngineError::TooManySites)));
}

// ---- projects -------------------------------------------------------------

#[test]
fn project_apply_dry_run_create_and_replay() {
    let mut fx = fixture();
    let dry_run = fx
        .engine
        .apply_project(
            OWNER,
            &project_request(true),
            "https://git.finite.chat/finitechat-native.git".to_string(),
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
            &project_request(false),
            "https://git.finite.chat/finitechat-native.git".to_string(),
            NOW + 1,
        )
        .unwrap();
    assert!(!created.dry_run);
    assert!(created.created);
    assert!(created.project_id.is_some());
    assert!(created.outputs[0].created);
    assert_eq!(created.collaborators[0].email, "skyler@example.com");
    assert!(
        fx.engine
            .resolve_site("finitechat-native-mockup")
            .unwrap()
            .is_some()
    );

    let replay = fx
        .engine
        .apply_project(
            OWNER,
            &project_request(false),
            "https://git.finite.chat/finitechat-native.git".to_string(),
            NOW + 2,
        )
        .unwrap();
    assert!(!replay.created);
    assert!(!replay.outputs[0].created);
    assert_eq!(replay.project_id, created.project_id);
}

#[test]
fn project_apply_rejects_role_that_does_not_belong_to_collaborators() {
    let mut fx = fixture();
    let mut request = project_request(false);
    request.collaborators[0].role = "owner".to_string();
    let result = fx.engine.apply_project(
        OWNER,
        &request,
        "https://git.finite.chat/finitechat-native.git".to_string(),
        NOW,
    );
    assert!(matches!(result, Err(EngineError::Validation(_))));
}

#[test]
fn git_credential_requires_verified_project_collaborator() {
    let mut fx = fixture();
    fx.engine
        .apply_project(
            OWNER,
            &project_request(false),
            "https://git.finite.chat/finitechat-native.git".to_string(),
            NOW,
        )
        .unwrap();

    let unverified = fx.engine.mint_git_credential(
        OTHER_OWNER,
        "finitechat-native",
        "skyler@example.com",
        "https://git.finite.chat/finitechat-native.git".to_string(),
        NOW + 1,
    );
    assert!(matches!(unverified, Err(EngineError::NotAuthorized)));

    verify_email_key(&mut fx.engine, "skyler@example.com", OTHER_OWNER);
    let credential = fx
        .engine
        .mint_git_credential(
            OTHER_OWNER,
            "finitechat-native",
            "skyler@example.com",
            "https://git.finite.chat/finitechat-native.git".to_string(),
            NOW + 2,
        )
        .unwrap();
    assert_eq!(credential.project_slug, "finitechat-native");
    assert_eq!(credential.username, credential.credential_id);
    assert_eq!(credential.password.len(), 64);
    assert!(
        fx.engine
            .store_mut()
            .git_credential_by_id(&credential.credential_id)
            .unwrap()
            .is_some()
    );
}

#[test]
fn project_output_version_replays_by_git_ref_event_id_after_ack_crash() {
    let mut fx = fixture();
    let created = fx
        .engine
        .apply_project(
            OWNER,
            &project_request(false),
            "https://git.finite.chat/finitechat-native.git".to_string(),
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

    let first_bytes: &[u8] = b"<h1>git version</h1>";
    let first_file = ManifestFile {
        path: "/index.html".to_string(),
        sha256: sha(first_bytes),
        size: first_bytes.len() as u64,
    };
    let first = fx
        .engine
        .commit_project_output_version_for_git_event(
            &site_id,
            Some(event.id),
            vec![(first_file, first_bytes.to_vec())],
            false,
            NOW + 3,
        )
        .unwrap();
    assert_eq!(first.version_number, 1);

    let replay_bytes: &[u8] = b"<h1>must not become version two</h1>";
    let replay_file = ManifestFile {
        path: "/index.html".to_string(),
        sha256: sha(replay_bytes),
        size: replay_bytes.len() as u64,
    };
    let replay = fx
        .engine
        .commit_project_output_version_for_git_event(
            &site_id,
            Some(event.id),
            vec![(replay_file, replay_bytes.to_vec())],
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
    assert_eq!(fx.engine.read_blob(&found.sha256).unwrap(), first_bytes);
}

// ---- publish ----------------------------------------------------------------

#[test]
fn publish_full_flow_serves_content() {
    let mut fx = fixture();
    let outcome = publish_site(&mut fx.engine, "hello", SITE_KEY);
    assert_eq!(outcome.version_number, 1);
    assert_eq!(outcome.path_count, 2);
    assert_eq!(outcome.url, "http://hello.sites.test/");

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

    // Root and folder fallbacks resolve to the index manifest path.
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
fn second_publish_dedups_blobs_and_bumps_version() {
    let mut fx = fixture();
    publish_site(&mut fx.engine, "hello", SITE_KEY);

    let index: &[u8] = b"<h1>hello</h1>"; // unchanged file
    let fresh: &[u8] = b"<p>new page</p>";
    let manifest = manifest_for(&[("/index.html", index), ("/new.html", fresh)]);
    let begun = fx
        .engine
        .begin_publish(SITE_KEY, &manifest, false, None, NOW + 10)
        .unwrap();
    // Only the new content is missing; the unchanged file dedups.
    assert_eq!(begun.missing, vec![sha(fresh)]);

    fx.engine
        .upload_blob(SITE_KEY, &begun.publish_id, &sha(fresh), fresh, NOW + 10)
        .unwrap();
    let outcome = fx
        .engine
        .finalize_publish(SITE_KEY, &begun.publish_id, NOW + 10)
        .unwrap();
    assert_eq!(outcome.version_number, 2);

    let site = fx.engine.resolve_site("hello").unwrap().unwrap();
    assert!(fx.engine.lookup_file(&site, "/new.html").unwrap().is_some());
    // Old version's exclusive file is gone from the active version.
    assert!(
        fx.engine
            .lookup_file(&site, "/css/style.css")
            .unwrap()
            .is_none()
    );
}

#[test]
fn begin_publish_rejects_unknown_key_and_bad_manifest() {
    let mut fx = fixture();
    fx.engine.claim(OWNER, "hello", SITE_KEY, NOW).unwrap();

    let manifest = manifest_for(&[("/index.html", b"x")]);
    let unknown = fx
        .engine
        .begin_publish(OTHER_SITE_KEY, &manifest, false, None, NOW);
    assert!(matches!(unknown, Err(EngineError::SiteNotFound)));

    let empty = PublishManifest { files: vec![] };
    let invalid = fx.engine.begin_publish(SITE_KEY, &empty, false, None, NOW);
    assert!(matches!(invalid, Err(EngineError::Proto(_))));
}

#[test]
fn begin_publish_stops_after_owner_grant_revoked() {
    let mut fx = fixture();
    fx.engine.claim(OWNER, "hello", SITE_KEY, NOW).unwrap();
    fx.engine.store_mut().disallow_pubkey(OWNER).unwrap();
    let manifest = manifest_for(&[("/index.html", b"x")]);
    let result = fx
        .engine
        .begin_publish(SITE_KEY, &manifest, false, None, NOW);
    assert!(matches!(result, Err(EngineError::NotAllowlisted)));
}

#[test]
fn begin_publish_stops_after_publish_grant_expires() {
    let mut fx = fixture();
    fx.engine.claim(OWNER, "hello", SITE_KEY, NOW).unwrap();
    fx.engine.store_mut().disallow_pubkey(OWNER).unwrap();
    fx.engine
        .store_mut()
        .grant_publish_access(
            OWNER,
            PublishGrantSource::Core,
            "paid until cutoff",
            Some(NOW + 10),
            NOW + 1,
        )
        .unwrap();
    let manifest = manifest_for(&[("/index.html", b"x")]);
    fx.engine
        .begin_publish(SITE_KEY, &manifest, false, None, NOW + 9)
        .unwrap();

    let expired = fx
        .engine
        .begin_publish(SITE_KEY, &manifest, false, None, NOW + 10);
    assert!(matches!(expired, Err(EngineError::NotAllowlisted)));
}

#[test]
fn upload_blob_negative_paths() {
    let mut fx = fixture();
    fx.engine.claim(OWNER, "hello", SITE_KEY, NOW).unwrap();
    fx.engine
        .store_mut()
        .allow_pubkey(OTHER_OWNER, "other", NOW)
        .unwrap();
    fx.engine
        .claim(OTHER_OWNER, "other", OTHER_SITE_KEY, NOW)
        .unwrap();

    let content: &[u8] = b"<h1>hello</h1>";
    let manifest = manifest_for(&[("/index.html", content)]);
    let begun = fx
        .engine
        .begin_publish(SITE_KEY, &manifest, false, None, NOW)
        .unwrap();

    // Another site's key may not upload into this publish.
    let foreign = fx.engine.upload_blob(
        OTHER_SITE_KEY,
        &begun.publish_id,
        &sha(content),
        content,
        NOW,
    );
    assert!(matches!(foreign, Err(EngineError::NotAuthorized)));

    // A hash the manifest does not reference is rejected.
    let stray: &[u8] = b"stray bytes";
    let unlisted = fx
        .engine
        .upload_blob(SITE_KEY, &begun.publish_id, &sha(stray), stray, NOW);
    assert!(matches!(unlisted, Err(EngineError::Validation(_))));

    // Size mismatch against the manifest is rejected.
    let truncated = fx.engine.upload_blob(
        SITE_KEY,
        &begun.publish_id,
        &sha(content),
        &content[..3],
        NOW,
    );
    assert!(matches!(truncated, Err(EngineError::Validation(_))));

    // Replay of a good upload is fine.
    fx.engine
        .upload_blob(SITE_KEY, &begun.publish_id, &sha(content), content, NOW)
        .unwrap();
    fx.engine
        .upload_blob(SITE_KEY, &begun.publish_id, &sha(content), content, NOW)
        .unwrap();

    // After finalize, further uploads conflict.
    fx.engine
        .finalize_publish(SITE_KEY, &begun.publish_id, NOW)
        .unwrap();
    let late = fx
        .engine
        .upload_blob(SITE_KEY, &begun.publish_id, &sha(content), content, NOW);
    assert!(matches!(late, Err(EngineError::Conflict(_))));
}

#[test]
fn finalize_requires_all_blobs_and_replays_idempotently() {
    let mut fx = fixture();
    fx.engine.claim(OWNER, "hello", SITE_KEY, NOW).unwrap();
    let content: &[u8] = b"<h1>hello</h1>";
    let manifest = manifest_for(&[("/index.html", content)]);
    let begun = fx
        .engine
        .begin_publish(SITE_KEY, &manifest, false, None, NOW)
        .unwrap();

    let early = fx.engine.finalize_publish(SITE_KEY, &begun.publish_id, NOW);
    assert!(matches!(
        early,
        Err(EngineError::Conflict("publish has missing blobs"))
    ));

    fx.engine
        .upload_blob(SITE_KEY, &begun.publish_id, &sha(content), content, NOW)
        .unwrap();
    let first = fx
        .engine
        .finalize_publish(SITE_KEY, &begun.publish_id, NOW)
        .unwrap();
    let replay = fx
        .engine
        .finalize_publish(SITE_KEY, &begun.publish_id, NOW + 5)
        .unwrap();
    assert_eq!(first.version_number, replay.version_number);

    let unknown = fx.engine.finalize_publish(SITE_KEY, "pub_missing", NOW);
    assert!(matches!(unknown, Err(EngineError::PublishNotFound)));
}

// ---- sharing -------------------------------------------------------------

#[test]
fn sharing_owner_and_site_key_can_mutate_others_cannot() {
    let mut fx = fixture();
    publish_site(&mut fx.engine, "hello", SITE_KEY);

    let request = SharingRequest {
        visibility: Some("shared".into()),
        confirm_public: false,
        add_emails: vec!["Friend@Example.com".into()],
        remove_emails: vec![],
    };
    let by_owner = fx
        .engine
        .set_sharing(OWNER, "hello", &request, NOW)
        .unwrap();
    assert_eq!(by_owner.visibility, "shared");
    assert_eq!(by_owner.shared_emails, vec!["friend@example.com"]);

    let by_site_key = fx
        .engine
        .set_sharing(
            SITE_KEY,
            "hello",
            &SharingRequest {
                visibility: None,
                confirm_public: false,
                add_emails: vec!["second@example.com".into()],
                remove_emails: vec![],
            },
            NOW,
        )
        .unwrap();
    assert_eq!(by_site_key.shared_emails.len(), 2);

    let by_rando = fx.engine.set_sharing(OTHER_OWNER, "hello", &request, NOW);
    assert!(matches!(by_rando, Err(EngineError::NotAuthorized)));

    let missing = fx.engine.set_sharing(OWNER, "ghost", &request, NOW);
    assert!(matches!(missing, Err(EngineError::SiteNotFound)));
}

#[test]
fn public_requires_explicit_confirmation() {
    let mut fx = fixture();
    publish_site(&mut fx.engine, "hello", SITE_KEY);

    let unconfirmed = SharingRequest {
        visibility: Some("public".into()),
        confirm_public: false,
        add_emails: vec![],
        remove_emails: vec![],
    };
    let rejected = fx.engine.set_sharing(OWNER, "hello", &unconfirmed, NOW);
    assert!(matches!(rejected, Err(EngineError::Validation(_))));

    let confirmed = SharingRequest {
        confirm_public: true,
        ..unconfirmed
    };
    let response = fx
        .engine
        .set_sharing(OWNER, "hello", &confirmed, NOW)
        .unwrap();
    assert_eq!(response.visibility, "public");
}

#[test]
fn sharing_validates_emails_and_limits() {
    let mut fx = fixture();
    publish_site(&mut fx.engine, "hello", SITE_KEY);

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

    // Fill the share table to the per-site cap in bounded batches.
    let mut added: u32 = 0;
    while added < MAX_SHARES_PER_SITE {
        let batch = SharingRequest {
            visibility: None,
            confirm_public: false,
            add_emails: (added..(added + 10).min(MAX_SHARES_PER_SITE))
                .map(|i| format!("user{i}@example.com"))
                .collect(),
            remove_emails: vec![],
        };
        fx.engine.set_sharing(OWNER, "hello", &batch, NOW).unwrap();
        added += 10;
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

// ---- email editors and source snapshots -----------------------------------

fn verify_email_key(engine: &mut Engine, email: &str, pubkey: &str) {
    let token = engine.request_email_login(email, NOW).unwrap();
    engine
        .redeem_email_login(pubkey, email, &token.token, NOW + 1)
        .unwrap();
}

#[test]
fn owner_and_editor_email_publish_with_source_snapshot() {
    let mut fx = fixture();
    fx.engine
        .claim_with_owner_email(OWNER, "hello", SITE_KEY, Some("paul@finite.vip"), NOW)
        .unwrap();
    verify_email_key(&mut fx.engine, "paul@finite.vip", OWNER);
    verify_email_key(&mut fx.engine, "skyler_bot@finite.vip", OTHER_OWNER);

    let owner_content: &[u8] = b"<h1>owner</h1>";
    let owner_manifest = manifest_for(&[("/index.html", owner_content)]);
    let owner_publish = fx
        .engine
        .begin_publish_as_email(
            OWNER,
            "paul@finite.vip",
            "hello",
            &owner_manifest,
            false,
            None,
            None,
            NOW + 2,
        )
        .unwrap();
    fx.engine
        .upload_blob_as_actor(
            OWNER,
            &owner_publish.publish_id,
            &sha(owner_content),
            owner_content,
            NOW + 2,
        )
        .unwrap();
    let owner_finalized = fx
        .engine
        .finalize_publish_as_actor(OWNER, &owner_publish.publish_id, NOW + 2)
        .unwrap();
    assert_eq!(owner_finalized.version_number, 1);

    fx.engine
        .update_editors_with_actor_email(
            OWNER,
            Some("paul@finite.vip"),
            "hello",
            &EditorsRequest {
                actor_email: Some("paul@finite.vip".into()),
                add_emails: vec!["skyler_bot@finite.vip".into()],
                remove_emails: vec![],
            },
            NOW + 3,
        )
        .unwrap();

    let editor_content: &[u8] = b"<h1>editor</h1>";
    let source_bytes: &[u8] = b"pretend-source-tarball";
    let source_request = SourceSnapshotRequest {
        sha256: sha(source_bytes),
        size: source_bytes.len() as u64,
    };
    let editor_manifest = manifest_for(&[("/index.html", editor_content)]);
    let editor_publish = fx
        .engine
        .begin_publish_as_email(
            OTHER_OWNER,
            "skyler_bot@finite.vip",
            "hello",
            &editor_manifest,
            false,
            None,
            Some(&source_request),
            NOW + 4,
        )
        .unwrap();
    assert_eq!(
        editor_publish.missing,
        vec![sha(editor_content), sha(source_bytes)]
    );
    fx.engine
        .upload_blob_as_actor(
            OTHER_OWNER,
            &editor_publish.publish_id,
            &sha(editor_content),
            editor_content,
            NOW + 4,
        )
        .unwrap();
    fx.engine
        .upload_blob_as_actor(
            OTHER_OWNER,
            &editor_publish.publish_id,
            &sha(source_bytes),
            source_bytes,
            NOW + 4,
        )
        .unwrap();
    let editor_finalized = fx
        .engine
        .finalize_publish_as_actor(OTHER_OWNER, &editor_publish.publish_id, NOW + 4)
        .unwrap();
    assert_eq!(editor_finalized.version_number, 2);
    let expected_source_sha = sha(source_bytes);
    assert_eq!(
        editor_finalized
            .source
            .as_ref()
            .map(|source| source.sha256.as_str()),
        Some(expected_source_sha.as_str())
    );

    let summary = fx.engine.site_status(SITE_KEY, "hello").unwrap();
    assert_eq!(summary.owner_email.as_deref(), Some("paul@finite.vip"));
    assert_eq!(summary.editor_emails, vec!["skyler_bot@finite.vip"]);
    assert!(summary.source.is_some());

    let pulled = fx
        .engine
        .source_snapshot(OTHER_OWNER, Some("skyler_bot@finite.vip"), "hello", NOW + 5)
        .unwrap();
    assert_eq!(pulled.bytes, source_bytes);
    assert_eq!(pulled.version_number, 2);
}

#[test]
fn viewer_share_does_not_grant_email_publish() {
    let mut fx = fixture();
    publish_site(&mut fx.engine, "hello", SITE_KEY);
    verify_email_key(&mut fx.engine, "viewer@example.com", OTHER_OWNER);
    fx.engine
        .set_sharing(
            SITE_KEY,
            "hello",
            &SharingRequest {
                visibility: Some("shared".into()),
                confirm_public: false,
                add_emails: vec!["viewer@example.com".into()],
                remove_emails: vec![],
            },
            NOW + 1,
        )
        .unwrap();

    let manifest = manifest_for(&[("/index.html", b"x")]);
    let result = fx.engine.begin_publish_as_email(
        OTHER_OWNER,
        "viewer@example.com",
        "hello",
        &manifest,
        false,
        None,
        None,
        NOW + 2,
    );
    assert!(matches!(result, Err(EngineError::NotAuthorized)));
}

#[test]
fn removed_editor_cannot_start_or_replay_publish() {
    let mut fx = fixture();
    fx.engine
        .claim_with_owner_email(OWNER, "hello", SITE_KEY, Some("paul@finite.vip"), NOW)
        .unwrap();
    verify_email_key(&mut fx.engine, "skyler_bot@finite.vip", OTHER_OWNER);
    fx.engine
        .update_editors(
            SITE_KEY,
            "hello",
            &EditorsRequest {
                actor_email: None,
                add_emails: vec!["skyler_bot@finite.vip".into()],
                remove_emails: vec![],
            },
            NOW + 1,
        )
        .unwrap();

    let content: &[u8] = b"<h1>pending</h1>";
    let manifest = manifest_for(&[("/index.html", content)]);
    let pending = fx
        .engine
        .begin_publish_as_email(
            OTHER_OWNER,
            "skyler_bot@finite.vip",
            "hello",
            &manifest,
            false,
            None,
            None,
            NOW + 2,
        )
        .unwrap();

    fx.engine
        .update_editors(
            SITE_KEY,
            "hello",
            &EditorsRequest {
                actor_email: None,
                add_emails: vec![],
                remove_emails: vec!["skyler_bot@finite.vip".into()],
            },
            NOW + 3,
        )
        .unwrap();

    let start_after_remove = fx.engine.begin_publish_as_email(
        OTHER_OWNER,
        "skyler_bot@finite.vip",
        "hello",
        &manifest,
        false,
        None,
        None,
        NOW + 4,
    );
    assert!(matches!(
        start_after_remove,
        Err(EngineError::NotAuthorized)
    ));

    let upload_replay = fx.engine.upload_blob_as_actor(
        OTHER_OWNER,
        &pending.publish_id,
        &sha(content),
        content,
        NOW + 4,
    );
    assert!(matches!(upload_replay, Err(EngineError::NotAuthorized)));

    let finalize_replay =
        fx.engine
            .finalize_publish_as_actor(OTHER_OWNER, &pending.publish_id, NOW + 4);
    assert!(matches!(finalize_replay, Err(EngineError::NotAuthorized)));
}

#[test]
fn publish_with_source_requires_source_blob_before_finalize() {
    let mut fx = fixture();
    fx.engine.claim(OWNER, "hello", SITE_KEY, NOW).unwrap();
    let content: &[u8] = b"<h1>hello</h1>";
    let source_bytes: &[u8] = b"source";
    let source_request = SourceSnapshotRequest {
        sha256: sha(source_bytes),
        size: source_bytes.len() as u64,
    };
    let manifest = manifest_for(&[("/index.html", content)]);
    let begun = fx
        .engine
        .begin_publish_with_source(SITE_KEY, &manifest, false, None, Some(&source_request), NOW)
        .unwrap();
    fx.engine
        .upload_blob(SITE_KEY, &begun.publish_id, &sha(content), content, NOW)
        .unwrap();
    let result = fx.engine.finalize_publish(SITE_KEY, &begun.publish_id, NOW);
    assert!(matches!(
        result,
        Err(EngineError::Conflict("publish has missing source blob"))
    ));
}

// ---- viewing and magic links ------------------------------------------------

#[test]
fn public_site_is_viewable_without_cookie() {
    let mut fx = fixture();
    publish_site(&mut fx.engine, "hello", SITE_KEY);
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
            NOW,
        )
        .unwrap();
    let site = fx.engine.resolve_site("hello").unwrap().unwrap();
    assert_eq!(
        fx.engine.view_access(&site, None, NOW).unwrap(),
        ViewAccess::Allowed
    );
}

#[test]
fn shared_site_full_magic_link_flow() {
    let mut fx = fixture();
    publish_site(&mut fx.engine, "hello", SITE_KEY);
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

    // No cookie: needs login.
    assert_eq!(
        fx.engine.view_access(&site, None, NOW).unwrap(),
        ViewAccess::NeedsLogin
    );

    // Unshared email gets no link (generic response).
    assert!(
        fx.engine
            .request_login("hello", "stranger@example.com", NOW)
            .unwrap()
            .is_none()
    );
    // Private sites never issue links.
    assert!(
        fx.engine
            .request_login("ghost", "friend@example.com", NOW)
            .unwrap()
            .is_none()
    );

    // Shared email gets a link.
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

    // Token is single use.
    assert!(matches!(
        fx.engine.redeem_login(&token, NOW + 61),
        Err(EngineError::Validation(_))
    ));

    // Revoking the email revokes access even with a live cookie.
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
fn login_tokens_expire() {
    let mut fx = fixture();
    publish_site(&mut fx.engine, "hello", SITE_KEY);
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

#[test]
fn unpublished_site_is_never_viewable() {
    let mut fx = fixture();
    fx.engine.claim(OWNER, "hello", SITE_KEY, NOW).unwrap();
    let site = fx.engine.resolve_site("hello").unwrap().unwrap();
    assert_eq!(
        fx.engine.view_access(&site, None, NOW).unwrap(),
        ViewAccess::NeedsLogin
    );
    assert!(fx.engine.lookup_file(&site, "/").unwrap().is_none());
}

// ---- listing / status -----------------------------------------------------

#[test]
fn list_and_status_respect_ownership() {
    let mut fx = fixture();
    publish_site(&mut fx.engine, "hello", SITE_KEY);

    let sites = fx.engine.list_sites(OWNER).unwrap();
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].name, "hello");
    assert_eq!(sites[0].kind, "static");
    assert_eq!(sites[0].active_version, Some(1));
    assert!(fx.engine.list_sites(OTHER_OWNER).unwrap().is_empty());

    let by_owner = fx.engine.site_status(OWNER, "hello").unwrap();
    assert_eq!(by_owner.status, "published");
    let by_site_key = fx.engine.site_status(SITE_KEY, "hello").unwrap();
    assert_eq!(by_site_key.site_id, by_owner.site_id);
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

// ---- spa fallback -----------------------------------------------------------

#[test]
fn spa_publish_routes_unknown_paths_to_index() {
    let mut fx = fixture();
    fx.engine.claim(OWNER, "hello", SITE_KEY, NOW).unwrap();
    let index: &[u8] = b"<div id=app></div>";
    let manifest = manifest_for(&[("/index.html", index), ("/assets/app.js", b"render()")]);
    let begun = fx
        .engine
        .begin_publish(SITE_KEY, &manifest, true, None, NOW)
        .unwrap();
    fx.engine
        .upload_blob(SITE_KEY, &begun.publish_id, &sha(index), index, NOW)
        .unwrap();
    fx.engine
        .upload_blob(
            SITE_KEY,
            &begun.publish_id,
            &sha(b"render()"),
            b"render()",
            NOW,
        )
        .unwrap();
    fx.engine
        .finalize_publish(SITE_KEY, &begun.publish_id, NOW)
        .unwrap();

    let site = fx.engine.resolve_site("hello").unwrap().unwrap();
    assert!(site.active_version_spa);

    // Real files still serve exactly.
    let asset = fx
        .engine
        .lookup_file(&site, "/assets/app.js")
        .unwrap()
        .unwrap();
    assert_eq!(asset.path, "/assets/app.js");
    // Virtual client-side routes fall back to the app shell.
    let virtual_route = fx
        .engine
        .lookup_file(&site, "/settings/profile")
        .unwrap()
        .unwrap();
    assert_eq!(virtual_route.path, "/index.html");
}

#[test]
fn non_spa_publish_keeps_missing_paths_missing() {
    let mut fx = fixture();
    publish_site(&mut fx.engine, "hello", SITE_KEY);
    let site = fx.engine.resolve_site("hello").unwrap().unwrap();
    assert!(!site.active_version_spa);
    assert!(
        fx.engine
            .lookup_file(&site, "/settings/profile")
            .unwrap()
            .is_none()
    );
}

#[test]
fn spa_publish_requires_root_index() {
    let mut fx = fixture();
    fx.engine.claim(OWNER, "hello", SITE_KEY, NOW).unwrap();
    let manifest = manifest_for(&[("/main.html", b"<p>not an index</p>")]);
    let rejected = fx
        .engine
        .begin_publish(SITE_KEY, &manifest, true, None, NOW);
    assert!(matches!(rejected, Err(EngineError::Validation(_))));
}

// ---- app sites (tier 2) -------------------------------------------------------

fn publish_app(
    engine: &mut Engine,
    name: &str,
    site_key: &str,
    start: &str,
) -> crate::FinalizeOutcome {
    engine.claim(OWNER, name, site_key, NOW).unwrap();
    let bundle: &[u8] = b"pretend-tarball-bytes";
    let manifest = manifest_for(&[("/app.tar.gz", bundle)]);
    let begun = engine
        .begin_publish(site_key, &manifest, false, Some(start), NOW)
        .unwrap();
    engine
        .upload_blob(site_key, &begun.publish_id, &sha(bundle), bundle, NOW)
        .unwrap();
    engine
        .finalize_publish(site_key, &begun.publish_id, NOW)
        .unwrap()
}

#[test]
fn app_publish_returns_deploy_info() {
    let mut fx = fixture();
    let outcome = publish_app(&mut fx.engine, "myapp", SITE_KEY, "node server.js");
    let deploy = outcome.app.expect("app publish carries deploy info");
    assert_eq!(deploy.port, 21000);
    assert_eq!(deploy.start_command, "node server.js");
    assert_eq!(deploy.bundle_sha256, sha(b"pretend-tarball-bytes"));

    // Reconciliation sees the same deploy.
    let deploys = fx.engine.app_deploys().unwrap();
    assert_eq!(deploys.len(), 1);
    assert_eq!(deploys[0].port, 21000);
    assert_eq!(fx.engine.site_status(OWNER, "myapp").unwrap().kind, "app");

    // Static publishes carry no deploy info.
    let static_outcome = publish_site(&mut fx.engine, "plain", OTHER_SITE_KEY);
    assert!(static_outcome.app.is_none());
}

#[test]
fn app_manifest_rules_are_enforced() {
    let mut fx = fixture();
    fx.engine.claim(OWNER, "myapp", SITE_KEY, NOW).unwrap();

    // Wrong manifest shape.
    let multi = manifest_for(&[("/app.tar.gz", b"x"), ("/extra.txt", b"y")]);
    let rejected = fx
        .engine
        .begin_publish(SITE_KEY, &multi, false, Some("node server.js"), NOW);
    assert!(matches!(rejected, Err(EngineError::Validation(_))));

    // Apps and the spa flag are mutually exclusive.
    let bundle_only = manifest_for(&[("/app.tar.gz", b"x")]);
    let spa_app =
        fx.engine
            .begin_publish(SITE_KEY, &bundle_only, true, Some("node server.js"), NOW);
    assert!(matches!(spa_app, Err(EngineError::Validation(_))));

    // Bad commands.
    for bad in ["", "   ", "evil\ncommand", &"x".repeat(2000)] {
        let result = fx
            .engine
            .begin_publish(SITE_KEY, &bundle_only, false, Some(bad), NOW);
        assert!(matches!(result, Err(EngineError::Validation(_))), "{bad:?}");
    }
}

#[test]
fn site_kind_cannot_change_after_first_publish() {
    let mut fx = fixture();
    publish_app(&mut fx.engine, "myapp", SITE_KEY, "node server.js");

    // Static publish onto an app site is rejected.
    let static_manifest = manifest_for(&[("/index.html", b"<h1>hi</h1>")]);
    let to_static = fx
        .engine
        .begin_publish(SITE_KEY, &static_manifest, false, None, NOW + 1);
    assert!(matches!(to_static, Err(EngineError::Conflict(_))));

    // App publish onto a static site is rejected.
    publish_site(&mut fx.engine, "plain", OTHER_SITE_KEY);
    let bundle = manifest_for(&[("/app.tar.gz", b"x")]);
    let to_app =
        fx.engine
            .begin_publish(OTHER_SITE_KEY, &bundle, false, Some("node x.js"), NOW + 1);
    assert!(matches!(to_app, Err(EngineError::Conflict(_))));
}
