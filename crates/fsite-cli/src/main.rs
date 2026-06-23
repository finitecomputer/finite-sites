//! `fsite` — the agent-facing CLI for Finite Sites.
//!
//! Commands hide nostr, keys, manifests, and blob mechanics; the agent only
//! sees names, paths, emails, and URLs:
//!
//!   fsite whoami
//!   fsite project apply --json project.json --dry-run --output json
//!   fsite auth git PROJECT [--email EMAIL] [--store] [--output json]
//!   fsite status NAME
//!   fsite list
//!   fsite share NAME [--shared|--private] [--public --yes-public]
//!                    [--add-email E]... [--remove-email E]...
//!
//! Server address comes from FINITE_SITES_API (default https://api.finite.chat).

mod api;
mod keys;

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use thiserror::Error;

use finitesites_proto::dto::{
    GitAuthRequest, GitAuthResponse, ProjectApplyRequest, ProjectCollaboratorRemoveRequest,
    SharingRequest,
};
use finitesites_proto::limits::MAX_EMAILS_PER_SHARING_REQUEST;
use finitesites_proto::npub;
use finitesites_proto::project_config::parse_project_config_toml;

#[derive(Debug, Error)]
pub enum CliError {
    #[error("{0}")]
    Usage(String),
    #[error("key error: {0}")]
    Key(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("server error: {0}")]
    Api(String),
    #[error("server error: {method} {path}: {status}: {message}")]
    ApiStatus {
        method: String,
        path: String,
        status: u16,
        message: String,
    },
    #[error("network error: {0}")]
    Http(String),
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("fsite: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), CliError> {
    let Some(command) = args.first() else {
        return Err(CliError::Usage(usage()));
    };
    match command.as_str() {
        "whoami" => no_args_or_help(&args[1..], "fsite whoami", whoami_help(), whoami),
        "email-login" => email_login(&args[1..]),
        "email-redeem" => email_redeem(&args[1..]),
        "describe" => describe(&args[1..]),
        "project" => project_command(&args[1..]),
        "auth" => auth_command(&args[1..]),
        "status" => status(&args[1..]),
        "list" => no_args_or_help(&args[1..], "fsite list", list_help(), list),
        "share" => share(&args[1..]),
        "--help" | "help" => {
            println!("{}", usage());
            Ok(())
        }
        other => Err(CliError::Usage(format!(
            "unknown command `{other}`\n{}",
            usage()
        ))),
    }
}

fn is_help_arg(arg: &str) -> bool {
    arg == "--help" || arg == "-h" || arg == "help"
}

fn help_requested(args: &[String]) -> bool {
    args.iter().any(|arg| is_help_arg(arg))
}

fn print_help(text: &str) -> Result<(), CliError> {
    println!("{text}");
    Ok(())
}

fn no_args_or_help(
    args: &[String],
    command: &str,
    help: &str,
    action: fn() -> Result<(), CliError>,
) -> Result<(), CliError> {
    if help_requested(args) {
        return print_help(help);
    }
    if !args.is_empty() {
        return Err(CliError::Usage(format!("usage: {command}")));
    }
    action()
}

fn usage() -> String {
    "usage:\n  fsite whoami\n  fsite email-login EMAIL\n  \
     fsite email-redeem EMAIL TOKEN\n  \
     fsite describe [workflow NAME] [--output json]\n  \
     fsite project apply --json FILE|- [--dry-run] [--send-invite] [--output json] [--config finite.toml]\n  \
     fsite project collaborator remove PROJECT --email EMAIL [--output json]\n  \
     fsite auth git PROJECT [--email EMAIL] [--store] [--output json]\n  \
     fsite status NAME\n  fsite list\n  fsite share NAME [--shared|--private] \
     [--public --yes-public] [--send-invite] [--add-email EMAIL]... [--remove-email EMAIL]..."
        .to_string()
}

fn whoami_help() -> &'static str {
    "usage: fsite whoami\n\nPrint the local User Key npub and key file path. Creates the identity if missing."
}

fn email_login_help() -> &'static str {
    "usage: fsite email-login EMAIL\n\nRequest a one-time email verification token for an External Principal."
}

fn email_redeem_help() -> &'static str {
    "usage: fsite email-redeem EMAIL TOKEN\n\nVerify this machine's Email Key for an External Principal."
}

fn describe_help() -> &'static str {
    "usage: fsite describe [workflow NAME] [--output json]\n\nMachine-readable command and workflow discovery. Workflows: project-config, initial-project-publish, edit-shared-project, grant-collaborator, remove-collaborator."
}

fn project_help() -> &'static str {
    "usage:\n  fsite project apply --json FILE|- [--dry-run] [--send-invite] [--output json] [--config finite.toml]\n  fsite project collaborator remove PROJECT --email EMAIL [--output json]\n\nCreate/update Project Repositories and manage Project Collaborators. See: fsite describe workflow project-config --output json"
}

