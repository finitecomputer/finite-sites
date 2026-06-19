//! HTTP server assembly: one listener, two planes.
//!
//! Requests whose Host matches the API host go to the control-plane API.
//! Requests whose Host is `{label}.{base_domain}` go to the site-serving
//! plane. Everything else (the bare listen address, a load balancer health
//! check) goes to the API. The API check runs first because in production
//! the API host (`api.finite.chat`) itself matches `*.finite.chat`; `api`
//! is also a reserved site name, so the two planes can never both claim a
//! host. The split is decided in one place, by host, before any route
//! matching.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::HOST;
use axum::response::Response;
use tower::util::ServiceExt as _;

use finitesites_engine::Engine;

use crate::apps::Supervisor;
use crate::limiter::{RateLimiter, WINDOW_SECONDS};
use crate::mailer::Mailer;
use crate::{ServeOptions, api, git, sites};

pub struct AppState {
    /// The engine owns the registry connection, which is not Sync; one
    /// mutex serializes control-plane work. Fine for v1 scale (see the
    /// technical debt ledger for the pooling plan).
    pub engine: Mutex<Engine>,
    pub mailer: Box<dyn Mailer>,
    /// Owns app isolation (the runner) plus the density policy: wake on
    /// request, stop when idle.
    pub apps: Supervisor,
    pub login_limiter: RateLimiter,
    pub api_url: String,
    pub git_base_url: String,
    pub base_domain: String,
    pub data_dir: PathBuf,
}

pub fn now_unix() -> u64 {
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    assert!(now > 0);
    now as u64
}

#[derive(Clone)]
struct Dispatcher {
    api: Router,
    git: Router,
    sites: Router,
    base_domain: String,
    /// Port-stripped host of the configured `--api-url`, checked before the
    /// wildcard so `api.finite.chat` never falls into the sites plane.
    api_host: String,
    git_host: String,
}

pub fn build_app(state: Arc<AppState>) -> Router {
    let dispatcher = Dispatcher {
        api: api::router(state.clone()),
        git: git::router(state.clone()),
        sites: sites::router(state.clone()),
        base_domain: state.base_domain.clone(),
        api_host: host_of_url(&state.api_url),
        git_host: host_of_url(&state.git_base_url),
    };
    Router::new().fallback(dispatch).with_state(dispatcher)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Plane {
    Api,
    Git,
    Sites,
}

/// The one routing decision: which plane serves this Host header.
pub fn plane_for_host(host: &str, api_host: &str, git_host: &str, base_domain: &str) -> Plane {
    if strip_port(host).eq_ignore_ascii_case(api_host) {
        return Plane::Api;
    }
    if strip_port(host).eq_ignore_ascii_case(git_host) {
        return Plane::Git;
    }
    if site_label(host, base_domain).is_some() {
        return Plane::Sites;
    }
    Plane::Api
}

async fn dispatch(State(dispatcher): State<Dispatcher>, request: Request<Body>) -> Response {
    let host = request
        .headers()
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let router = match plane_for_host(
        host,
        &dispatcher.api_host,
        &dispatcher.git_host,
        &dispatcher.base_domain,
    ) {
        Plane::Sites => dispatcher.sites.clone(),
        Plane::Git => dispatcher.git.clone(),
        Plane::Api => dispatcher.api.clone(),
    };
    match router.oneshot(request).await {
        Ok(response) => response,
        Err(never) => match never {},
    }
}

/// Host (no port) of a URL like `https://api.finite.chat` or
/// `http://127.0.0.1:8787`.
pub fn host_of_url(url: &str) -> String {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let host_and_port = after_scheme.split(['/', '?']).next().unwrap_or("");
    strip_port(host_and_port).to_ascii_lowercase()
}

fn strip_port(host: &str) -> &str {
    if host.starts_with('[') {
        // IPv6 literal: [::1]:8787 -> [::1]
        return host
            .split_once("]:")
            .map(|(left, _)| &host[..left.len() + 1])
            .unwrap_or(host);
    }
    match host.rsplit_once(':') {
        Some((left, right)) if right.bytes().all(|b| b.is_ascii_digit()) => left,
        _ => host,
    }
}

/// Extract the site label from a Host header value: `hello.sites.localhost`
/// with base domain `sites.localhost` yields `hello`. Ports are stripped.
/// Multi-level labels (`a.b.sites.localhost`) are rejected: one wildcard
/// level keeps certificates and cookies simple.
pub fn site_label(host: &str, base_domain: &str) -> Option<String> {
    if host.is_empty() || host.starts_with('[') {
        // IPv6 literals are never site hosts.
        return None;
    }
    let without_port = match host.rsplit_once(':') {
        Some((left, right)) if right.bytes().all(|b| b.is_ascii_digit()) => left,
        _ => host,
    };
    let label = without_port.strip_suffix(base_domain)?.strip_suffix('.')?;
    if label.is_empty() || label.contains('.') {
        return None;
    }
    Some(label.to_ascii_lowercase())
}

pub async fn serve(
    engine: Engine,
    mailer: Box<dyn Mailer>,
    apps: Supervisor,
    options: ServeOptions,
) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(options.listen)
        .await
        .map_err(|error| format!("cannot bind {}: {error}", options.listen))?;
    serve_on(listener, engine, mailer, apps, options).await
}

