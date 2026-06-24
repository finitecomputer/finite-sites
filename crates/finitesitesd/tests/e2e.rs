//! End-to-end test: a real finitesitesd server on an ephemeral port, driven
//! over HTTP exactly the way `fsite` and a browser would drive it —
//! NIP-98-signed API calls, Host-routed site requests, magic-link login.

// Test helpers return ureq's own error so assertions can match on exact
// HTTP statuses; its size does not matter in a test binary.
#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use finitesites_blob::BlobStore;
use finitesites_engine::{Engine, EngineConfig};
use finitesites_proto::dto::{
    EmailLoginRequest, EmailLoginResponse, EmailRedeemRequest, EmailRedeemResponse, GitAuthRequest,
    GitAuthResponse, NativeViewerSessionRequest, ProjectApplyRequest, ProjectApplyResponse,
    ProjectCollaboratorRemoveRequest, ProjectCollaboratorRemoveResponse, ProjectCollaboratorSpec,
    SharingRequest, SharingResponse, SiteSummary,
};
use finitesites_proto::nip98;
use finitesites_proto::project_config::{
    ProjectConfig, ProjectOutputConfig, ProjectOutputKind, ProjectSection,
};
use finitesites_store::Store;
use finitesitesd::mailer::DevMailer;
use finitesitesd::{ServeOptions, server};

const BASE_DOMAIN: &str = "sites.localhost";