fn project_apply_help() -> &'static str {
    "usage: fsite project apply --json FILE|- [--dry-run] [--send-invite] [--output json] [--config finite.toml]\n\nReads Project apply JSON, validates it, optionally writes finite.toml, and creates/updates Project Outputs. Use --send-invite to email Project Collaborators with fsite/git instructions. Use --dry-run before mutating. Use --output json for agent workflows."
}

fn project_collaborator_help() -> &'static str {
    "usage: fsite project collaborator remove PROJECT --email EMAIL [--output json]\n\nRemove a Project Collaborator by External Principal email and revoke that Principal's active Git Credentials for this Project."
}

fn project_collaborator_remove_help() -> &'static str {
    "usage: fsite project collaborator remove PROJECT --email EMAIL [--output json]\n\nOwner-authenticated revocation. Safe to replay: removed=false means the collaborator was already inactive or unknown."
}

fn auth_help() -> &'static str {
    "usage: fsite auth git PROJECT [--email EMAIL] [--store] [--output json]\n\nMint scoped HTTPS Git Credentials. Omit --email when the local User Key is already a native Project Collaborator. Use --store to save the credential for standard git without printing the password."
}

fn auth_git_help() -> &'static str {
    "usage: fsite auth git PROJECT [--email EMAIL] [--store] [--output json]\n\nReturns git_remote_url, username, and password for standard git clone/push against one Project Repository. With --store, configures a path-aware Git credential helper for the Finite Git host, stores the scoped credential, and omits the password from output."
}

fn status_help() -> &'static str {
    "usage: fsite status NAME\n\nPrint registry status for one Finite Site."
}

fn list_help() -> &'static str {
    "usage: fsite list\n\nList Finite Sites owned by the local User Key."
}

fn share_help() -> &'static str {
    "usage: fsite share NAME [--shared|--private] [--public --yes-public] [--send-invite] [--add-email EMAIL]... [--remove-email EMAIL]...\n\nChange output Visibility or email Share rows. Use --send-invite with --add-email to email one-time viewer links. Public sharing requires explicit human confirmation and --yes-public."
}

fn describe(args: &[String]) -> Result<(), CliError> {
    if help_requested(args) {
        return print_help(describe_help());
    }
    let mut positionals: Vec<&String> = Vec::new();
    let mut output_json = false;
    let mut index: usize = 0;
    // Bounded by argv length.
    while index < args.len() {
        match args[index].as_str() {
            "--output" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| CliError::Usage("--output needs a value".to_string()))?;
                if value != "json" {
                    return Err(CliError::Usage(
                        "only --output json is supported".to_string(),
                    ));
                }
                output_json = true;
                index += 2;
            }
            other if other.starts_with("--") => {
                return Err(CliError::Usage(format!("unknown flag `{other}`")));
            }
            _ => {
                positionals.push(&args[index]);
                index += 1;
            }
        }
    }

    let value = match positionals.as_slice() {
        [] => describe_commands(),
        [workflow, name] if workflow.as_str() == "workflow" => describe_workflow(name)?,
        _ => {
            return Err(CliError::Usage(
                "usage: fsite describe [workflow NAME] [--output json]".to_string(),
            ));
        }
    };
    if output_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&value).expect("describe json serializes")
        );
    } else {
        println!("machine-readable description (use --output json in agent workflows):");
        println!(
            "{}",
            serde_json::to_string_pretty(&value).expect("describe json serializes")
        );
    }
    Ok(())
}

fn describe_commands() -> serde_json::Value {
    serde_json::json!({
        "commands": [
            {
                "name": "project apply",
                "summary": "Create or update a Project Repository and finite.toml-described outputs.",
                "usage": "fsite project apply --json FILE|- [--dry-run] [--send-invite] [--output json] [--config finite.toml]"
            },
            {
                "name": "share",
                "summary": "Change Project Output visibility and email view shares.",
                "usage": "fsite share NAME [--shared|--private] [--public --yes-public] [--send-invite] [--add-email EMAIL]... [--remove-email EMAIL]..."
            },
            {
                "name": "project collaborator remove",
                "summary": "Remove a Project Collaborator and revoke active Git Credentials for that Principal.",
                "usage": "fsite project collaborator remove PROJECT --email EMAIL [--output json]"
            },
            {
                "name": "auth git",
                "summary": "Mint a scoped HTTPS Git Credential for a native Project Collaborator or verified External Principal.",
                "usage": "fsite auth git PROJECT [--email EMAIL] [--store] [--output json]"
            },
            {
                "name": "describe workflow",
                "summary": "Print machine-readable workflow guidance.",
                "usage": "fsite describe workflow NAME --output json"
            }
        ],
        "workflows": [
            "project-config",
            "initial-project-publish",
            "edit-shared-project",
            "grant-collaborator",
            "remove-collaborator"
        ]
    })
}

