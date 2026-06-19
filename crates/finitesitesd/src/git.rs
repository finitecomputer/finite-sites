//! Git smart HTTP bridge.
//!
//! Finite Sites authenticates and authorizes the Project Repository request,
//! then delegates the git protocol itself to `git http-backend`. Repositories
//! live on disk by internal Project ID; public URLs use Project Slugs.

use std::io::{BufRead as _, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, OriginalUri, State};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE, HeaderName, HeaderValue, WWW_AUTHENTICATE};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;

use finitesites_proto::limits::{
    MAX_GIT_HTTP_BODY_BYTES, MAX_GIT_REF_NAME_BYTES, MAX_GIT_REF_UPDATES_PER_PUSH,
};
use finitesites_proto::project_config::{parse_project_config_toml, validate_project_slug};
use finitesites_proto::{ManifestFile, hex};
use finitesites_store::Store;
use sha2::{Digest, Sha256};

use crate::server::{AppState, now_unix};

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .fallback(handle_git)
        .layer(DefaultBodyLimit::max(MAX_GIT_HTTP_BODY_BYTES as usize))
        .with_state(state)
}

pub fn ensure_bare_project_repo(
    data_dir: &Path,
    project_id: &str,
    hook_helper_path: &Path,
) -> Result<PathBuf, String> {
    let root = project_root(data_dir);
    let repo = root.join(format!("{project_id}.git"));
    if !repo.exists() {
        std::fs::create_dir_all(&root)
            .map_err(|error| format!("cannot create git project root: {error}"))?;
        let output = Command::new("git")
            .arg("init")
            .arg("--bare")
            .arg(&repo)
            .output()
            .map_err(|error| format!("cannot run git init --bare: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "git init --bare failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    let output = Command::new("git")
        .arg("--git-dir")
        .arg(&repo)
        .arg("config")
        .arg("http.receivepack")
        .arg("true")
        .output()
        .map_err(|error| format!("cannot configure bare repo: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git config http.receivepack failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    install_post_receive_hook(&repo, hook_helper_path)?;
    Ok(repo)
}

pub fn project_root(data_dir: &Path) -> PathBuf {
    data_dir.join("git").join("projects")
}

fn install_post_receive_hook(repo: &Path, hook_helper_path: &Path) -> Result<(), String> {
    let hooks_dir = repo.join("hooks");
    std::fs::create_dir_all(&hooks_dir)
        .map_err(|error| format!("cannot create hooks dir: {error}"))?;
    let helper = shell_single_quote(&hook_helper_path.to_string_lossy());
    let script = format!("#!/bin/sh\nexec {helper} git-post-receive\n");
    let hook_path = hooks_dir.join("post-receive");
    std::fs::write(&hook_path, script)
        .map_err(|error| format!("cannot write post-receive hook: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(&hook_path)
            .map_err(|error| format!("cannot stat post-receive hook: {error}"))?
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&hook_path, permissions)
            .map_err(|error| format!("cannot chmod post-receive hook: {error}"))?;
    }
    Ok(())
}

fn shell_single_quote(value: &str) -> String {
    let escaped = value.replace('\'', "'\\''");
    format!("'{escaped}'")
}

async fn handle_git(
    State(state): State<Arc<AppState>>,
    OriginalUri(original_uri): OriginalUri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if method != Method::GET && method != Method::POST {
        return (StatusCode::METHOD_NOT_ALLOWED, "method not allowed").into_response();
    }
    let Some((project_slug, suffix)) = parse_git_path(original_uri.path()) else {
        return (StatusCode::NOT_FOUND, "unknown git repository").into_response();
    };
    if validate_project_slug(&project_slug).is_err() {
        return (StatusCode::NOT_FOUND, "unknown git repository").into_response();
    }
    let Some((username, password)) = parse_basic_auth(&headers) else {
        return unauthorized_git();
    };

    let auth = {
        let engine = state.engine.lock().expect("engine mutex never poisoned");
        match engine.authenticate_git_credential(&username, &password, &project_slug, now_unix()) {
            Ok(auth) => auth,
            Err(_) => return unauthorized_git(),
        }
    };
    let wants_receive_pack = suffix.contains("git-receive-pack")
        || original_uri
            .query()
            .map(|query| query.contains("service=git-receive-pack"))
            .unwrap_or(false);
    if wants_receive_pack && !auth.can_push {
        return (StatusCode::FORBIDDEN, "git credential cannot push").into_response();
    }
    let repo = match ensure_bare_project_repo(
        &state.data_dir,
        &auth.project_id,
        &state.git_hook_helper_path,
    ) {
        Ok(repo) => repo,
        Err(error) => {
            eprintln!("git repo setup failed for {}: {error}", auth.project_id);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "git repository setup failed",
            )
                .into_response();
        }
    };
    assert!(repo.ends_with(format!("{}.git", auth.project_id)));

    let request = GitBackendRequest {
        data_dir: state.data_dir.clone(),
        project_id: auth.project_id.clone(),
        actor_principal_id: auth.principal_id.clone(),
        actor_agent_key_id: auth.actor_agent_key_id.clone(),
        git_credential_id: auth.git_credential_id.clone(),
        project_root: project_root(&state.data_dir),
        path_info: format!("/{}.git{suffix}", auth.project_id),
        query_string: original_uri.query().unwrap_or("").to_string(),
        method: method.as_str().to_string(),
        content_type: headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string(),
        remote_user: auth.principal_id.clone(),
        body: body.to_vec(),
    };
    let backend = tokio::task::spawn_blocking(move || run_git_http_backend(request)).await;
    match backend {
        Ok(Ok(response)) => {
            if wants_receive_pack && response.status().is_success() && state.git_auto_reconcile {
                let state = state.clone();
                let project_id = auth.project_id.clone();
                tokio::task::spawn_blocking(move || {
                    let mut engine = state.engine.lock().expect("engine mutex never poisoned");
                    if let Err(error) = reconcile_pending_events(
                        &mut engine,
                        &state.data_dir,
                        Some(&project_id),
                        now_unix(),
                    ) {
                        eprintln!("git receive-pack reconcile failed: {error}");
                    }
                });
            }
            response
        }
        Ok(Err(error)) => {
            eprintln!("git http-backend failed: {error}");
            (StatusCode::BAD_GATEWAY, "git backend failed").into_response()
        }
        Err(_join) => {
            (StatusCode::INTERNAL_SERVER_ERROR, "git backend task failed").into_response()
        }
    }
}

pub fn run_post_receive_hook_from_env() -> Result<(), String> {
    let data_dir = PathBuf::from(required_env("FINITE_SITES_DATA_DIR")?);
    let project_id = required_env("FINITE_GIT_PROJECT_ID")?;
    let actor_principal_id = required_env("FINITE_GIT_ACTOR_PRINCIPAL_ID")?;
    let actor_agent_key_id = std::env::var("FINITE_GIT_ACTOR_AGENT_KEY_ID")
        .ok()
        .filter(|value| !value.is_empty());
    let git_credential_id = required_env("FINITE_GIT_CREDENTIAL_ID")?;
    let mut store = Store::open(&data_dir.join("registry.db"))
        .map_err(|error| format!("cannot open registry: {error}"))?;
    let stdin = std::io::stdin();
    let mut count: u32 = 0;
    for line in stdin.lock().lines() {
        count += 1;
        if count > MAX_GIT_REF_UPDATES_PER_PUSH {
            return Err(format!(
                "one push may update at most {MAX_GIT_REF_UPDATES_PER_PUSH} refs"
            ));
        }
        let line = line.map_err(|error| format!("cannot read hook input: {error}"))?;
        let (old_sha, new_sha, ref_name) = parse_post_receive_line(&line)?;
        store
            .record_git_ref_event(
                &project_id,
                ref_name,
                old_sha,
                new_sha,
                &actor_principal_id,
                actor_agent_key_id.as_deref(),
                &git_credential_id,
                now_unix(),
            )
            .map_err(|error| format!("cannot record git ref event: {error}"))?;
    }
    Ok(())
}

fn required_env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} is required"))
}

fn parse_post_receive_line(line: &str) -> Result<(&str, &str, &str), String> {
    let mut parts = line.split_whitespace();
    let old_sha = parts.next().ok_or("missing old sha")?;
    let new_sha = parts.next().ok_or("missing new sha")?;
    let ref_name = parts.next().ok_or("missing ref name")?;
    if parts.next().is_some() {
        return Err("too many fields in post-receive input".to_string());
    }
    let old_is_hex = old_sha.bytes().all(|byte| byte.is_ascii_hexdigit());
    let new_is_hex = new_sha.bytes().all(|byte| byte.is_ascii_hexdigit());
    if old_sha.len() != 40 || new_sha.len() != 40 || !old_is_hex || !new_is_hex {
        return Err("git hook sha must be 40 hex chars".to_string());
    }
    if ref_name.is_empty() || ref_name.len() > MAX_GIT_REF_NAME_BYTES as usize {
        return Err("git ref name empty or too long".to_string());
    }
    Ok((old_sha, new_sha, ref_name))
}

pub fn reconcile_pending_events(
    engine: &mut finitesites_engine::Engine,
    data_dir: &Path,
    project_id: Option<&str>,
    now: u64,
) -> Result<u32, String> {
    let events = engine
        .pending_git_ref_events(project_id)
        .map_err(|error| error.to_string())?;
    let mut processed: u32 = 0;
    // Bounded by pending registry events.
    for event in events {
        let repo = project_root(data_dir).join(format!("{}.git", event.project_id));
        reconcile_ref_event(engine, &repo, &event, now)?;
        processed += 1;
    }
    Ok(processed)
}

fn reconcile_ref_event(
    engine: &mut finitesites_engine::Engine,
    repo: &Path,
    event: &finitesites_store::GitRefEventRecord,
    now: u64,
) -> Result<(), String> {
    let event_id = event.id;
    let project_id = event.project_id.as_str();
    let ref_name = event.ref_name.as_str();
    let new_sha = event.new_sha.as_str();
    let zero = "0000000000000000000000000000000000000000";
    if new_sha == zero {
        engine
            .mark_git_ref_event_ignored(event_id, now)
            .map_err(|error| error.to_string())?;
        return Ok(());
    }
    let config = read_project_config_at(repo, new_sha)?;
    let branch = ref_name.strip_prefix("refs/heads/").unwrap_or(ref_name);
    let matching: Vec<_> = config
        .outputs
        .iter()
        .filter(|(_, output)| output.branch == branch)
        .collect();
    if matching.is_empty() {
        engine
            .mark_git_ref_event_ignored(event_id, now)
            .map_err(|error| error.to_string())?;
        return Ok(());
    }
    if matching.len() > 1 {
        let _ = engine.mark_git_ref_event_failed(
            event_id,
            "multiple outputs match one pushed ref",
            now,
        );
        return Err("multiple outputs match one pushed ref".to_string());
    }
    let (output_id, output_config) = matching[0];
    let output_record = {
        let outputs = engine
            .project_outputs(project_id)
            .map_err(|error| error.to_string())?;
        outputs
            .into_iter()
            .find(|output| output.output_id == *output_id)
            .ok_or("project output missing from registry")?
    };
    let files = match files_from_git_archive(repo, new_sha, &output_config.path) {
        Ok(files) => files,
        Err(error) => {
            let _ = engine.mark_git_ref_event_failed(event_id, &truncate_error(&error), now);
            return Err(error);
        }
    };
    match engine.commit_project_output_version_for_git_event(
        &output_record.site_id,
        Some(event_id),
        files,
        output_config.spa,
        now,
    ) {
        Ok(outcome) => {
            engine
                .mark_git_ref_event_deployed(event_id, &output_record.id, &outcome.version_id, now)
                .map_err(|error| error.to_string())?;
            Ok(())
        }
        Err(error) => {
            let message = truncate_error(&error.to_string());
            let _ = engine.mark_git_ref_event_failed(event_id, &message, now);
            Err(error.to_string())
        }
    }
}

fn read_project_config_at(
    repo: &Path,
    commit: &str,
) -> Result<finitesites_proto::project_config::ProjectConfig, String> {
    let spec = format!("{commit}:finite.toml");
    let output = Command::new("git")
        .arg("--git-dir")
        .arg(repo)
        .arg("show")
        .arg(spec)
        .output()
        .map_err(|error| format!("cannot read finite.toml: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "finite.toml is required for deploys: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let text = String::from_utf8(output.stdout).map_err(|_| "finite.toml is not utf8")?;
    parse_project_config_toml(&text).map_err(|error| error.to_string())
}

fn files_from_git_archive(
    repo: &Path,
    commit: &str,
    output_path: &str,
) -> Result<Vec<(ManifestFile, Vec<u8>)>, String> {
    let mut command = Command::new("git");
    command
        .arg("--git-dir")
        .arg(repo)
        .arg("archive")
        .arg("--format=tar")
        .arg(commit);
    if output_path != "." {
        command.arg(output_path);
    }
    let output = command
        .output()
        .map_err(|error| format!("cannot archive output path: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cannot archive output path `{output_path}`: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let mut archive = tar::Archive::new(output.stdout.as_slice());
    let mut files = Vec::new();
    let entries = archive
        .entries()
        .map_err(|error| format!("cannot read git archive: {error}"))?;
    // Bounded by manifest validation after collection.
    for entry in entries {
        let mut entry = entry.map_err(|error| format!("cannot read archive entry: {error}"))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|error| format!("cannot read archive path: {error}"))?
            .into_owned();
        let relative = relative_archive_path(&path, output_path)?;
        if should_skip_project_file(&relative) {
            continue;
        }
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|error| format!("cannot read archive file: {error}"))?;
        let manifest_path = format!("/{}", relative.replace('\\', "/"));
        let sha256 = hex::encode(&Sha256::digest(&bytes));
        files.push((
            ManifestFile {
                path: manifest_path,
                sha256,
                size: bytes.len() as u64,
            },
            bytes,
        ));
    }
    if files.is_empty() {
        return Err("configured output path contains no deployable files".to_string());
    }
    Ok(files)
}

fn relative_archive_path(path: &Path, output_path: &str) -> Result<String, String> {
    let relative = if output_path == "." {
        path
    } else {
        path.strip_prefix(output_path)
            .map_err(|_| "archive entry escaped configured output path")?
    };
    relative
        .to_str()
        .map(str::to_string)
        .ok_or_else(|| "archive path is not utf8".to_string())
}

fn should_skip_project_file(relative: &str) -> bool {
    if relative == "finite.toml" {
        return true;
    }
    relative
        .split('/')
        .any(|part| part.starts_with('.') || matches!(part, "node_modules" | "target" | "dist"))
        && relative != "index.html"
}

fn truncate_error(error: &str) -> String {
    const MAX_ERROR: usize = 512;
    if error.len() <= MAX_ERROR {
        return error.to_string();
    }
    error[..MAX_ERROR].to_string()
}

struct GitBackendRequest {
    data_dir: PathBuf,
    project_id: String,
    actor_principal_id: String,
    actor_agent_key_id: Option<String>,
    git_credential_id: String,
    project_root: PathBuf,
    path_info: String,
    query_string: String,
    method: String,
    content_type: String,
    remote_user: String,
    body: Vec<u8>,
}

fn run_git_http_backend(request: GitBackendRequest) -> Result<Response, String> {
    let mut child_command = Command::new("git");
    child_command
        .arg("http-backend")
        .env("GIT_PROJECT_ROOT", &request.project_root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("REQUEST_METHOD", &request.method)
        .env("PATH_INFO", &request.path_info)
        .env("QUERY_STRING", &request.query_string)
        .env("CONTENT_TYPE", &request.content_type)
        .env("CONTENT_LENGTH", request.body.len().to_string())
        .env("REMOTE_USER", &request.remote_user)
        .env("FINITE_SITES_DATA_DIR", &request.data_dir)
        .env("FINITE_GIT_PROJECT_ID", &request.project_id)
        .env("FINITE_GIT_ACTOR_PRINCIPAL_ID", &request.actor_principal_id)
        .env("FINITE_GIT_CREDENTIAL_ID", &request.git_credential_id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(agent_key_id) = &request.actor_agent_key_id {
        child_command.env("FINITE_GIT_ACTOR_AGENT_KEY_ID", agent_key_id);
    }
    let mut child = child_command
        .spawn()
        .map_err(|error| format!("cannot spawn git http-backend: {error}"))?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(&request.body)
            .map_err(|error| format!("cannot write git request body: {error}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("cannot read git http-backend output: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git http-backend exited {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    cgi_response_to_http(&output.stdout)
}

fn cgi_response_to_http(bytes: &[u8]) -> Result<Response, String> {
    let (head, body) = split_cgi_response(bytes).ok_or("git backend returned no headers")?;
    let head_text = std::str::from_utf8(head).map_err(|_| "git backend headers not utf8")?;
    let mut status = StatusCode::OK;
    let mut builder = Response::builder();
    // Bounded by git's finite CGI header block.
    for line in head_text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if let Some(raw_status) = line.strip_prefix("Status:") {
            let code = raw_status
                .split_whitespace()
                .next()
                .ok_or("empty git status header")?
                .parse::<u16>()
                .map_err(|_| "invalid git status header")?;
            status = StatusCode::from_u16(code).map_err(|_| "invalid git status code")?;
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err("malformed git cgi header".to_string());
        };
        let name = HeaderName::from_bytes(name.trim().as_bytes())
            .map_err(|_| "invalid git header name")?;
        let value = HeaderValue::from_str(value.trim()).map_err(|_| "invalid git header value")?;
        builder = builder.header(name, value);
    }
    builder
        .status(status)
        .body(Body::from(body.to_vec()))
        .map_err(|error| format!("cannot build git response: {error}"))
}

fn split_cgi_response(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
        return Some((&bytes[..index], &bytes[index + 4..]));
    }
    if let Some(index) = bytes.windows(2).position(|window| window == b"\n\n") {
        return Some((&bytes[..index], &bytes[index + 2..]));
    }
    None
}

fn parse_git_path(path: &str) -> Option<(String, String)> {
    let rest = path.strip_prefix('/')?;
    let (slug, suffix) = rest.split_once(".git")?;
    if slug.is_empty() {
        return None;
    }
    if !suffix.is_empty() && !suffix.starts_with('/') {
        return None;
    }
    Some((slug.to_string(), suffix.to_string()))
}

fn parse_basic_auth(headers: &HeaderMap) -> Option<(String, String)> {
    let raw = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let encoded = raw.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (username, password) = decoded.split_once(':')?;
    if username.is_empty() || password.is_empty() {
        return None;
    }
    Some((username.to_string(), password.to_string()))
}

fn unauthorized_git() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(
            WWW_AUTHENTICATE,
            HeaderValue::from_static("Basic realm=\"Finite Sites Git\""),
        )],
        "git authentication required",
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_path_parsing_rejects_non_repo_paths() {
        assert_eq!(
            parse_git_path("/demo.git/info/refs"),
            Some(("demo".to_string(), "/info/refs".to_string()))
        );
        assert_eq!(
            parse_git_path("/demo.git"),
            Some(("demo".to_string(), "".to_string()))
        );
        assert_eq!(parse_git_path("/demo.gitx/info"), None);
        assert_eq!(parse_git_path("/.git/info"), None);
        assert_eq!(parse_git_path("/demo/info"), None);
    }

    #[test]
    fn cgi_response_parses_status_headers_and_body() {
        let response =
            cgi_response_to_http(b"Status: 201 Created\r\nContent-Type: text/plain\r\n\r\nhello")
                .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            HeaderValue::from_static("text/plain")
        );
    }

    #[test]
    fn post_receive_line_parser_rejects_malformed_input() {
        let zero = "0000000000000000000000000000000000000000";
        let one = "1111111111111111111111111111111111111111";
        assert_eq!(
            parse_post_receive_line(&format!("{zero} {one} refs/heads/main")).unwrap(),
            (zero, one, "refs/heads/main")
        );
        assert!(parse_post_receive_line(&format!("{zero} {one}")).is_err());
        assert!(parse_post_receive_line(&format!("{zero} not-a-sha refs/heads/main")).is_err());
        assert!(parse_post_receive_line(&format!("{zero} {one} refs/heads/main extra")).is_err());
    }
}
