//! Reverse proxy for tier-2 app sites: forward the request to the app's
//! loopback port and stream the response back. The visibility gate has
//! already run by the time a request reaches here.
//!
//! v0 limitations (documented in the roadmap): no websocket upgrade, no
//! response buffering limits beyond hyper's own.

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;

use crate::pages;

/// The upstream app could not be reached at all. Callers use this to
/// invalidate a cached endpoint; mid-stream errors do not qualify.
#[derive(Debug, thiserror::Error)]
#[error("app upstream unreachable")]
pub struct UpstreamUnreachable;

/// One shared client; connections to app ports are pooled.
fn client() -> &'static Client<HttpConnector, Body> {
    static CLIENT: std::sync::OnceLock<Client<HttpConnector, Body>> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| Client::builder(TokioExecutor::new()).build_http())
}

use std::net::SocketAddr;
use std::time::Duration;

/// How long to wait for a just-woken app to start accepting connections
/// before giving up with a 502. A cold Node/Bun/Python start (and, for the
/// Kata runner, a microVM boot) fits comfortably under this.
const WAKE_TIMEOUT: Duration = Duration::from_secs(20);

pub async fn forward(
    request: Request,
    target: SocketAddr,
) -> Result<Response, UpstreamUnreachable> {
    // The app may have just been woken from idle; wait for its port.
    if !wait_until_ready(target, WAKE_TIMEOUT).await {
        return Err(UpstreamUnreachable);
    }

    let (mut parts, body) = request.into_parts();
    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let upstream: Uri = match format!("http://{target}{path_and_query}").parse() {
        Ok(uri) => uri,
        Err(_) => return Ok(html_502("bad upstream path")),
    };
    parts.uri = upstream;
    // The app sees the original Host plus standard forwarding headers.
    parts
        .headers
        .insert("x-forwarded-proto", HeaderValue::from_static("https"));
    // Hop-by-hop headers must not be forwarded.
    parts.headers.remove(header::CONNECTION);
    parts.headers.remove(header::TE);
    parts.headers.remove(header::TRANSFER_ENCODING);
    parts.headers.remove(header::UPGRADE);

    let upstream_request = Request::from_parts(parts, body);
    match client().request(upstream_request).await {
        Ok(response) => {
            let (parts, body) = response.into_parts();
            Ok(Response::from_parts(parts, Body::new(body)))
        }
        Err(error) if error.is_connect() => Err(UpstreamUnreachable),
        Err(error) => {
            // Mid-stream failure: the app is reachable but misbehaving;
            // do not invalidate the endpoint cache for this.
            eprintln!("finitesitesd proxy error ({target}): {error}");
            Ok(html_502("the app is reachable but failed mid-request"))
        }
    }
}

/// Poll the target until a TCP connection succeeds or the deadline passes.
/// A ready app connects on the first try; a waking one takes a few hundred
/// ms (process) to a couple seconds (microVM cold boot).
async fn wait_until_ready(target: SocketAddr, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut backoff = Duration::from_millis(25);
    loop {
        match tokio::time::timeout(
            Duration::from_millis(500),
            tokio::net::TcpStream::connect(target),
        )
        .await
        {
            Ok(Ok(_stream)) => return true,
            _ => {
                if tokio::time::Instant::now() >= deadline {
                    return false;
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_millis(400));
            }
        }
    }
}

fn html_502(_reason: &str) -> Response {
    app_unavailable_response()
}

/// The 502 "app isn't responding" page, also used when a wake fails before
/// the proxy step.
pub fn app_unavailable_response() -> Response {
    (
        StatusCode::BAD_GATEWAY,
        [(header::CACHE_CONTROL, "no-store")],
        Html(pages::app_unavailable()),
    )
        .into_response()
}
