//! Git smart HTTP bridge.
//!
//! Finite Sites authenticates and authorizes the Project Repository request,
//! then delegates the git protocol itself to `git http-backend`. Repositories
//! live on disk by internal Project ID; public URLs use Project Slugs.

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
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

use finitesites_proto::limits::MAX_GIT_HTTP_BODY_BYTES;
use finitesites_proto::project_config::{parse_project_config_toml, validate_project_slug};
use finitesites_proto::{ManifestFile, hex};
use sha2::{Digest, Sha256};

use crate::server::{AppState, now_unix};

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .fallback(handle_git)
        .layer(DefaultBodyLimit::max(MAX_GIT_HTTP_BODY_BYTES as usize))
        .with_state(state)
}

pub fn ensure_bare_project_repo(data_dir: &Path, project_id: &str) -> Result<PathBuf, String> {
    let root = project_root(data_dir);
    let repo = root.join(format!("{project_id}.git"));
    if repo.exists() {
        return Ok(repo);
    }
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
    Ok(repo)
}

pub fn project_root(data_dir: &Path) -> PathBuf {
    data_dir.join("git").join("projects")
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
    let repo = match ensure_bare_project_repo(&state.data_dir, &auth.project_id) {
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

    let refs_before = if wants_receive_pack {
        match read_refs(&repo) {
            Ok(refs) => refs,
            Err(error) => {
                eprintln!("git ref snapshot failed before receive-pack: {error}");
                return (StatusCode::INTERNAL_SERVER_ERROR, "git ref snapshot failed")
                    .into_response();
            }
        }
    } else {
        HashMap::new()
    };

    let request = GitBackendRequest {
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
            if wants_receive_pack
                && response.status().is_success()
                && let Err(error) = reconcile_receive_pack(&state, &auth, &repo, refs_before)
            {
                eprintln!("git receive-pack reconcile failed: {error}");
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

fn read_refs(repo: &Path) -> Result<HashMap<String, String>, String> {
    let output = Command::new("git")
        .arg("--git-dir")
        .arg(repo)
        .arg("for-each-ref")
        .arg("--format=%(refname) %(objectname)")
        .arg("refs/heads")
        .output()
        .map_err(|error| format!("cannot list refs: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git for-each-ref failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let text = String::from_utf8(output.stdout).map_err(|_| "git refs not utf8")?;
    let mut refs = HashMap::new();
    // Bounded by the number of branches in one Project Repository.
    for line in text.lines() {
        let Some((name, sha)) = line.split_once(' ') else {
            return Err("malformed git ref output".to_string());
        };
        if sha.len() != 40 {
            return Err("malformed git ref sha".to_string());
        }
        refs.insert(name.to_string(), sha.to_string());
    }
    Ok(refs)
}

fn reconcile_receive_pack(
    state: &Arc<AppState>,
    auth: &finitesites_engine::GitCredentialAuth,
    repo: &Path,
    refs_before: HashMap<String, String>,
) -> Result<(), String> {
    let refs_after = read_refs(repo)?;
    let zero = "0000000000000000000000000000000000000000";
    // Bounded by the number of branches in one Project Repository.
    for (ref_name, new_sha) in refs_after {
        if refs_before.get(&ref_name) == Some(&new_sha) {
            continue;
        }
        let old_sha = refs_before
            .get(&ref_name)
            .map(String::as_str)
            .unwrap_or(zero);
        let event = {
            let mut engine = state.engine.lock().expect("engine mutex never poisoned");
            let (event, inserted) = engine
                .record_git_ref_event(auth, &ref_name, old_sha, &new_sha, now_unix())
                .map_err(|error| error.to_string())?;
            if inserted { Some(event) } else { None }
        };
        if let Some(event) = event {
            reconcile_ref_event(state, repo, event.id, &auth.project_id, &ref_name, &new_sha)?;
        }
    }
    Ok(())
}

fn reconcile_ref_event(
    state: &Arc<AppState>,
    repo: &Path,
    event_id: i64,
    project_id: &str,
    ref_name: &str,
    new_sha: &str,
) -> Result<(), String> {
    let config = read_project_config_at(repo, new_sha)?;
    let branch = ref_name.strip_prefix("refs/heads/").unwrap_or(ref_name);
    let matching: Vec<_> = config
        .outputs
        .iter()
        .filter(|(_, output)| output.branch == branch)
        .collect();
    if matching.is_empty() {
        let mut engine = state.engine.lock().expect("engine mutex never poisoned");
        engine
            .mark_git_ref_event_ignored(event_id, now_unix())
            .map_err(|error| error.to_string())?;
        return Ok(());
    }
    if matching.len() > 1 {
        let mut engine = state.engine.lock().expect("engine mutex never poisoned");
        let _ = engine.mark_git_ref_event_failed(
            event_id,
            "multiple outputs match one pushed ref",
            now_unix(),
        );
        return Err("multiple outputs match one pushed ref".to_string());
    }
    let (output_id, output_config) = matching[0];
    let output_record = {
        let engine = state.engine.lock().expect("engine mutex never poisoned");
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
            let mut engine = state.engine.lock().expect("engine mutex never poisoned");
            let _ = engine.mark_git_ref_event_failed(event_id, &truncate_error(&error), now_unix());
            return Err(error);
        }
    };
    let mut engine = state.engine.lock().expect("engine mutex never poisoned");
    match engine.commit_project_output_version(
        &output_record.site_id,
        files,
        output_config.spa,
        now_unix(),
    ) {
        Ok(outcome) => {
            engine
                .mark_git_ref_event_deployed(
                    event_id,
                    &output_record.id,
                    &outcome.version_id,
                    now_unix(),
                )
                .map_err(|error| error.to_string())?;
            Ok(())
        }
        Err(error) => {
            let message = truncate_error(&error.to_string());
            let _ = engine.mark_git_ref_event_failed(event_id, &message, now_unix());
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
    project_root: PathBuf,
    path_info: String,
    query_string: String,
    method: String,
    content_type: String,
    remote_user: String,
    body: Vec<u8>,
}

fn run_git_http_backend(request: GitBackendRequest) -> Result<Response, String> {
    let mut child = Command::new("git")
        .arg("http-backend")
        .env("GIT_PROJECT_ROOT", &request.project_root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("REQUEST_METHOD", &request.method)
        .env("PATH_INFO", &request.path_info)
        .env("QUERY_STRING", &request.query_string)
        .env("CONTENT_TYPE", &request.content_type)
        .env("CONTENT_LENGTH", request.body.len().to_string())
        .env("REMOTE_USER", &request.remote_user)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
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
}
