//! End-to-end test: a real finitesitesd server on an ephemeral port, driven
//! over HTTP exactly the way `fsite` and a browser would drive it —
//! NIP-98-signed API calls, Host-routed site requests, magic-link login.

// Test helpers return ureq's own error so assertions can match on exact
// HTTP statuses; its size does not matter in a test binary.
#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use sha2::{Digest, Sha256};

use finitesites_blob::BlobStore;
use finitesites_engine::{Engine, EngineConfig};
use finitesites_proto::dto::{
    ClaimRequest, ClaimResponse, EditorsRequest, EmailLoginRequest, EmailLoginResponse,
    EmailRedeemRequest, EmailRedeemResponse, GitAuthRequest, GitAuthResponse, ProjectApplyRequest,
    ProjectApplyResponse, ProjectCollaboratorRemoveRequest, ProjectCollaboratorRemoveResponse,
    ProjectCollaboratorSpec, PublishBeginRequest, PublishBeginResponse, PublishFinalizeResponse,
    SharingRequest, SharingResponse, SiteSummary, SourceSnapshotRequest,
};
use finitesites_proto::project_config::{
    ProjectConfig, ProjectOutputConfig, ProjectOutputKind, ProjectSection,
};
use finitesites_proto::{ManifestFile, PublishManifest, hex, nip98};
use finitesites_store::Store;
use finitesitesd::mailer::DevMailer;
use finitesitesd::{ServeOptions, server};

const BASE_DOMAIN: &str = "sites.localhost";

fn user_secret() -> [u8; 32] {
    let mut secret = [0u8; 32];
    secret[31] = 11;
    secret
}

fn site_secret() -> [u8; 32] {
    let mut secret = [0u8; 32];
    secret[31] = 22;
    secret
}

fn stranger_secret() -> [u8; 32] {
    let mut secret = [0u8; 32];
    secret[31] = 33;
    secret
}

fn now_unix() -> u64 {
    time::OffsetDateTime::now_utc().unix_timestamp() as u64
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(&Sha256::digest(bytes))
}

/// ureq agent that resolves every hostname to the test server. This is what
/// wildcard DNS does in production.
fn agent_for(addr: SocketAddr) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        .redirects(0)
        .resolver(move |netloc: &str| {
            let port = netloc
                .rsplit_once(':')
                .and_then(|(_, p)| p.parse::<u16>().ok())
                .unwrap_or(80);
            Ok(vec![SocketAddr::new(addr.ip(), port)])
        })
        .build()
}

struct TestServer {
    agent: ureq::Agent,
    api_url: String,
    outbox: std::path::PathBuf,
    data_dir: tempfile::TempDir,
}

impl TestServer {
    async fn start(allowed_pubkey: &str) -> TestServer {
        Self::start_with_git_auto_reconcile(allowed_pubkey, true).await
    }

    async fn start_with_git_auto_reconcile(
        allowed_pubkey: &str,
        git_auto_reconcile: bool,
    ) -> TestServer {
        let data_dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&data_dir.path().join("registry.db")).unwrap();
        store
            .allow_pubkey(allowed_pubkey, "e2e", now_unix())
            .unwrap();
        let blobs = BlobStore::open(&data_dir.path().join("blobs")).unwrap();
        let outbox = data_dir.path().join("outbox");
        let mailer = DevMailer::new(outbox.clone()).unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Engine::new(
            store,
            blobs,
            [9u8; 32],
            EngineConfig {
                base_domain: BASE_DOMAIN.to_string(),
                site_url_scheme: "http".to_string(),
                site_url_port: Some(addr.port()),
            },
        );
        let options = ServeOptions {
            data_dir: data_dir.path().to_path_buf(),
            listen: addr,
            base_domain: BASE_DOMAIN.to_string(),
            api_url: format!("http://127.0.0.1:{}", addr.port()),
            git_base_url: format!("http://git.{BASE_DOMAIN}:{}", addr.port()),
            git_hook_helper_path: hook_helper_path(),
            git_auto_reconcile,
            site_url_scheme: "http".to_string(),
            site_url_port: Some(addr.port()),
            mail_provider: None,
            mail_from: None,
            app_runner_kind: finitesitesd::AppRunnerKind::Disabled,
            idle_timeout_seconds: 900,
        };
        let api_url = options.api_url.clone();
        tokio::spawn(async move {
            server::serve_on(
                listener,
                engine,
                Box::new(mailer),
                finitesitesd::apps::Supervisor::new(
                    Box::new(finitesitesd::apps::DisabledRunner),
                    900,
                ),
                options,
            )
            .await
            .expect("test server runs");
        });

        TestServer {
            agent: agent_for(addr),
            api_url,
            outbox,
            data_dir,
        }
    }

    fn data_dir(&self) -> &Path {
        self.data_dir.path()
    }

    fn signed(
        &self,
        secret: &[u8; 32],
        method: &str,
        path: &str,
        body: Option<&[u8]>,
    ) -> Result<ureq::Response, ureq::Error> {
        let url = format!("{}{path}", self.api_url);
        let header = nip98::build_auth_header(secret, &url, method, body, now_unix()).unwrap();
        let request = self
            .agent
            .request(method, &url)
            .set("Authorization", &header);
        match body {
            Some(bytes) => request.send_bytes(bytes),
            None => request.call(),
        }
    }

    fn site_get(&self, name: &str, path: &str, port: u16) -> Result<ureq::Response, ureq::Error> {
        self.agent
            .get(&format!("http://{name}.{BASE_DOMAIN}:{port}{path}"))
            .call()
    }

    fn port(&self) -> u16 {
        self.api_url.rsplit_once(':').unwrap().1.parse().unwrap()
    }
}

fn hook_helper_path() -> PathBuf {
    if let Some(path) = option_env!("CARGO_BIN_EXE_finitesitesd") {
        return PathBuf::from(path);
    }
    let current = std::env::current_exe().unwrap();
    let debug_dir = current
        .parent()
        .and_then(Path::parent)
        .expect("test binary lives under target/debug/deps");
    let name = if cfg!(windows) {
        "finitesitesd.exe"
    } else {
        "finitesitesd"
    };
    let candidate = debug_dir.join(name);
    assert!(
        candidate.exists(),
        "finitesitesd hook helper binary missing at {}",
        candidate.display()
    );
    candidate
}

fn json_body<T: serde::de::DeserializeOwned>(response: ureq::Response) -> T {
    response.into_json().unwrap()
}