fn user_secret() -> [u8; 32] {
    let mut secret = [0u8; 32];
    secret[31] = 11;
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

    fn signed_site_post(
        &self,
        secret: &[u8; 32],
        name: &str,
        path: &str,
        body: &[u8],
        signed_url: Option<&str>,
        signed_method: &str,
    ) -> Result<ureq::Response, ureq::Error> {
        let url = format!("http://{name}.{BASE_DOMAIN}:{}{path}", self.port());
        let auth_url = signed_url.unwrap_or(&url);
        let header =
            nip98::build_auth_header(secret, auth_url, signed_method, Some(body), now_unix())
                .unwrap();
        self.agent
            .post(&url)
            .set("Authorization", &header)
            .set("Content-Type", "application/json")
            .send_bytes(body)
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

fn outbox_bodies(outbox: &Path) -> Vec<String> {
    let mut paths: Vec<_> = std::fs::read_dir(outbox)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| std::fs::read_to_string(path).unwrap())
        .collect()
}

fn clear_outbox(outbox: &Path) {
    for entry in std::fs::read_dir(outbox).unwrap() {
        std::fs::remove_file(entry.unwrap().path()).unwrap();
    }
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

fn try_git(args: &[&str], cwd: Option<&Path>) -> bool {
    let mut command = Command::new("git");
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    command.stdout(Stdio::null()).stderr(Stdio::null());
    command.status().unwrap().success()
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

fn mint_skyler_git_credential(server: &TestServer) -> GitAuthResponse {
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

    let auth_body = serde_json::to_vec(&GitAuthRequest {
        email: Some("skyler@example.com".into()),
    })
    .unwrap();
    json_body(
        server
            .signed(
                &stranger_secret(),
                "POST",
                "/api/v1/projects/finitechat-native/git-auth",
                Some(&auth_body),
            )
            .unwrap(),
    )
}

fn push_project_files(
    server: &TestServer,
    credential: &GitAuthResponse,
    finite_toml: &str,
    branch: &str,
    files: &[(&str, &str)],
    message: &str,
) {
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
    if !try_git(&["checkout", branch], Some(&repo)) {
        run_git(&["checkout", "-b", branch], Some(&repo));
    }
    std::fs::write(repo.join("finite.toml"), finite_toml).unwrap();
    for (path, content) in files {
        let target = repo.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(target, content).unwrap();
    }
    let mut add_args = vec!["add", "finite.toml"];
    for (path, _) in files {
        add_args.push(path);
    }
    run_git(&add_args, Some(&repo));
    run_git(
        &[
            "-c",
            "user.email=skyler@example.com",
            "-c",
            "user.name=Skyler Bot",
            "commit",
            "-m",
            message,
        ],
        Some(&repo),
    );
    run_git(
        &[
            "-c",
            &format!("http.extraHeader={host_header}"),
            "push",
            "origin",
            branch,
        ],
        Some(&repo),
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn full_publish_share_and_view_flow() {
    let user_pubkey = finitesites_proto::event::pubkey_for_secret(&user_secret()).unwrap();
    let server = TestServer::start(&user_pubkey).await;
    let port = server.port();

    let task = tokio::task::spawn_blocking(move || {
        let health = server
            .agent
            .get(&format!("{}/api/v1/healthz", server.api_url))
            .call();
        assert!(health.is_ok());

        let apply_body = serde_json::to_vec(&project_apply_request(false)).unwrap();
        let denied = server.signed(
            &stranger_secret(),
            "POST",
            "/api/v1/projects/apply",
            Some(&apply_body),
        );
        assert!(matches!(denied, Err(ureq::Error::Status(403, _))));

        let no_auth = server
            .agent
            .post(&format!("{}/api/v1/projects/apply", server.api_url))
            .set("Authorization", "Nostr bm90LWFuLWV2ZW50")
            .send_bytes(&apply_body);
        assert!(matches!(no_auth, Err(ureq::Error::Status(401, _))));

        let created: ProjectApplyResponse = json_body(
            server
                .signed(
                    &user_secret(),
                    "POST",
                    "/api/v1/projects/apply",
                    Some(&apply_body),
                )
                .unwrap(),
        );
        let placeholder = server
            .site_get("finitechat-native-mockup", "/", port)
            .unwrap();
        assert!(placeholder.into_string().unwrap().contains("claimed"));

        let credential = mint_skyler_git_credential(&server);
        push_project_files(
            &server,
            &credential,
            &created.finite_toml,
            "main",
            &[
                ("index.html", "<h1>hello from finite</h1>"),
                ("css/style.css", "body { background: black }"),
            ],
            "Initial deploy",
        );
        let summary = wait_for_active_version(&server, "finitechat-native-mockup", Some(1));
        assert_eq!(summary.active_version, Some(1));

        let gated = server.site_get("finitechat-native-mockup", "/", port);
        let Err(ureq::Error::Status(401, response)) = gated else {
            panic!("expected 401 for private site");
        };
        assert!(response.into_string().unwrap().contains("private"));

        let share_body = serde_json::to_vec(&SharingRequest {
            visibility: Some("shared".into()),
            confirm_public: false,
            add_emails: vec!["friend@example.com".into()],
            remove_emails: vec![],
            add_pubkeys: vec![],
            remove_pubkeys: vec![],
        })
        .unwrap();
        let shared: SharingResponse = json_body(
            server
                .signed(
                    &user_secret(),
                    "POST",
                    "/api/v1/sites/finitechat-native-mockup/sharing",
                    Some(&share_body),
                )
                .unwrap(),
        );
        assert_eq!(shared.shared_emails, vec!["friend@example.com"]);

        let site_base = format!("http://finitechat-native-mockup.{BASE_DOMAIN}:{port}");
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

        let redeemed = server.agent.get(&link).call().unwrap();
        assert_eq!(redeemed.status(), 303);
        let cookie = redeemed
            .header("set-cookie")
            .expect("login sets a cookie")
            .split(';')
            .next()
            .unwrap()
            .to_string();

        let replayed = server.agent.get(&link).call();
        assert!(matches!(replayed, Err(ureq::Error::Status(400, _))));

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

        let public_body = serde_json::to_vec(&SharingRequest {
            visibility: Some("public".into()),
            confirm_public: true,
            add_emails: vec![],
            remove_emails: vec![],
            add_pubkeys: vec![],
            remove_pubkeys: vec![],
        })
        .unwrap();
        server
            .signed(
                &user_secret(),
                "POST",
                "/api/v1/sites/finitechat-native-mockup/sharing",
                Some(&public_body),
            )
            .unwrap();
        let open = server
            .site_get("finitechat-native-mockup", "/", port)
            .unwrap();
        assert_eq!(open.into_string().unwrap(), "<h1>hello from finite</h1>");

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
async fn share_send_invite_emails_viewer_magic_link_and_replays() {
    let user_pubkey = finitesites_proto::event::pubkey_for_secret(&user_secret()).unwrap();
    let server = TestServer::start(&user_pubkey).await;
    let port = server.port();

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
        let credential = mint_skyler_git_credential(&server);
        push_project_files(
            &server,
            &credential,
            &created.finite_toml,
            "main",
            &[("index.html", "<h1>invite</h1>")],
            "Invite test deploy",
        );
        wait_for_active_version(&server, "finitechat-native-mockup", Some(1));

        let invalid_invite_body = serde_json::to_vec(&SharingRequest {
            visibility: Some("private".into()),
            confirm_public: false,
            add_emails: vec!["Friend@Example.com".into()],
            remove_emails: vec![],
            add_pubkeys: vec![],
            remove_pubkeys: vec![],
        })
        .unwrap();
        let invalid_invite = server
            .signed(
                &user_secret(),
                "POST",
                "/api/v1/sites/finitechat-native-mockup/sharing?send_invites=true",
                Some(&invalid_invite_body),
            )
            .unwrap_err();
        assert!(matches!(invalid_invite, ureq::Error::Status(400, _)));
        let unchanged: SiteSummary = json_body(
            server
                .signed(
                    &user_secret(),
                    "GET",
                    "/api/v1/sites/finitechat-native-mockup",
                    None,
                )
                .unwrap(),
        );
        assert_eq!(unchanged.visibility, "private");
        assert!(unchanged.shared_emails.is_empty());
        assert!(outbox_bodies(&server.outbox).is_empty());

        let share_body = serde_json::to_vec(&SharingRequest {
            visibility: Some("shared".into()),
            confirm_public: false,
            add_emails: vec!["Friend@Example.com".into()],
            remove_emails: vec![],
            add_pubkeys: vec![],
            remove_pubkeys: vec![],
        })
        .unwrap();
        let shared: SharingResponse = json_body(
            server
                .signed(
                    &user_secret(),
                    "POST",
                    "/api/v1/sites/finitechat-native-mockup/sharing?send_invites=true",
                    Some(&share_body),
                )
                .unwrap(),
        );
        assert_eq!(shared.shared_emails, vec!["friend@example.com"]);
        assert_eq!(shared.invited_emails, vec!["friend@example.com"]);

        let bodies = outbox_bodies(&server.outbox);
        assert_eq!(bodies.len(), 1);
        assert!(bodies[0].contains("You've been invited to view finitechat-native-mockup"));
        assert!(bodies[0].contains("/llms.txt"));
        let site_base = format!("http://finitechat-native-mockup.{BASE_DOMAIN}:{port}");
        let link = outbox_link(&server.outbox);
        assert!(link.starts_with(&format!("{site_base}/_finite/auth?token=")));
        let redeemed = server.agent.get(&link).call().unwrap();
        assert_eq!(redeemed.status(), 303);

        clear_outbox(&server.outbox);
        let replay: SharingResponse = json_body(
            server
                .signed(
                    &user_secret(),
                    "POST",
                    "/api/v1/sites/finitechat-native-mockup/sharing?send_invites=true",
                    Some(&share_body),
                )
                .unwrap(),
        );
        assert_eq!(replay.shared_emails, vec!["friend@example.com"]);
        assert_eq!(replay.invited_emails, vec!["friend@example.com"]);
        assert_eq!(outbox_bodies(&server.outbox).len(), 1);
    });
    task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn native_viewer_auth_sets_cookie_without_email_or_relays() {
    let user_pubkey = finitesites_proto::event::pubkey_for_secret(&user_secret()).unwrap();
    let viewer_pubkey = finitesites_proto::event::pubkey_for_secret(&stranger_secret()).unwrap();
    let server = TestServer::start(&user_pubkey).await;
    let port = server.port();

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
        let credential = mint_skyler_git_credential(&server);
        push_project_files(
            &server,
            &credential,
            &created.finite_toml,
            "main",
            &[("index.html", "<h1>native auth</h1>")],
            "Native auth deploy",
        );
        wait_for_active_version(&server, "finitechat-native-mockup", Some(1));

        let share_body = serde_json::to_vec(&SharingRequest {
            visibility: Some("shared".into()),
            confirm_public: false,
            add_emails: vec![],
            remove_emails: vec![],
            add_pubkeys: vec![viewer_pubkey.clone()],
            remove_pubkeys: vec![],
        })
        .unwrap();
        let shared: SharingResponse = json_body(
            server
                .signed(
                    &user_secret(),
                    "POST",
                    "/api/v1/sites/finitechat-native-mockup/sharing",
                    Some(&share_body),
                )
                .unwrap(),
        );
        assert!(shared.shared_emails.is_empty());
        assert_eq!(shared.shared_pubkeys, vec![viewer_pubkey.clone()]);

        let unauthenticated = server.site_get("finitechat-native-mockup", "/", port);
        assert!(matches!(unauthenticated, Err(ureq::Error::Status(401, _))));

        let request = NativeViewerSessionRequest {
            purpose: "finite_site_view_session".into(),
            return_to: "/".into(),
            client: "finite-chat-ios".into(),
            nonce: "native-nonce-0000001".into(),
        };
        let native_body = serde_json::to_vec(&request).unwrap();
        let authed = server
            .signed_site_post(
                &stranger_secret(),
                "finitechat-native-mockup",
                "/_finite/auth/native-session",
                &native_body,
                None,
                "POST",
            )
            .unwrap();
        assert_eq!(authed.status(), 303);
        assert_eq!(authed.header("location"), Some("/"));
        let cookie = authed
            .header("set-cookie")
            .expect("native auth sets a cookie")
            .split(';')
            .next()
            .unwrap()
            .to_string();

        let page = server
            .agent
            .get(&format!(
                "http://finitechat-native-mockup.{BASE_DOMAIN}:{port}/"
            ))
            .set("Cookie", &cookie)
            .call()
            .unwrap();
        assert!(page.into_string().unwrap().contains("native auth"));

        let replay = server.signed_site_post(
            &stranger_secret(),
            "finitechat-native-mockup",
            "/_finite/auth/native-session",
            &native_body,
            None,
            "POST",
        );
        assert!(matches!(replay, Err(ureq::Error::Status(400, _))));

        let wrong_url = server.signed_site_post(
            &stranger_secret(),
            "finitechat-native-mockup",
            "/_finite/auth/native-session",
            &native_body,
            Some("http://evil.test/_finite/auth/native-session"),
            "POST",
        );
        assert!(matches!(wrong_url, Err(ureq::Error::Status(401, _))));

        let wrong_method = server.signed_site_post(
            &stranger_secret(),
            "finitechat-native-mockup",
            "/_finite/auth/native-session",
            &native_body,
            None,
            "GET",
        );
        assert!(matches!(wrong_method, Err(ureq::Error::Status(401, _))));

        let url = format!(
            "http://finitechat-native-mockup.{BASE_DOMAIN}:{port}/_finite/auth/native-session"
        );
        let header = nip98::build_auth_header(
            &stranger_secret(),
            &url,
            "POST",
            Some(&native_body),
            now_unix(),
        )
        .unwrap();
        let tampered = serde_json::to_vec(&NativeViewerSessionRequest {
            nonce: "native-nonce-0000002".into(),
            ..request.clone()
        })
        .unwrap();
        let payload_mismatch = server
            .agent
            .post(&url)
            .set("Authorization", &header)
            .set("Content-Type", "application/json")
            .send_bytes(&tampered);
        assert!(matches!(payload_mismatch, Err(ureq::Error::Status(401, _))));

        let unshared = server.signed_site_post(
            &user_secret(),
            "finitechat-native-mockup",
            "/_finite/auth/native-session",
            &native_body,
            None,
            "POST",
        );
        assert!(matches!(unshared, Err(ureq::Error::Status(401, _))));

        let bad_return = serde_json::to_vec(&NativeViewerSessionRequest {
            return_to: "https://evil.test/".into(),
            nonce: "native-nonce-0000003".into(),
            ..request
        })
        .unwrap();
        let malformed = server.signed_site_post(
            &stranger_secret(),
            "finitechat-native-mockup",
            "/_finite/auth/native-session",
            &bad_return,
            None,
            "POST",
        );
        assert!(matches!(malformed, Err(ureq::Error::Status(400, _))));
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

        let owner_auth_body = serde_json::to_vec(&GitAuthRequest { email: None }).unwrap();
        let owner_credential: GitAuthResponse = json_body(
            server
                .signed(
                    &user_secret(),
                    "POST",
                    "/api/v1/projects/finitechat-native/git-auth",
                    Some(&owner_auth_body),
                )
                .unwrap(),
        );
        assert_eq!(owner_credential.project_slug, "finitechat-native");
        assert_eq!(owner_credential.username, owner_credential.credential_id);
        assert_eq!(owner_credential.password.len(), 64);

        let unauthorized_native = server
            .signed(
                &stranger_secret(),
                "POST",
                "/api/v1/projects/finitechat-native/git-auth",
                Some(&owner_auth_body),
            )
            .unwrap_err();
        assert!(matches!(unauthorized_native, ureq::Error::Status(403, _)));

        let bad_auth = serde_json::to_vec(&GitAuthRequest {
            email: Some("skyler@example.com".into()),
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
async fn project_apply_send_invite_emails_collaborator_and_replays() {
    let user_pubkey = finitesites_proto::event::pubkey_for_secret(&user_secret()).unwrap();
    let server = TestServer::start(&user_pubkey).await;

    let task = tokio::task::spawn_blocking(move || {
        let body = serde_json::to_vec(&project_apply_request(false)).unwrap();
        let created: ProjectApplyResponse = json_body(
            server
                .signed(
                    &user_secret(),
                    "POST",
                    "/api/v1/projects/apply?send_invites=true",
                    Some(&body),
                )
                .unwrap(),
        );
        assert!(!created.dry_run);
        assert!(created.created);
        assert_eq!(created.collaborators.len(), 1);
        assert_eq!(created.invited_emails, vec!["skyler@example.com"]);
        let bodies = outbox_bodies(&server.outbox);
        assert_eq!(bodies.len(), 1);
        assert!(bodies[0].contains("You've been invited to collaborate on finitechat-native"));
        assert!(bodies[0].contains("fsite email-redeem skyler@example.com"));
        assert!(bodies[0].contains(
            "fsite auth git finitechat-native --email skyler@example.com --store --output json"
        ));
        assert!(bodies[0].contains(&format!(
            "git clone http://git.{BASE_DOMAIN}:{}/finitechat-native.git",
            server.port()
        )));
        assert!(bodies[0].contains("finitechat-native-mockup"));

        clear_outbox(&server.outbox);
        let replay: ProjectApplyResponse = json_body(
            server
                .signed(
                    &user_secret(),
                    "POST",
                    "/api/v1/projects/apply?send_invites=true",
                    Some(&body),
                )
                .unwrap(),
        );
        assert!(!replay.created);
        assert_eq!(replay.collaborators.len(), 1);
        assert_eq!(replay.invited_emails, vec!["skyler@example.com"]);
        assert_eq!(outbox_bodies(&server.outbox).len(), 1);
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
            email: Some("skyler@example.com".into()),
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
            email: Some("skyler@example.com".into()),
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
            "fsite auth git finitechat-native --email YOUR_EDITOR_EMAIL --store --output json"
        ));
        assert!(llms.contains("fsite auth git finitechat-native --store --output json"));
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
            email: Some("skyler@example.com".into()),
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
            email: Some("skyler@example.com".into()),
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
            email: Some("skyler@example.com".into()),
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
async fn generated_llms_txt_requires_project_output_and_respects_user_file() {
    let user_pubkey = finitesites_proto::event::pubkey_for_secret(&user_secret()).unwrap();
    let server = TestServer::start(&user_pubkey).await;
    let port = server.port();

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
        let credential = mint_skyler_git_credential(&server);
        push_project_files(
            &server,
            &credential,
            &created.finite_toml,
            "main",
            &[("index.html", "<h1>v1</h1>")],
            "Initial agent docs deploy",
        );
        wait_for_active_version(&server, "finitechat-native-mockup", Some(1));

        let generated = server
            .site_get("finitechat-native-mockup", "/llms.txt", port)
            .unwrap()
            .into_string()
            .unwrap();
        assert!(generated.contains("Project: finitechat-native"));
        assert!(generated.contains("fsite auth git finitechat-native"));

        let public_body = serde_json::to_vec(&SharingRequest {
            visibility: Some("public".into()),
            confirm_public: true,
            add_emails: vec![],
            remove_emails: vec![],
            add_pubkeys: vec![],
            remove_pubkeys: vec![],
        })
        .unwrap();
        server
            .signed(
                &user_secret(),
                "POST",
                "/api/v1/sites/finitechat-native-mockup/sharing",
                Some(&public_body),
            )
            .unwrap();

        push_project_files(
            &server,
            &credential,
            &created.finite_toml,
            "main",
            &[
                ("index.html", "<h1>v2</h1>"),
                ("llms.txt", "custom project instructions"),
            ],
            "Author llms instructions",
        );
        wait_for_active_version(&server, "finitechat-native-mockup", Some(2));
        let custom = server
            .site_get("finitechat-native-mockup", "/llms.txt", port)
            .unwrap();
        assert_eq!(custom.into_string().unwrap(), "custom project instructions");
    });
    task.await.unwrap();
}