/// Serve on an already-bound listener. Split from `serve` so tests can bind
/// an ephemeral port first and build options around the real address.
pub async fn serve_on(
    listener: tokio::net::TcpListener,
    engine: Engine,
    mailer: Box<dyn Mailer>,
    apps: Supervisor,
    options: ServeOptions,
) -> Result<(), String> {
    let state = Arc::new(AppState {
        engine: Mutex::new(engine),
        mailer,
        apps,
        login_limiter: RateLimiter::new(WINDOW_SECONDS),
        api_url: options.api_url.clone(),
        git_base_url: options.git_base_url.clone(),
        base_domain: options.base_domain.clone(),
        data_dir: options.data_dir.clone(),
    });
    reconcile_apps(&state);
    spawn_idle_reaper(state.clone());
    let app = build_app(state);
    eprintln!(
        "finitesitesd listening on {} (api: {}, git: {}, sites: *.{})",
        options.listen, options.api_url, options.git_base_url, options.base_domain
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|error| format!("server error: {error}"))
}

/// Bring every app site with an active version back up after a daemon
/// restart. Failures are logged, not fatal: one broken app must not stop
/// the platform from serving.
fn reconcile_apps(state: &Arc<AppState>) {
    let engine = state.engine.lock().expect("engine mutex never poisoned");
    let deploys = match engine.app_deploys() {
        Ok(deploys) => deploys,
        Err(error) => {
            eprintln!("app reconcile: cannot list app sites: {error}");
            return;
        }
    };
    // Bounded by the app port range.
    for deploy in &deploys {
        let bundle_path = engine.blob_file_path(&deploy.bundle_sha256);
        if let Err(error) = state.apps.deploy(deploy, &bundle_path, now_unix()) {
            eprintln!("app reconcile: {} failed: {error}", deploy.site_id);
        }
    }
    if !deploys.is_empty() {
        eprintln!("app reconcile: {} app site(s) processed", deploys.len());
    }
}

/// Periodically stop apps that have been idle past the timeout. This is the
/// density mechanism: idle tenants cost ~0 memory and wake on the next
/// request. The check runs every minute; reaping itself is bounded by the
/// app count.
fn spawn_idle_reaper(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            tick.tick().await;
            let state = state.clone();
            // Runner calls (systemctl/ctr) are blocking; keep them off the
            // async reactor.
            let _ = tokio::task::spawn_blocking(move || {
                let deploys = {
                    let engine = state.engine.lock().expect("engine mutex");
                    engine.app_deploys()
                };
                let deploys = match deploys {
                    Ok(deploys) => deploys,
                    Err(error) => {
                        eprintln!("idle reaper: cannot list apps: {error}");
                        return;
                    }
                };
                let stopped = state.apps.reap_idle(&deploys, now_unix());
                if !stopped.is_empty() {
                    eprintln!("idle reaper: stopped {} idle app(s)", stopped.len());
                }
            })
            .await;
        }
    });
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    eprintln!("finitesitesd shutting down");
}

#[cfg(test)]
mod tests {
    use super::{host_of_url, site_label, strip_port};

    #[test]
    fn host_of_url_extraction() {
        assert_eq!(host_of_url("https://api.finite.chat"), "api.finite.chat");
        assert_eq!(host_of_url("http://127.0.0.1:8787"), "127.0.0.1");
        assert_eq!(
            host_of_url("https://API.Finite.Chat/path?q=1"),
            "api.finite.chat"
        );
    }

    #[test]
    fn strip_port_handles_ipv6() {
        assert_eq!(strip_port("api.finite.chat:443"), "api.finite.chat");
        assert_eq!(strip_port("api.finite.chat"), "api.finite.chat");
        assert_eq!(strip_port("[::1]:8787"), "[::1]");
        assert_eq!(strip_port("[::1]"), "[::1]");
    }

    // The production-shaped regression: api.finite.chat matches the
    // *.finite.chat wildcard but must classify as the API host.
    #[test]
    fn api_host_wins_over_wildcard() {
        use super::{Plane, plane_for_host};
        let base = "finite.chat";
        let api_host = host_of_url("https://api.finite.chat");
        let git_host = host_of_url("https://git.finite.chat");
        assert_eq!(
            plane_for_host("api.finite.chat", &api_host, &git_host, base),
            Plane::Api
        );
        assert_eq!(
            plane_for_host("api.finite.chat:443", &api_host, &git_host, base),
            Plane::Api
        );
        assert_eq!(
            plane_for_host("API.finite.chat", &api_host, &git_host, base),
            Plane::Api
        );
        assert_eq!(
            plane_for_host("git.finite.chat", &api_host, &git_host, base),
            Plane::Git
        );
        assert_eq!(
            plane_for_host("hello.finite.chat", &api_host, &git_host, base),
            Plane::Sites
        );
        assert_eq!(
            plane_for_host("finite.chat", &api_host, &git_host, base),
            Plane::Api
        );
        assert_eq!(
            plane_for_host("127.0.0.1:8787", &api_host, &git_host, base),
            Plane::Api
        );
    }

    #[test]
    fn site_label_extraction() {
        let base = "sites.localhost";
        assert_eq!(
            site_label("hello.sites.localhost", base),
            Some("hello".into())
        );
        assert_eq!(
            site_label("hello.sites.localhost:8787", base),
            Some("hello".into())
        );
        assert_eq!(
            site_label("HELLO.sites.localhost", base),
            Some("hello".into())
        );
        assert_eq!(site_label("sites.localhost", base), None);
        assert_eq!(site_label("a.b.sites.localhost", base), None);
        assert_eq!(site_label("127.0.0.1:8787", base), None);
        assert_eq!(site_label("evil-sites.localhost", base), None);
        assert_eq!(site_label("[::1]:8787", base), None);
        assert_eq!(site_label("", base), None);
    }
}