fn outbox_link(outbox: &Path) -> String {
    let entries: Vec<_> = std::fs::read_dir(outbox).unwrap().collect();
    assert_eq!(entries.len(), 1, "expected exactly one dev mail");
    let path = entries[0].as_ref().unwrap().path();
    let content = std::fs::read_to_string(path).unwrap();
    content
        .lines()
        .find(|line| line.starts_with("http"))
        .expect("mail contains a link")
        .to_string()
}

fn outbox_email_token(outbox: &Path) -> String {
    let entries: Vec<_> = std::fs::read_dir(outbox).unwrap().collect();
    assert_eq!(entries.len(), 1, "expected exactly one dev mail");
    let path = entries[0].as_ref().unwrap().path();
    let content = std::fs::read_to_string(path).unwrap();
    content
        .lines()
        .find_map(|line| line.trim().strip_prefix("fsite email-redeem "))
        .and_then(|rest| rest.split_whitespace().nth(1))
        .expect("mail contains an email-redeem command")
        .to_string()
}

fn clear_outbox(outbox: &Path) {
    for entry in std::fs::read_dir(outbox).unwrap() {
        std::fs::remove_file(entry.unwrap().path()).unwrap();
    }
}

fn publish_static_version(
    server: &TestServer,
    secret: &[u8; 32],
    name: &str,
    files: Vec<(&str, &[u8])>,
    source: Option<&[u8]>,
) -> PublishFinalizeResponse {
    let manifest = PublishManifest {
        files: files
            .iter()
            .map(|(path, bytes)| ManifestFile {
                path: (*path).to_string(),
                sha256: sha(bytes),
                size: bytes.len() as u64,
            })
            .collect(),
    };
    let source_request = source.map(|bytes| SourceSnapshotRequest {
        sha256: sha(bytes),
        size: bytes.len() as u64,
    });
    let begin_body = serde_json::to_vec(&PublishBeginRequest {
        manifest,
        spa: false,
        start_command: None,
        actor_email: None,
        source: source_request,
    })
    .unwrap();
    let begun: PublishBeginResponse = json_body(
        server
            .signed(
                secret,
                "POST",
                &format!("/api/v1/sites/{name}/publish"),
                Some(&begin_body),
            )
            .unwrap(),
    );

    // Bounded by the manifest size accepted by the server.
    for (_, bytes) in &files {
        server
            .signed(
                secret,
                "PUT",
                &format!(
                    "/api/v1/publishes/{}/blobs/{}",
                    begun.publish_id,
                    sha(bytes)
                ),
                Some(*bytes),
            )
            .unwrap();
    }
    if let Some(bytes) = source {
        server
            .signed(
                secret,
                "PUT",
                &format!(
                    "/api/v1/publishes/{}/blobs/{}",
                    begun.publish_id,
                    sha(bytes)
                ),
                Some(bytes),
            )
            .unwrap();
    }

    json_body(
        server
            .signed(
                secret,
                "POST",
                &format!("/api/v1/publishes/{}/finalize", begun.publish_id),
                None,
            )
            .unwrap(),
    )
}