fn describe_workflow(name: &str) -> Result<serde_json::Value, CliError> {
    let value = match name {
        "project-config" => serde_json::json!({
            "name": "project-config",
            "file": "finite.toml",
            "schema": {
                "project.slug": "lowercase DNS-label-shaped Project Slug",
                "outputs.<id>.kind": "site",
                "outputs.<id>.site_name": "Finite Site name for this Project Output",
                "outputs.<id>.branch": "Deploy Branch, usually main",
                "outputs.<id>.path": "relative directory containing committed deploy bytes",
                "outputs.<id>.spa": "boolean; true serves /index.html for unknown static paths"
            },
            "example": "[project]\nslug = \"finitechat-native\"\n\n[outputs.mockup]\nkind = \"site\"\nsite_name = \"finitechat-native-mockup\"\nbranch = \"main\"\npath = \".\"\nspa = false\n"
        }),
        "initial-project-publish" => serde_json::json!({
            "name": "initial-project-publish",
            "steps": [
                "Create or build committed deploy bytes in the path selected by finite.toml.",
                "Run fsite project apply --json project.json --dry-run --output json.",
                "Run fsite project apply --json project.json --output json. Add --send-invite to email Project Collaborators after the real apply.",
                "Commit finite.toml and deploy bytes to the Project Repository.",
                "Push the Deploy Branch; Finite Sites validates committed bytes and creates a Version."
            ]
        }),
        "edit-shared-project" => serde_json::json!({
            "name": "edit-shared-project",
            "steps": [
                "If you are a native Project Collaborator, run fsite auth git PROJECT --store --output json.",
                "If you are using an External Principal email, run fsite email-login EDITOR_EMAIL if this machine is not verified.",
                "For email auth, run fsite email-redeem EDITOR_EMAIL TOKEN_FROM_EMAIL.",
                "For email auth, run fsite auth git PROJECT --email EDITOR_EMAIL --store --output json.",
                "Clone using the returned git_remote_url; the password is stored in Git's credential helper and is not printed.",
                "Clone, edit source, run the project's tests/build, commit deploy bytes, and push the Deploy Branch."
            ]
        }),
        "grant-collaborator" => serde_json::json!({
            "name": "grant-collaborator",
            "steps": [
                "Add the collaborator email to the collaborators array in the project apply JSON.",
                "Use role editor for agents that may push deploy bytes.",
                "Run fsite project apply --json project.json --dry-run --output json.",
                "Run fsite project apply --json project.json --send-invite --output json after confirming the plan."
            ]
        }),
        "remove-collaborator" => serde_json::json!({
            "name": "remove-collaborator",
            "steps": [
                "Use the Project owner identity, not the collaborator email key.",
                "Run fsite project collaborator remove PROJECT --email COLLABORATOR_EMAIL --output json.",
                "Check removed and revoked_git_credentials in the JSON response.",
                "If the Project Output should no longer be viewable by that email, also run fsite share SITE_NAME --remove-email COLLABORATOR_EMAIL."
            ]
        }),
        other => {
            return Err(CliError::Usage(format!(
                "unknown workflow `{other}` (project-config|initial-project-publish|edit-shared-project|grant-collaborator|remove-collaborator)"
            )));
        }
    };
    Ok(value)
}

fn project_command(args: &[String]) -> Result<(), CliError> {
    let Some((subcommand, rest)) = args.split_first() else {
        return Err(CliError::Usage(project_help().to_string()));
    };
    match subcommand.as_str() {
        value if is_help_arg(value) => print_help(project_help()),
        "apply" => project_apply(rest),
        "collaborator" => project_collaborator_command(rest),
        other => Err(CliError::Usage(format!(
            "unknown project command `{other}`"
        ))),
    }
}

fn project_collaborator_command(args: &[String]) -> Result<(), CliError> {
    let Some((subcommand, rest)) = args.split_first() else {
        return Err(CliError::Usage(project_collaborator_help().to_string()));
    };
    match subcommand.as_str() {
        value if is_help_arg(value) => print_help(project_collaborator_help()),
        "remove" => project_collaborator_remove(rest),
        other => Err(CliError::Usage(format!(
            "unknown project collaborator command `{other}`"
        ))),
    }
}

