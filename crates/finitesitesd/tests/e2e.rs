//! End-to-end test: a real finitesitesd server on an ephemeral port, driven
//! over HTTP exactly the way `fsite` and a browser would drive it —
//! NIP-98-signed API calls, Host-routed site requests, magic-link login.

// Test helpers return ureq's own error so assertions can match on exact
// HTTP statuses; its size does not matter in a test binary.
#![allow(clippy::result_large_err)]

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256};

use finitesites_blob::BlobStore;
use finitesites_engine::{Engine, EngineConfig};
use finitesites_proto::dto::{
    ClaimRequest, ClaimResponse, PublishBeginRequest, PublishBeginResponse,
    PublishFinalizeResponse, SharingRequest, SharingResponse,
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
    _data_dir: tempfile::TempDir,
}

impl TestServer {
    async fn start(allowed_pubkey: &str) -> TestServer {
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
            _data_dir: data_dir,
        }
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