fn run_git(args: &[&str], cwd: Option<&Path>) {
    let mut command = Command::new("git");
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_git_expect_failure(args: &[&str], cwd: Option<&Path>) {
    let mut command = Command::new("git");
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let output = command.output().unwrap();
    assert!(
        !output.status.success(),
        "git {:?} unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn wait_for_active_version(server: &TestServer, name: &str, expected: Option<u32>) -> SiteSummary {
    let mut last: Option<SiteSummary> = None;
    // Bounded wait: the receive-pack request has already completed; this only
    // waits for the out-of-band reconciler spawned after that durable event.
    for _ in 0..60 {
        let summary: SiteSummary = json_body(
            server
                .signed(
                    &user_secret(),
                    "GET",
                    &format!("/api/v1/sites/{name}"),
                    None,
                )
                .unwrap(),
        );
        if summary.active_version == expected {
            return summary;
        }
        last = Some(summary);
        std::thread::sleep(Duration::from_millis(50));
    }
    let summary = last.expect("site summary was fetched at least once");
    assert_eq!(summary.active_version, expected);
    summary
}

fn project_apply_request(dry_run: bool) -> ProjectApplyRequest {
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

#[tokio::test(flavor = "multi_thread")]
async fn full_publish_share_and_view_flow() {
    let user_pubkey = finitesites_proto::event::pubkey_for_secret(&user_secret()).unwrap();
    let server = TestServer::start(&user_pubkey).await;
    let port = server.port();

    let task = tokio::task::spawn_blocking(move || {
        // Health check needs no auth.
        let health = server
            .agent
            .get(&format!("{}/api/v1/healthz", server.api_url))
            .call();
        assert!(health.is_ok());

        // Claim with the user key, registering the site key.
        let site_pubkey = finitesites_proto::event::pubkey_for_secret(&site_secret()).unwrap();
        let claim_body = serde_json::to_vec(&ClaimRequest {
            name: "hello".into(),
            site_pubkey: site_pubkey.clone(),
            owner_email: None,
        })
        .unwrap();
        let claim: ClaimResponse = json_body(
            server
                .signed(
                    &user_secret(),
                    "POST",
                    "/api/v1/sites/claim",
                    Some(&claim_body),
                )
                .unwrap(),
        );
        assert!(!claim.already_claimed);
        assert_eq!(claim.url, format!("http://hello.{BASE_DOMAIN}:{port}/"));

        // A key without a publish grant cannot claim.
        let stranger_claim = serde_json::to_vec(&ClaimRequest {
            name: "intruder".into(),
            site_pubkey: "44".repeat(32),
            owner_email: None,
        })
        .unwrap();
        let denied = server.signed(
            &stranger_secret(),
            "POST",
            "/api/v1/sites/claim",
            Some(&stranger_claim),
        );
        assert!(matches!(denied, Err(ureq::Error::Status(403, _))));

        // A garbage Authorization header is rejected.
        let no_auth = server
            .agent
            .post(&format!("{}/api/v1/sites/claim", server.api_url))
            .set("Authorization", "Nostr bm90LWFuLWV2ZW50")
            .send_bytes(&claim_body);
        assert!(matches!(no_auth, Err(ureq::Error::Status(401, _))));

        // Unpublished site renders the placeholder.
        let placeholder = server.site_get("hello", "/", port).unwrap();
        assert!(placeholder.into_string().unwrap().contains("claimed"));

        // Publish: begin, upload missing blobs, finalize.
        let index_html: &[u8] = b"<h1>hello from finite</h1>";
        let style_css: &[u8] = b"body { background: black }";
        let manifest = PublishManifest {
            files: vec![
                ManifestFile {
                    path: "/index.html".into(),
                    sha256: sha(index_html),
                    size: index_html.len() as u64,
                },
                ManifestFile {
                    path: "/css/style.css".into(),
                    sha256: sha(style_css),
                    size: style_css.len() as u64,
                },
            ],
        };
        let begin_body = serde_json::to_vec(&PublishBeginRequest {
            manifest: manifest.clone(),
            spa: false,
            start_command: None,
            actor_email: None,
            source: None,
        })
        .unwrap();
        let begun: PublishBeginResponse = json_body(
            server
                .signed(
                    &site_secret(),
                    "POST",
                    "/api/v1/sites/hello/publish",
                    Some(&begin_body),
                )
                .unwrap(),
        );
        assert_eq!(begun.missing.len(), 2);

        for (blob, digest) in [(index_html, sha(index_html)), (style_css, sha(style_css))] {
            server
                .signed(
                    &site_secret(),
                    "PUT",
                    &format!("/api/v1/publishes/{}/blobs/{digest}", begun.publish_id),
                    Some(blob),
                )
                .unwrap();
        }
        let finalized: PublishFinalizeResponse = json_body(
            server
                .signed(
                    &site_secret(),
                    "POST",
                    &format!("/api/v1/publishes/{}/finalize", begun.publish_id),
                    None,
                )
                .unwrap(),
        );
        assert_eq!(finalized.version_number, 1);
        assert_eq!(finalized.path_count, 2);

        // Default visibility is private: viewing demands login.
        let gated = server.site_get("hello", "/", port);
        let Err(ureq::Error::Status(401, response)) = gated else {
            panic!("expected 401 for private site");
        };
        assert!(response.into_string().unwrap().contains("private"));

        // Share with one email.
        let share_body = serde_json::to_vec(&SharingRequest {
            visibility: Some("shared".into()),
            confirm_public: false,
            add_emails: vec!["friend@example.com".into()],
            remove_emails: vec![],
        })
        .unwrap();
        let shared: SharingResponse = json_body(
            server
                .signed(
                    &site_secret(),
                    "POST",
                    "/api/v1/sites/hello/sharing",
                    Some(&share_body),
                )
                .unwrap(),
        );
        assert_eq!(shared.shared_emails, vec!["friend@example.com"]);

        // Request a magic link as the shared email; the dev mailer drops it
        // in the outbox. An unshared email gets the same generic page and
        // no mail.
        let site_base = format!("http://hello.{BASE_DOMAIN}:{port}");
        let generic = server
            .agent
            .post(&format!("{site_base}/_finite/request-link"))
            .send_form(&[("email", "stranger@example.com")])
            .unwrap();
        assert!(generic.into_string().unwrap().contains("Check your email"));
        assert_eq!(std::fs::read_dir(&server.outbox).unwrap().count(), 0);

        server
            .agent
            .post(&format!("{site_base}/_finite/request-link"))
            .send_form(&[("email", "friend@example.com")])
            .unwrap();
        let link = outbox_link(&server.outbox);
        assert!(link.starts_with(&format!("{site_base}/_finite/auth?token=")));

        // Redeem the link: cookie set, redirect home.
        let redeemed = server.agent.get(&link).call().unwrap();
        assert_eq!(redeemed.status(), 303);
        let cookie = redeemed
            .header("set-cookie")
            .expect("login sets a cookie")
            .split(';')
            .next()
            .unwrap()
            .to_string();

        // The link is single-use.
        let replayed = server.agent.get(&link).call();
        assert!(matches!(replayed, Err(ureq::Error::Status(400, _))));

        // Per-email rate limit: 3 links per window. One was already sent;
        // two more go through, the fourth renders the same generic page
        // but sends nothing.
        for _ in 0..3 {
            server
                .agent
                .post(&format!("{site_base}/_finite/request-link"))
                .send_form(&[("email", "friend@example.com")])
                .unwrap();
        }
        assert_eq!(
            std::fs::read_dir(&server.outbox).unwrap().count(),
            3,
            "fourth request must not send a fourth mail"
        );

        // With the cookie, content serves; folder fallback and 404 behave.
        let page = server
            .agent
            .get(&format!("{site_base}/"))
            .set("Cookie", &cookie)
            .call()
            .unwrap();
        assert_eq!(
            page.header("content-type").unwrap(),
            "text/html; charset=utf-8"
        );
        let etag = page.header("etag").unwrap().to_string();
        assert_eq!(page.into_string().unwrap(), "<h1>hello from finite</h1>");

        let revalidated = server
            .agent
            .get(&format!("{site_base}/"))
            .set("Cookie", &cookie)
            .set("If-None-Match", &etag)
            .call()
            .unwrap();
        assert_eq!(revalidated.status(), 304);

        let css = server
            .agent
            .get(&format!("{site_base}/css/style.css"))
            .set("Cookie", &cookie)
            .call()
            .unwrap();
        assert_eq!(
            css.header("content-type").unwrap(),
            "text/css; charset=utf-8"
        );

        let missing = server
            .agent
            .get(&format!("{site_base}/nope.html"))
            .set("Cookie", &cookie)
            .call();
        assert!(matches!(missing, Err(ureq::Error::Status(404, _))));

        // Make it public (with confirmation): no cookie needed anymore.
        let public_body = serde_json::to_vec(&SharingRequest {
            visibility: Some("public".into()),
            confirm_public: true,
            add_emails: vec![],
            remove_emails: vec![],
        })
        .unwrap();
        server
            .signed(
                &site_secret(),
                "POST",
                "/api/v1/sites/hello/sharing",
                Some(&public_body),
            )
            .unwrap();
        let open = server.site_get("hello", "/", port).unwrap();
        assert_eq!(open.into_string().unwrap(), "<h1>hello from finite</h1>");

        // Unknown subdomains render the unknown-site page.
        let unknown = server.site_get("ghost", "/", port);
        let Err(ureq::Error::Status(404, response)) = unknown else {
            panic!("expected 404 for unknown site");
        };
        assert!(
            response
                .into_string()
                .unwrap()
                .contains("No site lives here")
        );
    });
    task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn project_apply_and_git_auth_flow() {
    let user_pubkey = finitesites_proto::event::pubkey_for_secret(&user_secret()).unwrap();
    let server = TestServer::start(&user_pubkey).await;

    let task = tokio::task::spawn_blocking(move || {
        let dry_body = serde_json::to_vec(&project_apply_request(true)).unwrap();
        let dry_run: ProjectApplyResponse = json_body(
            server
                .signed(
                    &user_secret(),
                    "POST",
                    "/api/v1/projects/apply",
                    Some(&dry_body),
                )
                .unwrap(),
        );
        assert!(dry_run.dry_run);
        assert!(dry_run.created);
        assert_eq!(dry_run.project_id, None);
        assert_eq!(
            dry_run.git_remote_url,
            format!(
                "http://git.{BASE_DOMAIN}:{}/finitechat-native.git",
                server.port()
            )
        );
        assert!(dry_run.finite_toml.contains("[outputs.mockup]"));

        let body = serde_json::to_vec(&project_apply_request(false)).unwrap();
        let created: ProjectApplyResponse = json_body(
            server
                .signed(
                    &user_secret(),
                    "POST",
                    "/api/v1/projects/apply",
                    Some(&body),
                )
                .unwrap(),
        );
        assert!(!created.dry_run);
        assert!(created.created);
        assert!(created.project_id.is_some());
        assert!(created.outputs[0].created);
        assert_eq!(created.outputs[0].site_name, "finitechat-native-mockup");

        let replay: ProjectApplyResponse = json_body(
            server
                .signed(
                    &user_secret(),
                    "POST",
                    "/api/v1/projects/apply",
                    Some(&body),
                )
                .unwrap(),
        );
        assert!(!replay.created);
        assert!(!replay.outputs[0].created);
        assert_eq!(replay.project_id, created.project_id);

        let bad_auth = serde_json::to_vec(&GitAuthRequest {
            email: "skyler@example.com".into(),
        })
        .unwrap();
        let unverified = server
            .signed(
                &stranger_secret(),
                "POST",
                "/api/v1/projects/finitechat-native/git-auth",
                Some(&bad_auth),
            )
            .unwrap_err();
        assert!(matches!(unverified, ureq::Error::Status(403, _)));

        let login_body = serde_json::to_vec(&EmailLoginRequest {
            email: "skyler@example.com".into(),
        })
        .unwrap();
        let login: EmailLoginResponse = server
            .agent
            .post(&format!("{}/api/v1/email-auth/request", server.api_url))
            .set("Content-Type", "application/json")
            .send_bytes(&login_body)
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(login.email, "skyler@example.com");
        let token = outbox_email_token(&server.outbox);
        clear_outbox(&server.outbox);

        let redeem_body = serde_json::to_vec(&EmailRedeemRequest {
            email: "skyler@example.com".into(),
            token,
        })
        .unwrap();
        let redeemed: EmailRedeemResponse = json_body(
            server
                .signed(
                    &stranger_secret(),
                    "POST",
                    "/api/v1/email-auth/redeem",
                    Some(&redeem_body),
                )
                .unwrap(),
        );
        assert_eq!(redeemed.email, "skyler@example.com");

        let credential: GitAuthResponse = json_body(
            server
                .signed(
                    &stranger_secret(),
                    "POST",
                    "/api/v1/projects/finitechat-native/git-auth",
                    Some(&bad_auth),
                )
                .unwrap(),
        );
        assert_eq!(credential.project_slug, "finitechat-native");
        assert_eq!(credential.username, credential.credential_id);
        assert_eq!(credential.password.len(), 64);
    });
    task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn project_collaborator_remove_revokes_git_credentials() {
    let user_pubkey = finitesites_proto::event::pubkey_for_secret(&user_secret()).unwrap();
    let server = TestServer::start(&user_pubkey).await;

    let task = tokio::task::spawn_blocking(move || {
        let body = serde_json::to_vec(&project_apply_request(false)).unwrap();
        json_body::<ProjectApplyResponse>(
            server
                .signed(
                    &user_secret(),
                    "POST",
                    "/api/v1/projects/apply",
                    Some(&body),
                )
                .unwrap(),
        );

        let login_body = serde_json::to_vec(&EmailLoginRequest {
            email: "skyler@example.com".into(),
        })
        .unwrap();
        server
            .agent
            .post(&format!("{}/api/v1/email-auth/request", server.api_url))
            .set("Content-Type", "application/json")
            .send_bytes(&login_body)
            .unwrap();
        let token = outbox_email_token(&server.outbox);
        clear_outbox(&server.outbox);
        let redeem_body = serde_json::to_vec(&EmailRedeemRequest {
            email: "skyler@example.com".into(),
            token,
        })
        .unwrap();
        server
            .signed(
                &stranger_secret(),
                "POST",
                "/api/v1/email-auth/redeem",
                Some(&redeem_body),
            )
            .unwrap();

        let auth_body = serde_json::to_vec(&GitAuthRequest {
            email: "skyler@example.com".into(),
        })
        .unwrap();
        let credential: GitAuthResponse = json_body(
            server
                .signed(
                    &stranger_secret(),
                    "POST",
                    "/api/v1/projects/finitechat-native/git-auth",
                    Some(&auth_body),
                )
                .unwrap(),
        );
        let remote = format!(
            "http://{}:{}@127.0.0.1:{}/finitechat-native.git",
            credential.username,
            credential.password,
            server.port()
        );
        let host_header = format!("Host: git.{BASE_DOMAIN}:{}", server.port());
        let dir = tempfile::tempdir().unwrap();
        run_git(
            &[
                "-c",
                &format!("http.extraHeader={host_header}"),
                "ls-remote",
                &remote,
            ],
            Some(dir.path()),
        );

        let remove_body = serde_json::to_vec(&ProjectCollaboratorRemoveRequest {
            email: "skyler@example.com".into(),
        })
        .unwrap();
        let stranger_remove = server
            .signed(
                &stranger_secret(),
                "POST",
                "/api/v1/projects/finitechat-native/collaborators/remove",
                Some(&remove_body),
            )
            .unwrap_err();
        assert!(matches!(stranger_remove, ureq::Error::Status(403, _)));

        let removed: ProjectCollaboratorRemoveResponse = json_body(
            server
                .signed(
                    &user_secret(),
                    "POST",
                    "/api/v1/projects/finitechat-native/collaborators/remove",
                    Some(&remove_body),
                )
                .unwrap(),
        );
        assert_eq!(removed.project_slug, "finitechat-native");
        assert_eq!(removed.email, "skyler@example.com");
        assert!(removed.removed);
        assert_eq!(removed.revoked_git_credentials, 1);

        run_git_expect_failure(
            &[
                "-c",
                &format!("http.extraHeader={host_header}"),
                "ls-remote",
                &remote,
            ],
            Some(dir.path()),
        );
        let auth_after_remove = server
            .signed(
                &stranger_secret(),
                "POST",
                "/api/v1/projects/finitechat-native/git-auth",
                Some(&auth_body),
            )
            .unwrap_err();
        assert!(matches!(auth_after_remove, ureq::Error::Status(403, _)));

        let replay: ProjectCollaboratorRemoveResponse = json_body(
            server
                .signed(
                    &user_secret(),
                    "POST",
                    "/api/v1/projects/finitechat-native/collaborators/remove",
                    Some(&remove_body),
                )
                .unwrap(),
        );
        assert!(!replay.removed);
        assert_eq!(replay.revoked_git_credentials, 0);
    });
    task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn git_http_clone_and_push_with_minted_credential() {
    let user_pubkey = finitesites_proto::event::pubkey_for_secret(&user_secret()).unwrap();
    let server = TestServer::start(&user_pubkey).await;

    let task =
        tokio::task::spawn_blocking(move || {
            let body = serde_json::to_vec(&project_apply_request(false)).unwrap();
            let created: ProjectApplyResponse = json_body(
                server
                    .signed(
                        &user_secret(),
                        "POST",
                        "/api/v1/projects/apply",
                        Some(&body),
                    )
                    .unwrap(),
            );

            let login_body = serde_json::to_vec(&EmailLoginRequest {
                email: "skyler@example.com".into(),
            })
            .unwrap();
            server
                .agent
                .post(&format!("{}/api/v1/email-auth/request", server.api_url))
                .set("Content-Type", "application/json")
                .send_bytes(&login_body)
                .unwrap();
            let token = outbox_email_token(&server.outbox);
            clear_outbox(&server.outbox);
            let redeem_body = serde_json::to_vec(&EmailRedeemRequest {
                email: "skyler@example.com".into(),
                token,
            })
            .unwrap();
            server
                .signed(
                    &stranger_secret(),
                    "POST",
                    "/api/v1/email-auth/redeem",
                    Some(&redeem_body),
                )
                .unwrap();

            let auth_body = serde_json::to_vec(&GitAuthRequest {
                email: "skyler@example.com".into(),
            })
            .unwrap();
            let credential: GitAuthResponse = json_body(
                server
                    .signed(
                        &stranger_secret(),
                        "POST",
                        "/api/v1/projects/finitechat-native/git-auth",
                        Some(&auth_body),
                    )
                    .unwrap(),
            );

            let dir = tempfile::tempdir().unwrap();
            let remote = format!(
                "http://{}:{}@127.0.0.1:{}/finitechat-native.git",
                credential.username,
                credential.password,
                server.port()
            );
            let host_header = format!("Host: git.{BASE_DOMAIN}:{}", server.port());
            run_git(
                &[
                    "-c",
                    &format!("http.extraHeader={host_header}"),
                    "clone",
                    &remote,
                    "repo",
                ],
                Some(dir.path()),
            );
            let repo = dir.path().join("repo");
            run_git(&["checkout", "-b", "main"], Some(&repo));
            std::fs::write(repo.join("finite.toml"), created.finite_toml).unwrap();
            std::fs::write(repo.join("index.html"), "<h1>from git</h1>").unwrap();
            run_git(&["add", "finite.toml", "index.html"], Some(&repo));
            run_git(
                &[
                    "-c",
                    "user.email=skyler@example.com",
                    "-c",
                    "user.name=Skyler Bot",
                    "commit",
                    "-m",
                    "Initial project output",
                ],
                Some(&repo),
            );
            run_git(
                &[
                    "-c",
                    &format!("http.extraHeader={host_header}"),
                    "push",
                    "origin",
                    "main",
                ],
                Some(&repo),
            );

            let summary = wait_for_active_version(&server, "finitechat-native-mockup", Some(1));
            assert_eq!(summary.active_version, Some(1));

            let llms = server
                .site_get("finitechat-native-mockup", "/llms.txt", server.port())
                .unwrap()
                .into_string()
                .unwrap();
            assert!(llms.contains("Project: finitechat-native"));
            assert!(llms.contains(
                "fsite auth git finitechat-native --email YOUR_EDITOR_EMAIL --output json"
            ));
            assert!(llms.contains("git push origin main"));
        });
    task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn git_ref_event_reconciles_after_restart_boundary() {
    let user_pubkey = finitesites_proto::event::pubkey_for_secret(&user_secret()).unwrap();
    let server = TestServer::start_with_git_auto_reconcile(&user_pubkey, false).await;

    let task = tokio::task::spawn_blocking(move || {
        let body = serde_json::to_vec(&project_apply_request(false)).unwrap();
        let created: ProjectApplyResponse = json_body(
            server
                .signed(
                    &user_secret(),
                    "POST",
                    "/api/v1/projects/apply",
                    Some(&body),
                )
                .unwrap(),
        );

        let login_body = serde_json::to_vec(&EmailLoginRequest {
            email: "skyler@example.com".into(),
        })
        .unwrap();
        server
            .agent
            .post(&format!("{}/api/v1/email-auth/request", server.api_url))
            .set("Content-Type", "application/json")
            .send_bytes(&login_body)
            .unwrap();
        let token = outbox_email_token(&server.outbox);
        clear_outbox(&server.outbox);
        let redeem_body = serde_json::to_vec(&EmailRedeemRequest {
            email: "skyler@example.com".into(),
            token,
        })
        .unwrap();
        server
            .signed(
                &stranger_secret(),
                "POST",
                "/api/v1/email-auth/redeem",
                Some(&redeem_body),
            )
            .unwrap();

        let auth_body = serde_json::to_vec(&GitAuthRequest {
            email: "skyler@example.com".into(),
        })
        .unwrap();
        let credential: GitAuthResponse = json_body(
            server
                .signed(
                    &stranger_secret(),
                    "POST",
                    "/api/v1/projects/finitechat-native/git-auth",
                    Some(&auth_body),
                )
                .unwrap(),
        );

        let dir = tempfile::tempdir().unwrap();
        let remote = format!(
            "http://{}:{}@127.0.0.1:{}/finitechat-native.git",
            credential.username,
            credential.password,
            server.port()
        );
        let host_header = format!("Host: git.{BASE_DOMAIN}:{}", server.port());
        run_git(
            &[
                "-c",
                &format!("http.extraHeader={host_header}"),
                "clone",
                &remote,
                "repo",
            ],
            Some(dir.path()),
        );
        let repo = dir.path().join("repo");
        run_git(&["checkout", "-b", "main"], Some(&repo));
        std::fs::write(repo.join("finite.toml"), created.finite_toml).unwrap();
        std::fs::write(repo.join("index.html"), "<h1>after restart</h1>").unwrap();
        run_git(&["add", "finite.toml", "index.html"], Some(&repo));
        run_git(
            &[
                "-c",
                "user.email=skyler@example.com",
                "-c",
                "user.name=Skyler Bot",
                "commit",
                "-m",
                "Durable hook event",
            ],
            Some(&repo),
        );
        run_git(
            &[
                "-c",
                &format!("http.extraHeader={host_header}"),
                "push",
                "origin",
                "main",
            ],
            Some(&repo),
        );

        let summary: SiteSummary = json_body(
            server
                .signed(
                    &user_secret(),
                    "GET",
                    "/api/v1/sites/finitechat-native-mockup",
                    None,
                )
                .unwrap(),
        );
        assert_eq!(summary.active_version, None);

        let data_dir = server.data_dir().to_path_buf();
        {
            let store = Store::open(&data_dir.join("registry.db")).unwrap();
            let pending = store.pending_git_ref_events(None).unwrap();
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].ref_name, "refs/heads/main");
        }

        let store = Store::open(&data_dir.join("registry.db")).unwrap();
        let blobs = BlobStore::open(&data_dir.join("blobs")).unwrap();
        let mut engine = Engine::new(
            store,
            blobs,
            [9u8; 32],
            EngineConfig {
                base_domain: BASE_DOMAIN.to_string(),
                site_url_scheme: "http".to_string(),
                site_url_port: Some(server.port()),
            },
        );
        let processed =
            finitesitesd::git::reconcile_pending_events(&mut engine, &data_dir, None, now_unix())
                .unwrap();
        assert_eq!(processed, 1);
        let replay =
            finitesitesd::git::reconcile_pending_events(&mut engine, &data_dir, None, now_unix())
                .unwrap();
        assert_eq!(replay, 0);

        let summary = wait_for_active_version(&server, "finitechat-native-mockup", Some(1));
        assert_eq!(summary.active_version, Some(1));
    });
    task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn git_push_to_non_deploy_branch_does_not_publish() {
    let user_pubkey = finitesites_proto::event::pubkey_for_secret(&user_secret()).unwrap();
    let server = TestServer::start(&user_pubkey).await;

    let task = tokio::task::spawn_blocking(move || {
        let body = serde_json::to_vec(&project_apply_request(false)).unwrap();
        let created: ProjectApplyResponse = json_body(
            server
                .signed(
                    &user_secret(),
                    "POST",
                    "/api/v1/projects/apply",
                    Some(&body),
                )
                .unwrap(),
        );

        let login_body = serde_json::to_vec(&EmailLoginRequest {
            email: "skyler@example.com".into(),
        })
        .unwrap();
        server
            .agent
            .post(&format!("{}/api/v1/email-auth/request", server.api_url))
            .set("Content-Type", "application/json")
            .send_bytes(&login_body)
            .unwrap();
        let token = outbox_email_token(&server.outbox);
        clear_outbox(&server.outbox);
        let redeem_body = serde_json::to_vec(&EmailRedeemRequest {
            email: "skyler@example.com".into(),
            token,
        })
        .unwrap();
        server
            .signed(
                &stranger_secret(),
                "POST",
                "/api/v1/email-auth/redeem",
                Some(&redeem_body),
            )
            .unwrap();

        let auth_body = serde_json::to_vec(&GitAuthRequest {
            email: "skyler@example.com".into(),
        })
        .unwrap();
        let credential: GitAuthResponse = json_body(
            server
                .signed(
                    &stranger_secret(),
                    "POST",
                    "/api/v1/projects/finitechat-native/git-auth",
                    Some(&auth_body),
                )
                .unwrap(),
        );

        let dir = tempfile::tempdir().unwrap();
        let remote = format!(
            "http://{}:{}@127.0.0.1:{}/finitechat-native.git",
            credential.username,
            credential.password,
            server.port()
        );
        let host_header = format!("Host: git.{BASE_DOMAIN}:{}", server.port());
        let bad_remote = format!(
            "http://{}:{}@127.0.0.1:{}/finitechat-native.git",
            credential.username,
            "badpassword",
            server.port()
        );
        run_git_expect_failure(
            &[
                "-c",
                &format!("http.extraHeader={host_header}"),
                "ls-remote",
                &bad_remote,
            ],
            Some(dir.path()),
        );
        run_git(
            &[
                "-c",
                &format!("http.extraHeader={host_header}"),
                "clone",
                &remote,
                "repo",
            ],
            Some(dir.path()),
        );
        let repo = dir.path().join("repo");
        run_git(&["checkout", "-b", "notes"], Some(&repo));
        std::fs::write(repo.join("finite.toml"), created.finite_toml).unwrap();
        std::fs::write(repo.join("index.html"), "<h1>not deployed</h1>").unwrap();
        run_git(&["add", "finite.toml", "index.html"], Some(&repo));
        run_git(
            &[
                "-c",
                "user.email=skyler@example.com",
                "-c",
                "user.name=Skyler Bot",
                "commit",
                "-m",
                "Push non deploy branch",
            ],
            Some(&repo),
        );
        run_git(
            &[
                "-c",
                &format!("http.extraHeader={host_header}"),
                "push",
                "origin",
                "notes",
            ],
            Some(&repo),
        );

        let summary: SiteSummary = json_body(
            server
                .signed(
                    &user_secret(),
                    "GET",
                    "/api/v1/sites/finitechat-native-mockup",
                    None,
                )
                .unwrap(),
        );
        assert_eq!(summary.active_version, None);
    });
    task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn git_push_with_missing_output_path_does_not_publish() {
    let user_pubkey = finitesites_proto::event::pubkey_for_secret(&user_secret()).unwrap();
    let server = TestServer::start(&user_pubkey).await;

    let task = tokio::task::spawn_blocking(move || {
        let body = serde_json::to_vec(&project_apply_request(false)).unwrap();
        let created: ProjectApplyResponse = json_body(
            server
                .signed(
                    &user_secret(),
                    "POST",
                    "/api/v1/projects/apply",
                    Some(&body),
                )
                .unwrap(),
        );

        let login_body = serde_json::to_vec(&EmailLoginRequest {
            email: "skyler@example.com".into(),
        })
        .unwrap();
        server
            .agent
            .post(&format!("{}/api/v1/email-auth/request", server.api_url))
            .set("Content-Type", "application/json")
            .send_bytes(&login_body)
            .unwrap();
        let token = outbox_email_token(&server.outbox);
        clear_outbox(&server.outbox);
        let redeem_body = serde_json::to_vec(&EmailRedeemRequest {
            email: "skyler@example.com".into(),
            token,
        })
        .unwrap();
        server
            .signed(
                &stranger_secret(),
                "POST",
                "/api/v1/email-auth/redeem",
                Some(&redeem_body),
            )
            .unwrap();
        let auth_body = serde_json::to_vec(&GitAuthRequest {
            email: "skyler@example.com".into(),
        })
        .unwrap();
        let credential: GitAuthResponse = json_body(
            server
                .signed(
                    &stranger_secret(),
                    "POST",
                    "/api/v1/projects/finitechat-native/git-auth",
                    Some(&auth_body),
                )
                .unwrap(),
        );

        let dir = tempfile::tempdir().unwrap();
        let remote = format!(
            "http://{}:{}@127.0.0.1:{}/finitechat-native.git",
            credential.username,
            credential.password,
            server.port()
        );
        let host_header = format!("Host: git.{BASE_DOMAIN}:{}", server.port());
        run_git(
            &[
                "-c",
                &format!("http.extraHeader={host_header}"),
                "clone",
                &remote,
                "repo",
            ],
            Some(dir.path()),
        );
        let repo = dir.path().join("repo");
        run_git(&["checkout", "-b", "main"], Some(&repo));
        let bad_config = created
            .finite_toml
            .replace("path = \".\"", "path = \"dist\"");
        std::fs::write(repo.join("finite.toml"), bad_config).unwrap();
        std::fs::write(repo.join("index.html"), "<h1>not deployed</h1>").unwrap();
        run_git(&["add", "finite.toml", "index.html"], Some(&repo));
        run_git(
            &[
                "-c",
                "user.email=skyler@example.com",
                "-c",
                "user.name=Skyler Bot",
                "commit",
                "-m",
                "Missing output path",
            ],
            Some(&repo),
        );
        run_git(
            &[
                "-c",
                &format!("http.extraHeader={host_header}"),
                "push",
                "origin",
                "main",
            ],
            Some(&repo),
        );

        let summary: SiteSummary = json_body(
            server
                .signed(
                    &user_secret(),
                    "GET",
                    "/api/v1/sites/finitechat-native-mockup",
                    None,
                )
                .unwrap(),
        );
        assert_eq!(summary.active_version, None);
    });
    task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn generated_llms_txt_requires_source_and_respects_user_file() {
    let user_pubkey = finitesites_proto::event::pubkey_for_secret(&user_secret()).unwrap();
    let server = TestServer::start(&user_pubkey).await;
    let port = server.port();

    let task = tokio::task::spawn_blocking(move || {
        let site_pubkey = finitesites_proto::event::pubkey_for_secret(&site_secret()).unwrap();
        let claim_body = serde_json::to_vec(&ClaimRequest {
            name: "agentdocs".into(),
            site_pubkey,
            owner_email: None,
        })
        .unwrap();
        json_body::<ClaimResponse>(
            server
                .signed(
                    &user_secret(),
                    "POST",
                    "/api/v1/sites/claim",
                    Some(&claim_body),
                )
                .unwrap(),
        );

        let editors_body = serde_json::to_vec(&EditorsRequest {
            actor_email: None,
            add_emails: vec!["skyler_bot@finite.vip".into()],
            remove_emails: vec![],
        })
        .unwrap();
        server
            .signed(
                &site_secret(),
                "POST",
                "/api/v1/sites/agentdocs/editors",
                Some(&editors_body),
            )
            .unwrap();

        publish_static_version(
            &server,
            &site_secret(),
            "agentdocs",
            vec![("/index.html", b"<h1>v1</h1>")],
            None,
        );
        let no_source = server.site_get("agentdocs", "/llms.txt", port);
        assert!(matches!(no_source, Err(ureq::Error::Status(401, _))));

        publish_static_version(
            &server,
            &site_secret(),
            "agentdocs",
            vec![("/index.html", b"<h1>v2</h1>")],
            Some(b"source archive v2"),
        );
        let generated = server.site_get("agentdocs", "/llms.txt", port).unwrap();
        assert_eq!(
            generated.header("content-type").unwrap(),
            "text/plain; charset=utf-8"
        );
        assert_eq!(generated.header("cache-control").unwrap(), "no-store");
        let generated_body = generated.into_string().unwrap();
        assert!(generated_body.contains("fsite source pull agentdocs ./site-source"));
        assert!(generated_body.contains("https://github.com/finitecomputer/finite-sites"));
        assert!(!generated_body.contains("skyler_bot@finite.vip"));

        publish_static_version(
            &server,
            &site_secret(),
            "agentdocs",
            vec![
                ("/index.html", b"<h1>v3</h1>"),
                ("/llms.txt", b"custom project instructions"),
            ],
            Some(b"source archive v3"),
        );
        let private_user_file = server.site_get("agentdocs", "/llms.txt", port);
        assert!(matches!(
            private_user_file,
            Err(ureq::Error::Status(401, _))
        ));

        let public_body = serde_json::to_vec(&SharingRequest {
            visibility: Some("public".into()),
            confirm_public: true,
            add_emails: vec![],
            remove_emails: vec![],
        })
        .unwrap();
        server
            .signed(
                &site_secret(),
                "POST",
                "/api/v1/sites/agentdocs/sharing",
                Some(&public_body),
            )
            .unwrap();
        let custom = server.site_get("agentdocs", "/llms.txt", port).unwrap();
        assert_eq!(custom.into_string().unwrap(), "custom project instructions");
    });
    task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn email_editor_publish_and_source_flow() {
    let user_pubkey = finitesites_proto::event::pubkey_for_secret(&user_secret()).unwrap();
    let server = TestServer::start(&user_pubkey).await;
    let port = server.port();

    let task = tokio::task::spawn_blocking(move || {
        let site_pubkey = finitesites_proto::event::pubkey_for_secret(&site_secret()).unwrap();
        let claim_body = serde_json::to_vec(&ClaimRequest {
            name: "collab".into(),
            site_pubkey: site_pubkey.clone(),
            owner_email: Some("paul@finite.vip".into()),
        })
        .unwrap();
        let claim: ClaimResponse = json_body(
            server
                .signed(
                    &user_secret(),
                    "POST",
                    "/api/v1/sites/claim",
                    Some(&claim_body),
                )
                .unwrap(),
        );
        assert_eq!(claim.owner_email.as_deref(), Some("paul@finite.vip"));

        let owner_login_body = serde_json::to_vec(&EmailLoginRequest {
            email: "paul@finite.vip".into(),
        })
        .unwrap();
        json_body::<EmailLoginResponse>(
            server
                .agent
                .post(&format!("{}/api/v1/email-auth/request", server.api_url))
                .set("Content-Type", "application/json")
                .send_bytes(&owner_login_body)
                .unwrap(),
        );
        let owner_token = outbox_email_token(&server.outbox);
        let owner_redeem_body = serde_json::to_vec(&EmailRedeemRequest {
            email: "paul@finite.vip".into(),
            token: owner_token,
        })
        .unwrap();
        let owner_redeemed: EmailRedeemResponse = json_body(
            server
                .signed(
                    &user_secret(),
                    "POST",
                    "/api/v1/email-auth/redeem",
                    Some(&owner_redeem_body),
                )
                .unwrap(),
        );
        assert_eq!(owner_redeemed.email, "paul@finite.vip");
        clear_outbox(&server.outbox);

        let login_body = serde_json::to_vec(&EmailLoginRequest {
            email: "skyler_bot@finite.vip".into(),
        })
        .unwrap();
        let login: EmailLoginResponse = json_body(
            server
                .agent
                .post(&format!("{}/api/v1/email-auth/request", server.api_url))
                .set("Content-Type", "application/json")
                .send_bytes(&login_body)
                .unwrap(),
        );
        assert_eq!(login.email, "skyler_bot@finite.vip");
        let token = outbox_email_token(&server.outbox);

        let redeem_body = serde_json::to_vec(&EmailRedeemRequest {
            email: "skyler_bot@finite.vip".into(),
            token,
        })
        .unwrap();
        let redeemed: EmailRedeemResponse = json_body(
            server
                .signed(
                    &stranger_secret(),
                    "POST",
                    "/api/v1/email-auth/redeem",
                    Some(&redeem_body),
                )
                .unwrap(),
        );
        assert_eq!(redeemed.email, "skyler_bot@finite.vip");

        let editors_body = serde_json::to_vec(&EditorsRequest {
            actor_email: Some("paul@finite.vip".into()),
            add_emails: vec!["skyler_bot@finite.vip".into()],
            remove_emails: vec![],
        })
        .unwrap();
        server
            .signed(
                &user_secret(),
                "POST",
                "/api/v1/sites/collab/editors",
                Some(&editors_body),
            )
            .unwrap();

        let index_html: &[u8] = b"<h1>edited</h1>";
        let source_bytes: &[u8] = b"pretend source archive";
        let manifest = PublishManifest {
            files: vec![ManifestFile {
                path: "/index.html".into(),
                sha256: sha(index_html),
                size: index_html.len() as u64,
            }],
        };
        let source = SourceSnapshotRequest {
            sha256: sha(source_bytes),
            size: source_bytes.len() as u64,
        };
        let begin_body = serde_json::to_vec(&PublishBeginRequest {
            manifest,
            spa: false,
            start_command: None,
            actor_email: Some("skyler_bot@finite.vip".into()),
            source: Some(source.clone()),
        })
        .unwrap();
        let begun: PublishBeginResponse = json_body(
            server
                .signed(
                    &stranger_secret(),
                    "POST",
                    "/api/v1/sites/collab/publish",
                    Some(&begin_body),
                )
                .unwrap(),
        );
        assert_eq!(begun.missing, vec![sha(index_html), sha(source_bytes)]);
        for (blob, digest) in [
            (index_html, sha(index_html)),
            (source_bytes, sha(source_bytes)),
        ] {
            server
                .signed(
                    &stranger_secret(),
                    "PUT",
                    &format!("/api/v1/publishes/{}/blobs/{digest}", begun.publish_id),
                    Some(blob),
                )
                .unwrap();
        }
        let finalized: PublishFinalizeResponse = json_body(
            server
                .signed(
                    &stranger_secret(),
                    "POST",
                    &format!("/api/v1/publishes/{}/finalize", begun.publish_id),
                    None,
                )
                .unwrap(),
        );
        assert_eq!(finalized.version_number, 1);
        assert_eq!(
            finalized
                .source
                .as_ref()
                .map(|source| source.sha256.as_str()),
            Some(source.sha256.as_str())
        );

        let source_response = server
            .signed(
                &stranger_secret(),
                "GET",
                "/api/v1/sites/collab/source?email=skyler_bot%40finite.vip",
                None,
            )
            .unwrap();
        assert_eq!(source_response.status(), 200);
        assert_eq!(
            source_response.header("x-finite-source-version").unwrap(),
            "1"
        );
        assert_eq!(
            source_response.into_string().unwrap().as_bytes(),
            source_bytes
        );

        let remove_body = serde_json::to_vec(&EditorsRequest {
            actor_email: None,
            add_emails: vec![],
            remove_emails: vec!["skyler_bot@finite.vip".into()],
        })
        .unwrap();
        server
            .signed(
                &site_secret(),
                "POST",
                "/api/v1/sites/collab/editors",
                Some(&remove_body),
            )
            .unwrap();
        let denied = server.signed(
            &stranger_secret(),
            "POST",
            "/api/v1/sites/collab/publish",
            Some(&begin_body),
        );
        assert!(matches!(denied, Err(ureq::Error::Status(403, _))));

        let open = server.site_get("collab", "/", port);
        assert!(matches!(open, Err(ureq::Error::Status(401, _))));
    });
    task.await.unwrap();
}