fn project_collaborator_remove(args: &[String]) -> Result<(), CliError> {
    if help_requested(args) {
        return print_help(project_collaborator_remove_help());
    }
    let mut project: Option<String> = None;
    let mut email: Option<String> = None;
    let mut output_json = false;
    let mut index: usize = 0;
    // Bounded by argv length.
    while index < args.len() {
        match args[index].as_str() {
            "--email" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| CliError::Usage("--email needs a value".to_string()))?;
                email = Some(value.clone());
                index += 2;
            }
            "--output" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| CliError::Usage("--output needs a value".to_string()))?;
                if value != "json" {
                    return Err(CliError::Usage(
                        "only --output json is supported".to_string(),
                    ));
                }
                output_json = true;
                index += 2;
            }
            other if other.starts_with("--") => {
                return Err(CliError::Usage(format!("unknown flag `{other}`")));
            }
            value => {
                if project.is_some() {
                    return Err(CliError::Usage(
                        project_collaborator_remove_help().to_string(),
                    ));
                }
                project = Some(value.to_string());
                index += 1;
            }
        }
    }

    let project =
        project.ok_or_else(|| CliError::Usage(project_collaborator_remove_help().to_string()))?;
    let email =
        email.ok_or_else(|| CliError::Usage(project_collaborator_remove_help().to_string()))?;
    let identity = keys::load_or_create_identity()?;
    let client = api::Client::from_env();
    let response = client.remove_project_collaborator(
        &identity,
        &project,
        &ProjectCollaboratorRemoveRequest { email },
    )?;
    if output_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&response).expect("response serializes")
        );
    } else {
        println!("project: {}", response.project_slug);
        println!("email:   {}", response.email);
        println!("removed: {}", response.removed);
        println!(
            "revoked git credentials: {}",
            response.revoked_git_credentials
        );
    }
    Ok(())
}

fn project_apply(args: &[String]) -> Result<(), CliError> {
    if help_requested(args) {
        return print_help(project_apply_help());
    }
    let mut json_path: Option<String> = None;
    let mut dry_run = false;
    let mut send_invites = false;
    let mut output_json = false;
    let mut config_path = PathBuf::from("finite.toml");
    let mut index: usize = 0;
    // Bounded by argv length.
    while index < args.len() {
        match args[index].as_str() {
            "--json" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| CliError::Usage("--json needs FILE or -".to_string()))?;
                json_path = Some(value.clone());
                index += 2;
            }
            "--dry-run" => {
                dry_run = true;
                index += 1;
            }
            "--send-invite" => {
                send_invites = true;
                index += 1;
            }
            "--output" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| CliError::Usage("--output needs a value".to_string()))?;
                if value != "json" {
                    return Err(CliError::Usage(
                        "only --output json is supported".to_string(),
                    ));
                }
                output_json = true;
                index += 2;
            }
            "--config" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| CliError::Usage("--config needs a path".to_string()))?;
                config_path = PathBuf::from(value);
                index += 2;
            }
            other => return Err(CliError::Usage(format!("unknown flag `{other}`"))),
        }
    }
    let json_path = json_path.ok_or_else(|| CliError::Usage(project_apply_help().to_string()))?;
    let mut request: ProjectApplyRequest = serde_json::from_slice(&read_json_input(&json_path)?)
        .map_err(|error| CliError::Usage(format!("invalid project apply json: {error}")))?;
    if dry_run {
        request.dry_run = true;
    }
    request
        .config
        .validate()
        .map_err(|error| CliError::Usage(error.to_string()))?;
    validate_project_config_file(&config_path, &request)?;

    let identity = keys::load_or_create_identity()?;
    let client = api::Client::from_env();
    let response = client.apply_project(&identity, &request, send_invites)?;
    if !response.dry_run {
        write_project_config_file_if_missing(&config_path, &request)?;
    }
    if output_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&response).expect("response serializes")
        );
    } else {
        println!("project: {}", response.slug);
        println!("git:     {}", response.git_remote_url);
        for output in &response.outputs {
            println!(
                "output:  {} {} -> {} ({})",
                output.output_id, output.kind, output.site_url, output.path
            );
        }
        if !response.invited_emails.is_empty() {
            println!("invited: {}", response.invited_emails.join(", "));
        }
        if response.dry_run {
            println!("dry-run: no server state changed and finite.toml was not written");
            if send_invites {
                println!("dry-run: no invite email was sent");
            }
        }
    }
    Ok(())
}

fn read_json_input(path: &str) -> Result<Vec<u8>, CliError> {
    if path == "-" {
        let mut bytes = Vec::new();
        std::io::stdin()
            .read_to_end(&mut bytes)
            .map_err(|error| CliError::Io(format!("cannot read stdin: {error}")))?;
        return Ok(bytes);
    }
    std::fs::read(path).map_err(|error| CliError::Io(format!("cannot read {path}: {error}")))
}

fn validate_project_config_file(
    path: &Path,
    request: &ProjectApplyRequest,
) -> Result<(), CliError> {
    if path.exists() {
        let existing = std::fs::read_to_string(path)
            .map_err(|error| CliError::Io(format!("cannot read {}: {error}", path.display())))?;
        let parsed = parse_project_config_toml(&existing)
            .map_err(|error| CliError::Usage(format!("{} is invalid: {error}", path.display())))?;
        if parsed != request.config {
            return Err(CliError::Usage(format!(
                "{} already exists and does not match --json config",
                path.display()
            )));
        }
        return Ok(());
    }
    Ok(())
}

fn write_project_config_file_if_missing(
    path: &Path,
    request: &ProjectApplyRequest,
) -> Result<(), CliError> {
    if path.exists() {
        return Ok(());
    }
    let expected = request
        .config
        .to_toml_string()
        .map_err(|error| CliError::Usage(error.to_string()))?;
    std::fs::write(path, expected)
        .map_err(|error| CliError::Io(format!("cannot write {}: {error}", path.display())))?;
    Ok(())
}

