//! Control-plane API. Every mutation is authenticated with NIP-98 against
//! the exact URL and method received, and bodies are bound by payload hash.

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, OriginalUri, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post, put};

use finitesites_engine::EngineError;
use finitesites_proto::dto::{
    ApiErrorBody, ClaimRequest, ClaimResponse, PublishBeginRequest, PublishBeginResponse,
    PublishFinalizeResponse, SharingRequest, SiteListResponse,
};
use finitesites_proto::limits::{MAX_API_BODY_BYTES, MAX_APP_BUNDLE_BYTES};
use finitesites_proto::{ProtoError, nip98};

use crate::server::{AppState, now_unix};

pub fn router(state: Arc<AppState>) -> Router {
    // The blob route must admit app bundles; the engine enforces the
    // tighter static-file ceiling per publish kind.
    let blob_limit = DefaultBodyLimit::max(MAX_APP_BUNDLE_BYTES as usize + 1024);
    Router::new()
        .route("/api/v1/healthz", get(healthz))
        .route("/api/v1/sites/claim", post(claim))
        .route("/api/v1/sites", get(list_sites))
        .route("/api/v1/sites/{name}", get(site_status))
        .route("/api/v1/sites/{name}/sharing", post(set_sharing))
        .route("/api/v1/sites/{name}/publish", post(begin_publish))
        .route(
            "/api/v1/publishes/{publish_id}/blobs/{sha256}",
            put(upload_blob).layer(blob_limit),
        )
        .route(
            "/api/v1/publishes/{publish_id}/finalize",
            post(finalize_publish),
        )
        .layer(DefaultBodyLimit::max(MAX_API_BODY_BYTES as usize))
        .fallback(api_not_found)
        .with_state(state)
}

// ---- error mapping -----------------------------------------------------------

pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> ApiError {
        ApiError {
            status,
            code,
            message: message.into(),
        }
    }

    fn unauthorized(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", message)
    }

    fn bad_request(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ApiErrorBody {
            error: self.code.to_string(),
            message: self.message,
        };
        (self.status, Json(body)).into_response()
    }
}

impl From<EngineError> for ApiError {
    fn from(error: EngineError) -> ApiError {
        let message = error.to_string();
        match error {
            EngineError::NotAllowlisted => {
                ApiError::new(StatusCode::FORBIDDEN, "not_allowlisted", message)
            }
            EngineError::NotAuthorized => {
                ApiError::new(StatusCode::FORBIDDEN, "not_authorized", message)
            }
            EngineError::NameTaken => ApiError::new(StatusCode::CONFLICT, "name_taken", message),
            EngineError::SiteNotFound | EngineError::PublishNotFound => {
                ApiError::new(StatusCode::NOT_FOUND, "not_found", message)
            }
            EngineError::TooManySites | EngineError::TooManyShares => {
                ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "limit_exceeded", message)
            }
            EngineError::Validation(_) | EngineError::Proto(_) => {
                ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", message)
            }
            EngineError::Conflict(_) => ApiError::new(StatusCode::CONFLICT, "conflict", message),
            EngineError::Blob(inner) => match inner {
                finitesites_blob::BlobError::TooLarge { .. }
                | finitesites_blob::BlobError::HashMismatch { .. } => {
                    ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", message)
                }
                _ => internal_error("blob storage failure"),
            },
            EngineError::Store(_) => internal_error("registry failure"),
        }
    }
}

fn internal_error(message: &'static str) -> ApiError {
    // Internal details go to the operator log, not the wire.
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
}

// ---- auth helper ----------------------------------------------------------------

/// Verify the NIP-98 Authorization header against the request actually
/// received and return the signer's pubkey hex.
fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
    method: &str,
    original_uri: &OriginalUri,
    body: Option<&[u8]>,
) -> Result<String, ApiError> {
    let header_value = headers
        .get(header::AUTHORIZATION)
        .ok_or_else(|| ApiError::unauthorized("missing Authorization header"))?
        .to_str()
        .map_err(|_| ApiError::unauthorized("malformed Authorization header"))?;
    let path_and_query = original_uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let url = format!("{}{}", state.api_url, path_and_query);
    nip98::verify_auth_header(header_value, &url, method, body, now_unix()).map_err(|error| {
        match error {
            ProtoError::AuthRejected(reason) => {
                ApiError::unauthorized(format!("auth rejected: {reason}"))
            }
            other => ApiError::unauthorized(other.to_string()),
        }
    })
}

fn parse_json_body<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(body)
        .map_err(|error| ApiError::bad_request(format!("invalid json: {error}")))
}

/// Engine errors that indicate operator-side failure also go to stderr.
fn log_if_internal(error: &EngineError) {
    let is_internal = matches!(
        error,
        EngineError::Store(_) | EngineError::Blob(finitesites_blob::BlobError::Io(_))
    );
    if is_internal {
        eprintln!("finitesitesd internal error: {error}");
    }
}

// ---- handlers -------------------------------------------------------------------

async fn healthz() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true }))
}

async fn api_not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "not_found", "unknown api route")
}

