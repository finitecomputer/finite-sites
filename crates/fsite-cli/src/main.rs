//! `fsite` — the agent-facing CLI for Finite Sites.
//!
//! Commands hide nostr, keys, manifests, and blob mechanics; the agent only
//! sees names, paths, emails, and URLs:
//!
//!   fsite whoami
//!   fsite project apply --json project.json --dry-run --output json
//!   fsite auth git PROJECT --email EMAIL --output json
//!   fsite status NAME
//!   fsite list
//!   fsite share NAME [--shared|--private] [--public --yes-public]
//!                    [--add-email E]... [--remove-email E]...
//!
//! Server address comes from FINITE_SITES_API (default https://api.finite.chat).

mod api;
mod keys;

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use thiserror::Error;

use finitesites_proto::dto::{
    GitAuthRequest, ProjectApplyRequest, ProjectCollaboratorRemoveRequest, SharingRequest,
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
     fsite auth git PROJECT --email EMAIL [--output json]\n  \
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
    "usage: fsite auth git PROJECT --email EMAIL [--output json]\n\nMint scoped HTTPS Git Credentials after email verification."
}

fn auth_git_help() -> &'static str {
    "usage: fsite auth git PROJECT --email EMAIL [--output json]\n\nReturns git_remote_url, username, and password for standard git clone/push against one Project Repository."
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
                "summary": "Mint a scoped HTTPS Git Credential after email verification.",
                "usage": "fsite auth git PROJECT --email EMAIL [--output json]"
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
                "Run fsite email-login EDITOR_EMAIL if this machine is not verified.",
                "Run fsite email-redeem EDITOR_EMAIL TOKEN_FROM_EMAIL.",
                "Run fsite auth git PROJECT --email EDITOR_EMAIL --output json.",
                "Use the returned git_remote_url, username, and password with standard git.",
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
    let mut positionals: Vec<&String> = Vec::new();
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
            _ => {
                positionals.push(&args[index]);
                index += 1;
            }
        }
    }
    let [project] = positionals.as_slice() else {
        return Err(CliError::Usage(
            "usage: fsite auth git PROJECT --email EMAIL [--output json]".to_string(),
        ));
    };
    let email = email.ok_or_else(|| CliError::Usage("--email is required".to_string()))?;
    let key = keys::load_or_create_email_key(&email)?;
    let client = api::Client::from_env();
    let response = client.auth_git(&key, project, &GitAuthRequest { email })?;
    if output_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&response).expect("response serializes")
        );
    } else {
        println!("git:      {}", response.git_remote_url);
        println!("username: {}", response.username);
        println!("password: {}", response.password);
        println!("use this credential with standard git HTTPS for this project only");
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
