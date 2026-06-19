//! `fsite` — the agent-facing CLI for Finite Sites.
//!
//! Commands hide nostr, keys, manifests, and blob mechanics; the agent only
//! sees names, paths, emails, and URLs:
//!
//!   fsite whoami
//!   fsite claim NAME
//!   fsite publish NAME PATH [--spa]
//!   fsite status NAME
//!   fsite list
//!   fsite share NAME [--shared|--private] [--public --yes-public]
//!                    [--add-email E]... [--remove-email E]...
//!
//! Server address comes from FINITE_SITES_API (default http://127.0.0.1:8787).

mod api;
mod bundle;
mod keys;
mod source;
mod walk;

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use sha2::Digest as _;
use thiserror::Error;

use finitesites_proto::dto::{
    EditorsRequest, GitAuthRequest, ProjectApplyRequest, ProjectCollaboratorRemoveRequest,
    SharingRequest, SourceSnapshotRequest,
};
use finitesites_proto::limits::MAX_EMAILS_PER_SHARING_REQUEST;
use finitesites_proto::project_config::parse_project_config_toml;
use finitesites_proto::{hex, npub};

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
        "claim" => claim(&args[1..]),
        "publish" => publish(&args[1..]),
        "publish-app" => publish_app(&args[1..]),
        "status" => status(&args[1..]),
        "list" => no_args_or_help(&args[1..], "fsite list", list_help(), list),
        "share" => share(&args[1..]),
        "editors" => editors(&args[1..]),
        "source" => source_command(&args[1..]),
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
     fsite email-redeem EMAIL TOKEN\n  fsite claim NAME [--owner-email EMAIL]\n  \
     fsite describe [workflow NAME] [--output json]\n  \
     fsite project apply --json FILE|- [--dry-run] [--output json] [--config finite.toml]\n  \
     fsite project collaborator remove PROJECT --email EMAIL [--output json]\n  \
     fsite auth git PROJECT --email EMAIL [--output json]\n  \
     fsite publish NAME PATH [--spa] [--source PATH] [--owner-email EMAIL] [--email EMAIL]\n  \
     fsite publish-app NAME PATH --start \"CMD\"\n  \
     fsite status NAME\n  fsite list\n  fsite share NAME [--shared|--private] \
     [--public --yes-public] [--add-email EMAIL]... [--remove-email EMAIL]...\n  \
     fsite editors NAME [--email OWNER_EMAIL] [--add-email EMAIL]... [--remove-email EMAIL]...\n  \
     fsite source pull NAME PATH [--email EMAIL]"
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
    "usage:\n  fsite project apply --json FILE|- [--dry-run] [--output json] [--config finite.toml]\n  fsite project collaborator remove PROJECT --email EMAIL [--output json]\n\nCreate/update Project Repositories and manage Project Collaborators. See: fsite describe workflow project-config --output json"
}

fn project_apply_help() -> &'static str {
    "usage: fsite project apply --json FILE|- [--dry-run] [--output json] [--config finite.toml]\n\nReads Project apply JSON, validates it, optionally writes finite.toml, and creates/updates Project Outputs. Use --dry-run before mutating. Use --output json for agent workflows."
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

fn claim_help() -> &'static str {
    "usage: fsite claim NAME [--owner-email EMAIL]\n\nClaim a Finite Site name with the local User Key and create a workspace Site Key."
}

fn publish_help() -> &'static str {
    "usage: fsite publish NAME PATH [--spa] [--source PATH] [--owner-email EMAIL] [--email EMAIL]\n\nLegacy site-first static publish. For Project Repositories prefer fsite project apply plus git push."
}

fn publish_app_help() -> &'static str {
    "usage: fsite publish-app NAME PATH --start \"CMD\"\n\nPublish a tier-2 server app bundle. The start command must listen on $PORT."
}

fn status_help() -> &'static str {
    "usage: fsite status NAME\n\nPrint registry status for one Finite Site."
}

