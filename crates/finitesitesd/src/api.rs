//! Control-plane API. Every mutation is authenticated with NIP-98 against
//! the exact URL and method received, and bodies are bound by payload hash.

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, OriginalUri, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};

use finitesites_engine::EngineError;
use finitesites_proto::dto::{
    ApiErrorBody, GitAuthRequest, GitAuthResponse, ProjectApplyRequest, ProjectApplyResponse,
    ProjectCollaboratorRemoveRequest, ProjectCollaboratorRemoveResponse, SharingRequest,
    SiteListResponse,
};
use finitesites_proto::limits::MAX_API_BODY_BYTES;
use finitesites_proto::{ProtoError, nip98};

use crate::mailer::{ProjectCollaboratorInvite, ViewerInvite};
use crate::server::{AppState, now_unix};

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/v1/healthz", get(healthz))
        .route("/api/v1/email-auth/request", post(request_email_login))
        .route("/api/v1/email-auth/redeem", post(redeem_email_login))
        .route("/api/v1/projects/apply", post(apply_project))
        .route("/api/v1/projects/{slug}/git-auth", post(auth_git))
        .route(
            "/api/v1/projects/{slug}/collaborators/remove",
            post(remove_project_collaborator),
        )
        .route("/api/v1/sites", get(list_sites))
        .route("/api/v1/sites/{name}", get(site_status))
        .route("/api/v1/sites/{name}/sharing", post(set_sharing))
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
            EngineError::SiteNotFound | EngineError::ProjectNotFound => {
                ApiError::new(StatusCode::NOT_FOUND, "not_found", message)
            }
            EngineError::TooManySites
            | EngineError::TooManyShares
            | EngineError::TooManyEmailKeys
            | EngineError::TooManyProjectCollaborators => {
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

/// Best-effort client identity for rate limiting. Spoofable headers only
/// weaken the per-IP budget; the per-email budget still binds.
fn client_key(headers: &HeaderMap) -> String {
    let from_header = headers
        .get("cf-connecting-ip")
        .or_else(|| headers.get("x-forwarded-for"))
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 64);
    from_header.unwrap_or("direct").to_string()
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

async fn request_email_login(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<finitesites_proto::dto::EmailLoginResponse>, ApiError> {
    let request: finitesites_proto::dto::EmailLoginRequest = parse_json_body(&body)?;
    let now = now_unix();
    let ip_key = format!("email-login-ip:{}", client_key(&headers));
    let email_key = format!(
        "email-login-email:{}",
        request.email.trim().to_ascii_lowercase()
    );
    let ip_allowed =
        state
            .login_limiter
            .check_and_record(&ip_key, crate::limiter::MAX_LINKS_PER_IP, now);
    let email_allowed =
        state
            .login_limiter
            .check_and_record(&email_key, crate::limiter::MAX_LINKS_PER_EMAIL, now);
    if !ip_allowed || !email_allowed {
        return Ok(Json(finitesites_proto::dto::EmailLoginResponse {
            email: request.email.trim().to_ascii_lowercase(),
        }));
    }

    let token = {
        let mut engine = state.engine.lock().expect("engine mutex never poisoned");
        engine
            .request_email_login(&request.email, now)
            .map_err(ApiError::from)?
    };
    if let Err(error) = state
        .mailer
        .send_email_login_token(&token.email, &token.token)
    {
        eprintln!("finitesitesd mail error: {error}");
        return Err(internal_error("mail delivery failure"));
    }
    Ok(Json(finitesites_proto::dto::EmailLoginResponse {
        email: token.email,
    }))
}

async fn redeem_email_login(
    State(state): State<Arc<AppState>>,
    original_uri: OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<finitesites_proto::dto::EmailRedeemResponse>, ApiError> {
    let actor = authenticate(&state, &headers, "POST", &original_uri, Some(&body))?;
    let request: finitesites_proto::dto::EmailRedeemRequest = parse_json_body(&body)?;
    let mut engine = state.engine.lock().expect("engine mutex never poisoned");
    let email = engine
        .redeem_email_login(&actor, &request.email, &request.token, now_unix())
        .map_err(|error| {
            log_if_internal(&error);
            ApiError::from(error)
        })?;
    Ok(Json(finitesites_proto::dto::EmailRedeemResponse {
        email,
        pubkey: actor,
    }))
}

async fn apply_project(
    State(state): State<Arc<AppState>>,
    Query(query): Query<InviteQuery>,
    original_uri: OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<ProjectApplyResponse>, ApiError> {
    let owner = authenticate(&state, &headers, "POST", &original_uri, Some(&body))?;
    let request: ProjectApplyRequest = parse_json_body(&body)?;
    let git_remote_url = git_remote_url(&state, &request.config.project.slug);
    let mut engine = state.engine.lock().expect("engine mutex never poisoned");
    let mut response = engine
        .apply_project(&owner, &request, git_remote_url, now_unix())
        .map_err(|error| {
            log_if_internal(&error);
            ApiError::from(error)
        })?;
    drop(engine);
    if !response.dry_run
        && let Some(project_id) = response.project_id.as_deref()
        && let Err(error) = crate::git::ensure_bare_project_repo(
            &state.data_dir,
            project_id,
            &state.git_hook_helper_path,
        )
    {
        eprintln!("finitesitesd project repo setup failed: {error}");
        return Err(internal_error("git repository setup failure"));
    }
    if query.send_invites && !response.dry_run {
        send_project_collaborator_invites(&state, &mut response)?;
    }
    Ok(Json(response))
}

async fn auth_git(
    State(state): State<Arc<AppState>>,
    Path(slug): Path<String>,
    original_uri: OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<GitAuthResponse>, ApiError> {
    let actor = authenticate(&state, &headers, "POST", &original_uri, Some(&body))?;
    let request: GitAuthRequest = parse_json_body(&body)?;
    let git_remote_url = git_remote_url(&state, &slug);
    let mut engine = state.engine.lock().expect("engine mutex never poisoned");
    let response = engine
        .mint_git_credential(
            &actor,
            &slug,
            request.email.as_deref(),
            git_remote_url,
            now_unix(),
        )
        .map_err(|error| {
            log_if_internal(&error);
            ApiError::from(error)
        })?;
    Ok(Json(response))
}

async fn remove_project_collaborator(
    State(state): State<Arc<AppState>>,
    Path(slug): Path<String>,
    original_uri: OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<ProjectCollaboratorRemoveResponse>, ApiError> {
    let owner = authenticate(&state, &headers, "POST", &original_uri, Some(&body))?;
    let request: ProjectCollaboratorRemoveRequest = parse_json_body(&body)?;
    let mut engine = state.engine.lock().expect("engine mutex never poisoned");
    let response = engine
        .remove_project_collaborator(&owner, &slug, &request.email, now_unix())
        .map_err(|error| {
            log_if_internal(&error);
            ApiError::from(error)
        })?;
    Ok(Json(response))
}

fn git_remote_url(state: &AppState, slug: &str) -> String {
    format!("{}/{}.git", state.git_base_url, slug)
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
    Query(query): Query<InviteQuery>,
    original_uri: OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<finitesites_proto::dto::SharingResponse>, ApiError> {
    let actor = authenticate(&state, &headers, "POST", &original_uri, Some(&body))?;
    let request: SharingRequest = parse_json_body(&body)?;
    if query.send_invites && request.add_emails.is_empty() {
        return Err(ApiError::bad_request(
            "send_invites requires at least one added email",
        ));
    }
    if query.send_invites && request.visibility.as_deref() != Some("shared") {
        return Err(ApiError::bad_request(
            "send_invites requires shared visibility",
        ));
    }
    let mut engine = state.engine.lock().expect("engine mutex never poisoned");
    let mut response = engine
        .set_sharing(&actor, &name, &request, now_unix())
        .map_err(|error| {
            log_if_internal(&error);
            ApiError::from(error)
        })?;
    let invite_links = if query.send_invites {
        assert_eq!(response.visibility, "shared");
        let mut links = Vec::with_capacity(request.add_emails.len());
        for email in &request.add_emails {
            match engine
                .request_login(&name, email, now_unix())
                .map_err(|error| {
                    log_if_internal(&error);
                    ApiError::from(error)
                })? {
                Some(link) => links.push(link),
                None => {
                    return Err(internal_error(
                        "could not create login link for shared invite email",
                    ));
                }
            }
        }
        links
    } else {
        Vec::new()
    };
    drop(engine);
    for link in &invite_links {
        let site_url = {
            let engine = state.engine.lock().expect("engine mutex never poisoned");
            engine.site_url(&link.site_name)
        };
        state
            .mailer
            .send_viewer_invite(&ViewerInvite {
                email: &link.email,
                site_name: &link.site_name,
                site_url: &site_url,
                login_url: &link.url,
            })
            .map_err(|error| {
                eprintln!("finitesitesd viewer invite mail error: {error}");
                internal_error("mail delivery failure")
            })?;
    }
    response.invited_emails = invite_links.iter().map(|link| link.email.clone()).collect();
    Ok(Json(response))
}

#[derive(serde::Deserialize, Default)]
struct InviteQuery {
    #[serde(default)]
    send_invites: bool,
}

fn send_project_collaborator_invites(
    state: &AppState,
    response: &mut ProjectApplyResponse,
) -> Result<(), ApiError> {
    if response.collaborators.is_empty() {
        return Ok(());
    }

    let mut tokens = Vec::with_capacity(response.collaborators.len());
    {
        let mut engine = state.engine.lock().expect("engine mutex never poisoned");
        for collaborator in &response.collaborators {
            let token = engine
                .request_email_login(&collaborator.email, now_unix())
                .map_err(|error| {
                    log_if_internal(&error);
                    ApiError::from(error)
                })?;
            tokens.push((token.email, collaborator.role.clone(), token.token));
        }
    }

    for (email, role, token) in &tokens {
        state
            .mailer
            .send_project_collaborator_invite(&ProjectCollaboratorInvite {
                email,
                project_slug: &response.slug,
                role,
                api_url: &state.api_url,
                git_remote_url: &response.git_remote_url,
                email_login_token: token,
                outputs: &response.outputs,
            })
            .map_err(|error| {
                eprintln!("finitesitesd project collaborator invite mail error: {error}");
                internal_error("mail delivery failure")
            })?;
    }
    response.invited_emails = tokens.iter().map(|(email, _, _)| email.clone()).collect();
    Ok(())
}