fn auth_command(args: &[String]) -> Result<(), CliError> {
    let Some((subcommand, rest)) = args.split_first() else {
        return Err(CliError::Usage(auth_help().to_string()));
    };
    match subcommand.as_str() {
        value if is_help_arg(value) => print_help(auth_help()),
        "git" => auth_git(rest),
        other => Err(CliError::Usage(format!("unknown auth command `{other}`"))),
    }
}

fn auth_git(args: &[String]) -> Result<(), CliError> {
    if help_requested(args) {
        return print_help(auth_git_help());
    }
    let options = parse_auth_git_args(args)?;
    let key = match &options.email {
        Some(email) => keys::load_or_create_email_key(email)?,
        None => keys::load_or_create_identity()?,
    };
    let client = api::Client::from_env();
    let response = client.auth_git(
        &key,
        &options.project,
        &GitAuthRequest {
            email: options.email.clone(),
        },
    )?;
    if options.store {
        store_git_credential(&response)?;
    }
    print_git_auth_response(&response, options.output_json, options.store)
}

#[derive(Debug, PartialEq, Eq)]
struct AuthGitOptions {
    project: String,
    email: Option<String>,
    output_json: bool,
    store: bool,
}

fn parse_auth_git_args(args: &[String]) -> Result<AuthGitOptions, CliError> {
    let mut positionals: Vec<&String> = Vec::new();
    let mut email: Option<String> = None;
    let mut output_json = false;
    let mut store = false;
    let mut index: usize = 0;
    // Bounded by argv length.
    while index < args.len() {
        match args[index].as_str() {
            "--email" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| CliError::Usage("--email needs a value".to_string()))?;
                email = Some(value.clone());
                index += 2;
            }
            "--output" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| CliError::Usage("--output needs a value".to_string()))?;
                if value != "json" {
                    return Err(CliError::Usage(
                        "only --output json is supported".to_string(),
                    ));
                }
                output_json = true;
                index += 2;
            }
            "--store" => {
                store = true;
                index += 1;
            }
            other if other.starts_with("--") => {
                return Err(CliError::Usage(format!("unknown flag `{other}`")));
            }
            _ => {
                positionals.push(&args[index]);
                index += 1;
            }
        }
    }
    let [project] = positionals.as_slice() else {
        return Err(CliError::Usage(
            "usage: fsite auth git PROJECT [--email EMAIL] [--store] [--output json]".to_string(),
        ));
    };
    Ok(AuthGitOptions {
        project: (*project).clone(),
        email,
        output_json,
        store,
    })
}

fn print_git_auth_response(
    response: &GitAuthResponse,
    output_json: bool,
    stored: bool,
) -> Result<(), CliError> {
    if output_json {
        if stored {
            let value = serde_json::json!({
                "project_slug": response.project_slug,
                "git_remote_url": response.git_remote_url,
                "credential_id": response.credential_id,
                "username": response.username,
                "expires_at": response.expires_at,
                "stored": true
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&value).expect("response serializes")
            );
        } else {
            println!(
                "{}",
                serde_json::to_string_pretty(&response).expect("response serializes")
            );
        }
    } else {
        println!("git:      {}", response.git_remote_url);
        println!("username: {}", response.username);
        if stored {
            println!("password: stored in git credential helper");
            println!("clone:    git clone {}", response.git_remote_url);
        } else {
            println!("password: {}", response.password);
            println!("tip:      rerun with --store to save it without printing it");
        }
        println!("scope:    this credential works for this project only");
        println!("expires:  never, unless the Project Collaborator is removed");
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct GitCredentialContext {
    protocol: String,
    host: String,
    path: String,
}

fn git_credential_context(remote_url: &str) -> Result<GitCredentialContext, CliError> {
    let Some((protocol, rest)) = remote_url.split_once("://") else {
        return Err(CliError::Usage(
            "git_remote_url must include http:// or https://".to_string(),
        ));
    };
    if protocol != "http" && protocol != "https" {
        return Err(CliError::Usage(
            "git_remote_url must use http or https".to_string(),
        ));
    }
    let Some((host, raw_path)) = rest.split_once('/') else {
        return Err(CliError::Usage(
            "git_remote_url must include a repository path".to_string(),
        ));
    };
    if host.is_empty() || raw_path.is_empty() {
        return Err(CliError::Usage(
            "git_remote_url must include a host and repository path".to_string(),
        ));
    }
    Ok(GitCredentialContext {
        protocol: protocol.to_string(),
        host: host.to_string(),
        path: raw_path.to_string(),
    })
}

fn credential_store_path() -> Result<PathBuf, CliError> {
    let home = std::env::var("HOME")
        .map_err(|_| CliError::Key("HOME is not set; cannot store git credential".to_string()))?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("finite-sites")
        .join("git-credentials"))
}

fn set_private_file_permissions(path: &Path) -> Result<(), CliError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if path.exists() {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(
                |error| CliError::Io(format!("cannot chmod {}: {error}", path.display())),
            )?;
        }
    }
    Ok(())
}