async fn claim(
    State(state): State<Arc<AppState>>,
    original_uri: OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<ClaimResponse>, ApiError> {
    let owner = authenticate(&state, &headers, "POST", &original_uri, Some(&body))?;
    let request: ClaimRequest = parse_json_body(&body)?;
    let mut engine = state.engine.lock().expect("engine mutex never poisoned");
    let outcome = engine
        .claim(&owner, &request.name, &request.site_pubkey, now_unix())
        .map_err(|error| {
            log_if_internal(&error);
            ApiError::from(error)
        })?;
    Ok(Json(ClaimResponse {
        site_id: outcome.site.id,
        name: outcome.site.name,
        url: outcome.url,
        status: outcome.site.status.as_str().to_string(),
        already_claimed: outcome.already_claimed,
    }))
}

async fn list_sites(
    State(state): State<Arc<AppState>>,
    original_uri: OriginalUri,
    headers: HeaderMap,
) -> Result<Json<SiteListResponse>, ApiError> {
    let owner = authenticate(&state, &headers, "GET", &original_uri, None)?;
    let engine = state.engine.lock().expect("engine mutex never poisoned");
    let sites = engine.list_sites(&owner).map_err(|error| {
        log_if_internal(&error);
        ApiError::from(error)
    })?;
    Ok(Json(SiteListResponse { sites }))
}

async fn site_status(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    original_uri: OriginalUri,
    headers: HeaderMap,
) -> Result<Json<finitesites_proto::dto::SiteSummary>, ApiError> {
    let actor = authenticate(&state, &headers, "GET", &original_uri, None)?;
    let engine = state.engine.lock().expect("engine mutex never poisoned");
    let summary = engine.site_status(&actor, &name).map_err(|error| {
        log_if_internal(&error);
        ApiError::from(error)
    })?;
    Ok(Json(summary))
}

async fn set_sharing(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    original_uri: OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<finitesites_proto::dto::SharingResponse>, ApiError> {
    let actor = authenticate(&state, &headers, "POST", &original_uri, Some(&body))?;
    let request: SharingRequest = parse_json_body(&body)?;
    let mut engine = state.engine.lock().expect("engine mutex never poisoned");
    let response = engine
        .set_sharing(&actor, &name, &request, now_unix())
        .map_err(|error| {
            log_if_internal(&error);
            ApiError::from(error)
        })?;
    Ok(Json(response))
}

async fn begin_publish(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    original_uri: OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<PublishBeginResponse>, ApiError> {
    let site_key = authenticate(&state, &headers, "POST", &original_uri, Some(&body))?;
    let request: PublishBeginRequest = parse_json_body(&body)?;

    // Reject start commands the configured runner cannot execute NOW, so
    // the publisher gets a 400 instead of a published-but-dead site.
    if let Some(command) = &request.start_command
        && let Err(error) = state.apps.validate_start(command)
    {
        return Err(ApiError::bad_request(error.to_string()));
    }

    let mut engine = state.engine.lock().expect("engine mutex never poisoned");

    // The URL names a site; the signer must hold that exact site's key.
    let site = engine
        .resolve_site(&name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "not_found", "site not found"))?;
    if site.site_pubkey != site_key {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "not_authorized",
            "signer is not this site's key",
        ));
    }

    let outcome = engine
        .begin_publish(
            &site_key,
            &request.manifest,
            request.spa,
            request.start_command.as_deref(),
            now_unix(),
        )
        .map_err(|error| {
            log_if_internal(&error);
            ApiError::from(error)
        })?;
    Ok(Json(PublishBeginResponse {
        publish_id: outcome.publish_id,
        missing: outcome.missing,
    }))
}

async fn upload_blob(
    State(state): State<Arc<AppState>>,
    Path((publish_id, sha256)): Path<(String, String)>,
    original_uri: OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let site_key = authenticate(&state, &headers, "PUT", &original_uri, Some(&body))?;
    let mut engine = state.engine.lock().expect("engine mutex never poisoned");
    engine
        .upload_blob(&site_key, &publish_id, &sha256, &body, now_unix())
        .map_err(|error| {
            log_if_internal(&error);
            ApiError::from(error)
        })?;
    Ok((StatusCode::CREATED, Json(serde_json::json!({ "ok": true }))))
}

async fn finalize_publish(
    State(state): State<Arc<AppState>>,
    Path(publish_id): Path<String>,
    original_uri: OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<PublishFinalizeResponse>, ApiError> {
    let site_key = authenticate(&state, &headers, "POST", &original_uri, Some(&body))?;
    let mut engine = state.engine.lock().expect("engine mutex never poisoned");
    let outcome = engine
        .finalize_publish(&site_key, &publish_id, now_unix())
        .map_err(|error| {
            log_if_internal(&error);
            ApiError::from(error)
        })?;
    // App publishes go live (or restart) immediately after finalize. A
    // deploy failure is the operator's problem, not the publisher's: the
    // version is committed, reconcile retries at the next daemon start.
    if let Some(deploy) = &outcome.app {
        let bundle_path = engine.blob_file_path(&deploy.bundle_sha256);
        if let Err(error) = state.apps.deploy(deploy, &bundle_path, now_unix()) {
            eprintln!("app deploy failed for {}: {error}", deploy.site_id);
        }
    }
    Ok(Json(PublishFinalizeResponse {
        site_id: outcome.site_id,
        version_number: outcome.version_number,
        url: outcome.url,
        path_count: outcome.path_count,
        total_bytes: outcome.total_bytes,
    }))
}