fn list_help() -> &'static str {
    "usage: fsite list\n\nList Finite Sites owned by the local User Key."
}

fn share_help() -> &'static str {
    "usage: fsite share NAME [--shared|--private] [--public --yes-public] [--add-email EMAIL]... [--remove-email EMAIL]...\n\nChange output Visibility or email Share rows. Public sharing requires explicit human confirmation and --yes-public."
}

fn editors_help() -> &'static str {
    "usage: fsite editors NAME [--email OWNER_EMAIL] [--add-email EMAIL]... [--remove-email EMAIL]...\n\nLegacy site-first editor management. Project Repository collaboration uses fsite project apply collaborators."
}

fn source_help() -> &'static str {
    "usage: fsite source pull NAME PATH [--email EMAIL]\n\nPull a legacy Source Snapshot. Project-backed sites should use fsite auth git and git clone."
}

fn source_pull_help() -> &'static str {
    "usage: fsite source pull NAME PATH [--email EMAIL]\n\nExtract the active Version's Source Snapshot into an empty target directory."
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
                "usage": "fsite project apply --json FILE|- [--dry-run] [--output json] [--config finite.toml]"
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
            },
            {
                "name": "publish",
                "summary": "Legacy site-first publish with optional source snapshot.",
                "usage": "fsite publish NAME PATH [--spa] [--source PATH] [--email EMAIL]"
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
                "Run fsite project apply --json project.json --output json.",
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
                "Run fsite project apply --json project.json --output json after confirming the plan."
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
    let response = client.apply_project(&identity, &request)?;
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
        if response.dry_run {
            println!("dry-run: no server state changed and finite.toml was not written");
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
    println!("ask a finite operator to grant this npub publishing access before claiming sites");
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

fn claim(args: &[String]) -> Result<(), CliError> {
    if help_requested(args) {
        return print_help(claim_help());
    }
    let mut positionals: Vec<&String> = Vec::new();
    let mut owner_email: Option<String> = None;
    let mut index: usize = 0;
    // Bounded by argv length.
    while index < args.len() {
        match args[index].as_str() {
            "--owner-email" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| CliError::Usage("--owner-email needs a value".to_string()))?;
                owner_email = Some(value.clone());
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
    let [name] = positionals.as_slice() else {
        return Err(CliError::Usage(claim_help().to_string()));
    };
    let identity = keys::load_or_create_identity()?;
    let site_key = keys::load_or_create_site_key(name)?;
    let client = api::Client::from_env();
    let response = client.claim(&identity, name, &site_key.pubkey, owner_email.as_deref())?;
    if response.already_claimed {
        println!("{} was already yours", response.name);
    } else {
        println!("claimed {}", response.name);
    }
    println!("url:    {}", response.url);
    println!(
        "status: {} (publish with: fsite publish {} PATH)",
        response.status, response.name
    );
    if let Some(email) = response.owner_email {
        println!("owner:  {email}");
    }
    Ok(())
}

fn publish(args: &[String]) -> Result<(), CliError> {
    if help_requested(args) {
        return print_help(publish_help());
    }
    let mut spa = false;
    let mut actor_email: Option<String> = None;
    let mut owner_email: Option<String> = None;
    let mut source_path: Option<String> = None;
    let mut positionals: Vec<&String> = Vec::new();
    let mut index: usize = 0;
    // Bounded by argv length.
    while index < args.len() {
        match args[index].as_str() {
            "--spa" => {
                spa = true;
                index += 1;
            }
            "--email" | "--owner-email" | "--source" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| CliError::Usage(format!("{} needs a value", args[index])))?;
                match args[index].as_str() {
                    "--email" => actor_email = Some(value.clone()),
                    "--owner-email" => owner_email = Some(value.clone()),
                    "--source" => source_path = Some(value.clone()),
                    _ => unreachable!(),
                }
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
    let [name, path] = positionals.as_slice() else {
        return Err(CliError::Usage(publish_help().to_string()));
    };
    if owner_email.is_some() && actor_email.is_some() {
        return Err(CliError::Usage(
            "--owner-email requires the site key; do not combine it with --email".to_string(),
        ));
    }
    let client = api::Client::from_env();
    if let Some(email) = owner_email.as_deref() {
        ensure_claimed_with_owner_email(&client, name, email)?;
    }
    let actor_key = match actor_email.as_deref() {
        Some(email) => keys::load_or_create_email_key(email)?,
        None => keys::load_site_key(name)?,
    };
    let outcome = walk::build_manifest(&PathBuf::from(path))?;
    let source_bytes = match source_path.as_deref() {
        Some(path) => {
            eprintln!("building source snapshot from {path} ...");
            Some(source::build_source_snapshot(&PathBuf::from(path))?)
        }
        None => None,
    };
    let source_request = source_bytes.as_ref().map(|bytes| SourceSnapshotRequest {
        sha256: hex::encode(&sha2::Sha256::digest(bytes)),
        size: bytes.len() as u64,
    });
    if outcome.skipped_hidden > 0 {
        eprintln!(
            "skipped {} hidden file(s)/folder(s)",
            outcome.skipped_hidden
        );
    }

    let begun = client.begin_publish(
        &actor_key,
        name,
        &outcome.manifest,
        spa,
        actor_email.as_deref(),
        source_request.as_ref(),
    )?;
    let total = outcome.manifest.files.len();
    let to_upload = begun.missing.len();
    eprintln!(
        "{total} file(s) in manifest, {to_upload} new blob(s) to upload \
         ({} already on the server)",
        total - to_upload.min(total)
    );

    // Bounded by MAX_MANIFEST_FILES via manifest validation.
    for (index, sha256) in begun.missing.iter().enumerate() {
        if let (Some(request), Some(bytes)) = (source_request.as_ref(), source_bytes.as_ref())
            && sha256 == &request.sha256
        {
            client.upload_blob(&actor_key, &begun.publish_id, sha256, bytes)?;
            eprintln!("uploaded {}/{to_upload} source snapshot", index + 1);
            continue;
        }
        let file_source = outcome
            .sources
            .get(sha256)
            .ok_or_else(|| CliError::Api(format!("server asked for unknown blob {sha256}")))?;
        let bytes = std::fs::read(file_source).map_err(|error| {
            CliError::Io(format!("cannot read {}: {error}", file_source.display()))
        })?;
        client.upload_blob(&actor_key, &begun.publish_id, sha256, &bytes)?;
        eprintln!(
            "uploaded {}/{to_upload} {}",
            index + 1,
            file_source.display()
        );
    }

    let finalized = client.finalize_publish(&actor_key, &begun.publish_id)?;
    println!("published {name} version {}", finalized.version_number);
    println!(
        "{} file(s), {} bytes",
        finalized.path_count, finalized.total_bytes
    );
    println!("url: {}", finalized.url);
    if finalized.source.is_some() {
        println!("source: attached");
    }
    println!();
    println!("the site is PRIVATE by default; use `fsite share {name} ...` to share it");
    Ok(())
}

fn ensure_claimed_with_owner_email(
    client: &api::Client,
    name: &str,
    owner_email: &str,
) -> Result<(), CliError> {
    let identity = keys::load_or_create_identity()?;
    let site_key = keys::load_or_create_site_key(name)?;
    client.claim(&identity, name, &site_key.pubkey, Some(owner_email))?;
    client.set_owner_email(&site_key, name, owner_email)?;
    Ok(())
}

/// Tier 2: bundle a directory and publish it as a server app. The start
/// command runs in the platform sandbox and must listen on `$PORT`.
fn publish_app(args: &[String]) -> Result<(), CliError> {
    if help_requested(args) {
        return print_help(publish_app_help());
    }
    let mut positionals: Vec<&String> = Vec::new();
    let mut start: Option<String> = None;
    let mut index: usize = 0;
    // Bounded by argv length.
    while index < args.len() {
        match args[index].as_str() {
            "--start" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| CliError::Usage("--start needs a command".to_string()))?;
                start = Some(value.clone());
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
    let ([name, path], Some(start)) = (positionals.as_slice(), start) else {
        return Err(CliError::Usage(publish_app_help().to_string()));
    };

    let site_key = keys::load_site_key(name)?;
    eprintln!("bundling {path} ...");
    let bundle_bytes = bundle::build_bundle(&PathBuf::from(path))?;
    let sha256 = {
        use sha2::Digest as _;
        finitesites_proto::hex::encode(&sha2::Sha256::digest(&bundle_bytes))
    };
    let manifest = finitesites_proto::PublishManifest {
        files: vec![finitesites_proto::ManifestFile {
            path: finitesites_proto::manifest::APP_BUNDLE_PATH.to_string(),
            sha256: sha256.clone(),
            size: bundle_bytes.len() as u64,
        }],
    };

    let client = api::Client::from_env();
    let begun = client.begin_publish_app(&site_key, name, &manifest, &start)?;
    if begun.missing.is_empty() {
        eprintln!("bundle already on the server (unchanged)");
    } else {
        eprintln!(
            "uploading bundle ({} MiB) ...",
            bundle_bytes.len() / (1024 * 1024)
        );
        client.upload_blob(&site_key, &begun.publish_id, &sha256, &bundle_bytes)?;
    }
    let finalized = client.finalize_publish(&site_key, &begun.publish_id)?;
    println!("published app {name} version {}", finalized.version_number);
    println!("url: {}", finalized.url);
    println!();
    println!("the app is starting; it must listen on $PORT to serve");
    println!("the site is PRIVATE by default; use `fsite share {name} ...` to share it");
    Ok(())
}

fn status(args: &[String]) -> Result<(), CliError> {
    if help_requested(args) {
        return print_help(status_help());
    }
    let [name] = args else {
        return Err(CliError::Usage(status_help().to_string()));
    };
    let key = actor_key_for(name)?;
    let client = api::Client::from_env();
    let summary = client.site_status(&key, name)?;
    print_summary(&summary);
    Ok(())
}

fn list() -> Result<(), CliError> {
    let identity = keys::load_or_create_identity()?;
    let client = api::Client::from_env();
    let response = client.list_sites(&identity)?;
    if response.sites.is_empty() {
        println!("no sites yet; claim one with `fsite claim NAME`");
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
    if visibility.is_none() && add_emails.is_empty() && remove_emails.is_empty() {
        return Err(CliError::Usage(
            "nothing to change; pass --shared/--private/--public and/or email flags".to_string(),
        ));
    }
    // Adding emails to a site implies shared visibility unless stated.
    if visibility.is_none() && !add_emails.is_empty() {
        visibility = Some("shared".to_string());
    }

    let key = actor_key_for(name)?;
    let client = api::Client::from_env();
    let response = client.set_sharing(
        &key,
        name,
        &SharingRequest {
            visibility,
            confirm_public,
            add_emails,
            remove_emails,
        },
    )?;
    println!("visibility: {}", response.visibility);
    if response.shared_emails.is_empty() {
        println!("shared with: nobody");
    } else {
        println!("shared with: {}", response.shared_emails.join(", "));
    }
    Ok(())
}

fn editors(args: &[String]) -> Result<(), CliError> {
    if help_requested(args) {
        return print_help(editors_help());
    }
    let Some((name, flags)) = args.split_first() else {
        return Err(CliError::Usage(editors_help().to_string()));
    };
    let mut add_emails: Vec<String> = Vec::new();
    let mut remove_emails: Vec<String> = Vec::new();
    let mut actor_email: Option<String> = None;
    let mut index: usize = 0;
    // Bounded by argv length.
    while index < flags.len() {
        match flags[index].as_str() {
            "--email" => {
                let value = flags
                    .get(index + 1)
                    .ok_or_else(|| CliError::Usage("--email needs a value".to_string()))?;
                actor_email = Some(value.clone());
                index += 2;
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
            other => return Err(CliError::Usage(format!("unknown flag `{other}`"))),
        }
    }
    if add_emails.len() + remove_emails.len() > MAX_EMAILS_PER_SHARING_REQUEST as usize {
        return Err(CliError::Usage(format!(
            "at most {MAX_EMAILS_PER_SHARING_REQUEST} email changes per command"
        )));
    }
    let key = match actor_email.as_deref() {
        Some(email) => keys::load_or_create_email_key(email)?,
        None => actor_key_for(name)?,
    };
    let client = api::Client::from_env();
    let response = if add_emails.is_empty() && remove_emails.is_empty() {
        client.list_editors(&key, name, actor_email.as_deref())?
    } else {
        client.update_editors(
            &key,
            name,
            &EditorsRequest {
                actor_email: actor_email.clone(),
                add_emails,
                remove_emails,
            },
        )?
    };
    match response.owner_email {
        Some(email) => println!("owner:   {email}"),
        None => println!("owner:   unset"),
    }
    if response.editor_emails.is_empty() {
        println!("editors: nobody");
    } else {
        println!("editors: {}", response.editor_emails.join(", "));
    }
    Ok(())
}

fn source_command(args: &[String]) -> Result<(), CliError> {
    let Some((subcommand, rest)) = args.split_first() else {
        return Err(CliError::Usage(source_help().to_string()));
    };
    match subcommand.as_str() {
        value if is_help_arg(value) => print_help(source_help()),
        "pull" => source_pull(rest),
        other => Err(CliError::Usage(format!("unknown source command `{other}`"))),
    }
}

fn source_pull(args: &[String]) -> Result<(), CliError> {
    if help_requested(args) {
        return print_help(source_pull_help());
    }
    let mut positionals: Vec<&String> = Vec::new();
    let mut actor_email: Option<String> = None;
    let mut index: usize = 0;
    // Bounded by argv length.
    while index < args.len() {
        match args[index].as_str() {
            "--email" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| CliError::Usage("--email needs a value".to_string()))?;
                actor_email = Some(value.clone());
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
    let [name, target] = positionals.as_slice() else {
        return Err(CliError::Usage(source_pull_help().to_string()));
    };
    let key = match actor_email.as_deref() {
        Some(email) => keys::load_or_create_email_key(email)?,
        None => actor_key_for(name)?,
    };
    let client = api::Client::from_env();
    let bytes = client.source_snapshot(&key, name, actor_email.as_deref())?;
    source::extract_source_snapshot(&bytes, &PathBuf::from(target))?;
    println!("pulled source for {name} into {target}");
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
    if let Some(email) = &summary.owner_email {
        println!("owner:      {email}");
    }
    if !summary.shared_emails.is_empty() {
        println!("shared:     {}", summary.shared_emails.join(", "));
    }
    if !summary.editor_emails.is_empty() {
        println!("editors:    {}", summary.editor_emails.join(", "));
    }
    if let Some(source) = &summary.source {
        println!("source:     v{} {}", source.version_number, source.sha256);
    }
}

/// Prefer the site key (workspace-scoped) and fall back to the identity for
/// commands that accept either signer.
fn actor_key_for(name: &str) -> Result<keys::KeyFile, CliError> {
    if keys::site_key_path(name).exists() {
        keys::load_site_key(name)
    } else {
        keys::load_or_create_identity()
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
            &["claim", "--help"],
            &["publish", "--help"],
            &["publish-app", "--help"],
            &["status", "--help"],
            &["list", "--help"],
            &["share", "--help"],
            &["editors", "--help"],
            &["source", "--help"],
            &["source", "pull", "--help"],
        ];
        // Bounded by the explicit command table above.
        for command in commands {
            run(&args(command)).unwrap();
        }
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