fn ensure_private_parent(path: &Path) -> Result<(), CliError> {
    let parent = path
        .parent()
        .ok_or_else(|| CliError::Io(format!("{} has no parent directory", path.display())))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| CliError::Io(format!("cannot create {}: {error}", parent.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| CliError::Io(format!("cannot chmod {}: {error}", parent.display())))?;
    }
    Ok(())
}

fn run_git_config(args: &[&str]) -> Result<(), CliError> {
    let command_args = git_config_command_args(args);
    let output = Command::new("git")
        .args(&command_args)
        .output()
        .map_err(|error| CliError::Io(format!("cannot run git config: {error}")))?;
    if output.status.success() {
        return Ok(());
    }
    Err(CliError::Io(format!(
        "git config failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

fn git_config_command_args(args: &[&str]) -> Vec<String> {
    let mut command_args = Vec::with_capacity(args.len() + 1);
    command_args.push("config".to_string());
    // Bounded by the fixed git config invocations in this CLI.
    for arg in args {
        command_args.push((*arg).to_string());
    }
    command_args
}

fn configure_git_credential_storage(context: &GitCredentialContext) -> Result<PathBuf, CliError> {
    let store_path = credential_store_path()?;
    ensure_private_parent(&store_path)?;
    let url = format!("{}://{}", context.protocol, context.host);
    let helper_key = format!("credential.{url}.helper");
    let path_key = format!("credential.{url}.useHttpPath");
    let helper_value = format!("store --file {}", store_path.display());
    run_git_config(&["--global", "--replace-all", &helper_key, &helper_value])?;
    run_git_config(&["--global", "--replace-all", &path_key, "true"])?;
    Ok(store_path)
}

fn credential_approve_input(
    context: &GitCredentialContext,
    username: &str,
    password: &str,
) -> String {
    format!(
        "protocol={}\nhost={}\npath={}\nusername={}\npassword={}\n\n",
        context.protocol, context.host, context.path, username, password
    )
}

fn credential_fill_input(context: &GitCredentialContext) -> String {
    format!(
        "protocol={}\nhost={}\npath={}\n\n",
        context.protocol, context.host, context.path
    )
}

fn run_git_credential(command: &str, input: &str) -> Result<String, CliError> {
    let mut child = Command::new("git")
        .args(["credential", command])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| CliError::Io(format!("cannot run git credential {command}: {error}")))?;
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| CliError::Io("cannot open git credential stdin".to_string()))?;
        stdin
            .write_all(input.as_bytes())
            .map_err(|error| CliError::Io(format!("cannot write git credential input: {error}")))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| CliError::Io(format!("git credential {command} failed: {error}")))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }
    Err(CliError::Io(format!(
        "git credential {command} failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

fn credential_output_value(output: &str, key: &str) -> Option<String> {
    // Bounded by git credential's short key-value output.
    for line in output.lines() {
        if let Some(value) = line.strip_prefix(key)
            && let Some(value) = value.strip_prefix('=')
        {
            return Some(value.to_string());
        }
    }
    None
}

fn store_git_credential(response: &GitAuthResponse) -> Result<(), CliError> {
    let context = git_credential_context(&response.git_remote_url)?;
    let store_path = configure_git_credential_storage(&context)?;
    run_git_credential(
        "approve",
        &credential_approve_input(&context, &response.username, &response.password),
    )?;
    set_private_file_permissions(&store_path)?;
    let filled = run_git_credential("fill", &credential_fill_input(&context))?;
    let filled_username = credential_output_value(&filled, "username")
        .ok_or_else(|| CliError::Io("git credential helper did not return username".to_string()))?;
    let filled_password = credential_output_value(&filled, "password")
        .ok_or_else(|| CliError::Io("git credential helper did not return password".to_string()))?;
    if filled_username != response.username || filled_password != response.password {
        return Err(CliError::Io(
            "git credential helper returned a different credential".to_string(),
        ));
    }
    Ok(())
}

fn whoami() -> Result<(), CliError> {
    let identity = keys::load_or_create_identity()?;
    let display =
        npub::encode_npub(&identity.pubkey).map_err(|error| CliError::Key(error.to_string()))?;
    println!("npub:   {display}");
    println!("pubkey: {}", identity.pubkey);
    println!("file:   {}", keys::identity_path()?.display());
    println!();
    println!(
        "ask a finite operator to grant this npub publishing access before creating Project Outputs"
    );
    Ok(())
}

fn email_login(args: &[String]) -> Result<(), CliError> {
    if help_requested(args) {
        return print_help(email_login_help());
    }
    let [email] = args else {
        return Err(CliError::Usage(email_login_help().to_string()));
    };
    let client = api::Client::from_env();
    let response = client.request_email_login(email)?;
    println!("sent email login for {}", response.email);
    println!("run the fsite email-redeem command from the email to verify this machine");
    Ok(())
}

fn email_redeem(args: &[String]) -> Result<(), CliError> {
    if help_requested(args) {
        return print_help(email_redeem_help());
    }
    let [email, token] = args else {
        return Err(CliError::Usage(email_redeem_help().to_string()));
    };
    let key = keys::load_or_create_email_key(email)?;
    let client = api::Client::from_env();
    let response = client.redeem_email_login(&key, email, token)?;
    println!("verified {} for publishing", response.email);
    Ok(())
}

fn status(args: &[String]) -> Result<(), CliError> {
    if help_requested(args) {
        return print_help(status_help());
    }
    let [name] = args else {
        return Err(CliError::Usage(status_help().to_string()));
    };
    let identity = keys::load_or_create_identity()?;
    let client = api::Client::from_env();
    let summary = client.site_status(&identity, name)?;
    print_summary(&summary);
    Ok(())
}

fn list() -> Result<(), CliError> {
    let identity = keys::load_or_create_identity()?;
    let client = api::Client::from_env();
    let response = client.list_sites(&identity)?;
    if response.sites.is_empty() {
        println!(
            "no sites yet; create a Project Output with `fsite project apply --json project.json --dry-run --output json`"
        );
        return Ok(());
    }
    // Bounded by MAX_SITES_PER_OWNER.
    for site in &response.sites {
        let version = site
            .active_version
            .map(|v| format!("v{v}"))
            .unwrap_or_else(|| "unpublished".to_string());
        println!(
            "{:<24} {:<8} {:<12} {:<8} {}",
            site.name, site.kind, site.visibility, version, site.url
        );
    }
    Ok(())
}

fn share(args: &[String]) -> Result<(), CliError> {
    if help_requested(args) {
        return print_help(share_help());
    }
    let Some((name, flags)) = args.split_first() else {
        return Err(CliError::Usage(share_help().to_string()));
    };

    let mut visibility: Option<String> = None;
    let mut confirm_public = false;
    let mut send_invites = false;
    let mut add_emails: Vec<String> = Vec::new();
    let mut remove_emails: Vec<String> = Vec::new();
    let mut index: usize = 0;
    // Bounded by argv length.
    while index < flags.len() {
        match flags[index].as_str() {
            "--public" => {
                visibility = Some("public".to_string());
                index += 1;
            }
            "--shared" => {
                visibility = Some("shared".to_string());
                index += 1;
            }
            "--private" => {
                visibility = Some("private".to_string());
                index += 1;
            }
            "--yes-public" => {
                confirm_public = true;
                index += 1;
            }
            "--send-invite" => {
                send_invites = true;
                index += 1;
            }
            "--add-email" | "--remove-email" => {
                let value = flags
                    .get(index + 1)
                    .ok_or_else(|| CliError::Usage(format!("{} needs a value", flags[index])))?;
                if flags[index] == "--add-email" {
                    add_emails.push(value.clone());
                } else {
                    remove_emails.push(value.clone());
                }
                index += 2;
            }
            other => {
                return Err(CliError::Usage(format!("unknown flag `{other}`")));
            }
        }
    }

    if visibility.as_deref() == Some("public") && !confirm_public {
        return Err(CliError::Usage(
            "making a site public exposes it to the whole internet. \
             Confirm with the user first, then re-run with --yes-public"
                .to_string(),
        ));
    }
    if add_emails.len() + remove_emails.len() > MAX_EMAILS_PER_SHARING_REQUEST as usize {
        return Err(CliError::Usage(format!(
            "at most {MAX_EMAILS_PER_SHARING_REQUEST} email changes per command"
        )));
    }
    if send_invites && add_emails.is_empty() {
        return Err(CliError::Usage(
            "--send-invite requires at least one --add-email".to_string(),
        ));
    }
    if visibility.is_none() && add_emails.is_empty() && remove_emails.is_empty() {
        return Err(CliError::Usage(
            "nothing to change; pass --shared/--private/--public and/or email flags".to_string(),
        ));
    }
    // Adding emails to a site implies shared visibility unless stated.
    if visibility.is_none() && !add_emails.is_empty() {
        visibility = Some("shared".to_string());
    }
    if send_invites && visibility.as_deref() != Some("shared") {
        return Err(CliError::Usage(
            "--send-invite requires shared visibility".to_string(),
        ));
    }

    let client = api::Client::from_env();
    let request = SharingRequest {
        visibility,
        confirm_public,
        add_emails,
        remove_emails,
    };
    let identity = keys::load_or_create_identity()?;
    let response = client.set_sharing(&identity, name, &request, send_invites)?;
    println!("visibility: {}", response.visibility);
    if response.shared_emails.is_empty() {
        println!("shared with: nobody");
    } else {
        println!("shared with: {}", response.shared_emails.join(", "));
    }
    if !response.invited_emails.is_empty() {
        println!("invited: {}", response.invited_emails.join(", "));
    }
    Ok(())
}

fn print_summary(summary: &finitesites_proto::dto::SiteSummary) {
    println!("name:       {}", summary.name);
    println!("url:        {}", summary.url);
    println!("status:     {}", summary.status);
    println!("kind:       {}", summary.kind);
    println!("visibility: {}", summary.visibility);
    match summary.active_version {
        Some(version) => println!("version:    v{version}"),
        None => println!("version:    unpublished"),
    }
    if !summary.shared_emails.is_empty() {
        println!("shared:     {}", summary.shared_emails.join(", "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn help_is_read_only_for_agent_probe_paths() {
        let commands = [
            &["whoami", "--help"][..],
            &["email-login", "--help"],
            &["email-redeem", "--help"],
            &["describe", "--help"],
            &["project", "--help"],
            &["project", "apply", "--help"],
            &["project", "collaborator", "--help"],
            &["project", "collaborator", "remove", "--help"],
            &["auth", "--help"],
            &["auth", "git", "--help"],
            &["status", "--help"],
            &["list", "--help"],
            &["share", "--help"],
        ];
        // Bounded by the explicit command table above.
        for command in commands {
            run(&args(command)).unwrap();
        }
    }

    #[test]
    fn top_level_usage_is_project_first() {
        let text = usage();
        assert!(text.contains("fsite project apply"));
        assert!(text.contains("fsite auth git"));
        assert!(text.contains("fsite share"));
        assert!(!text.contains("fsite claim"));
        assert!(!text.contains("fsite publish "));
        assert!(!text.contains("fsite publish-app"));
        assert!(!text.contains("fsite editors"));
        assert!(!text.contains("fsite source"));
    }

    #[test]
    fn no_arg_commands_reject_extra_non_help_arguments() {
        assert!(matches!(
            run(&args(&["whoami", "extra"])),
            Err(CliError::Usage(_))
        ));
        assert!(matches!(
            run(&args(&["list", "extra"])),
            Err(CliError::Usage(_))
        ));
    }

    #[test]
    fn auth_git_parses_native_and_external_store_modes() {
        let native =
            parse_auth_git_args(&args(&["finite-curriculum", "--store", "--output", "json"]))
                .unwrap();
        assert_eq!(
            native,
            AuthGitOptions {
                project: "finite-curriculum".to_string(),
                email: None,
                output_json: true,
                store: true,
            }
        );

        let external =
            parse_auth_git_args(&args(&["finite-curriculum", "--email", "paul@finite.vip"]))
                .unwrap();
        assert_eq!(external.email.as_deref(), Some("paul@finite.vip"));
        assert!(!external.store);
    }

    #[test]
    fn git_credential_context_is_path_aware() {
        let context =
            git_credential_context("https://git.finite.chat/finite-curriculum.git").unwrap();
        assert_eq!(context.protocol, "https");
        assert_eq!(context.host, "git.finite.chat");
        assert_eq!(context.path, "finite-curriculum.git");
        let input = credential_approve_input(&context, "user", "secret");
        assert!(input.contains("path=finite-curriculum.git\n"));
        assert!(input.contains("username=user\n"));
        assert!(input.contains("password=secret\n"));
    }

    #[test]
    fn git_config_command_uses_config_subcommand() {
        assert_eq!(
            git_config_command_args(&[
                "--global",
                "credential.https://git.finite.chat.useHttpPath",
                "true"
            ]),
            vec![
                "config".to_string(),
                "--global".to_string(),
                "credential.https://git.finite.chat.useHttpPath".to_string(),
                "true".to_string(),
            ]
        );
    }

    #[test]
    fn share_invite_requires_added_email_and_shared_visibility() {
        assert!(matches!(
            run(&args(&["share", "demo", "--send-invite"])),
            Err(CliError::Usage(message)) if message.contains("--send-invite requires at least one --add-email")
        ));
        assert!(matches!(
            run(&args(&[
                "share",
                "demo",
                "--private",
                "--send-invite",
                "--add-email",
                "friend@example.com"
            ])),
            Err(CliError::Usage(message)) if message.contains("--send-invite requires shared visibility")
        ));
    }

    #[test]
    fn project_example_fixture_matches_committed_config() {
        let request: ProjectApplyRequest = serde_json::from_str(include_str!(
            "../../../examples/project-applies/finitechat-native-mockup.json"
        ))
        .unwrap();
        let config = parse_project_config_toml(include_str!(
            "../../../examples/finitechat-native-mockup/finite.toml"
        ))
        .unwrap();

        assert_eq!(request.config, config);
        assert_eq!(request.config.project.slug, "finitechat-native");
        assert_eq!(request.collaborators.len(), 1);
        assert_eq!(request.collaborators[0].email, "skyler@example.com");
        assert_eq!(request.collaborators[0].role, "editor");
    }
}
