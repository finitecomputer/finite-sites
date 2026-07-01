//! `fsite` — the agent-facing CLI for Finite Sites.
//!
//! Commands hide nostr, keys, manifests, and blob mechanics; the agent only
//! sees names, paths, emails, and URLs:
//!
//!   fsite whoami
//!   fsite describe workflow publish-static-site --output json
//!   fsite project init --config finite.toml --dry-run --output json
//!   fsite project grant PROJECT --email EDITOR_EMAIL --send-invite --output json
//!   fsite auth git PROJECT [--email EMAIL] [--store] [--output json]
//!   fsite project status PROJECT --output json
//!   fsite project list --output json
//!   fsite view URL_OR_NAME --output json
//!
//! Server address comes from FINITE_SITES_API (default https://api.finite.chat).

mod api;
mod keys;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use thiserror::Error;

use finitesites_proto::dto::{
    GitAuthRequest, GitAuthResponse, ProjectGrantRequest, ProjectInitRequest, ProjectRevokeRequest,
};
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
        "describe" => describe(&args[1..]),
        "project" => project_command(&args[1..]),
        "auth" => auth_command(&args[1..]),
        "view" => view(&args[1..]),
        "email-login" | "email-redeem" | "email-claim" | "status" | "list" | "share" | "claim"
        | "publish" | "publish-app" | "source" => Err(CliError::Usage(
            removed_site_first_command_help(command.as_str()),
        )),
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
    "Finite Sites shares the whole source tree through a Project Repository \
     for authorized collaborators. The Project's finite.toml selects which \
     committed path becomes the served website; source outside that path is \
     cloneable by collaborators but not served as ordinary web assets.\n\n\
     Agent quick start for a static site:\n  \
     fsite describe workflow publish-static-site --output json\n  \
     fsite project init --config finite.toml --dry-run --output json\n  \
     fsite project init --config finite.toml --output json\n  \
     fsite auth git PROJECT --store --output json\n  \
     git clone https://git.finite.chat/PROJECT.git\n  \
     # commit finite.toml plus deploy bytes, then push the Deploy Branch\n\n\
     Commands:\n  fsite whoami\n  \
     fsite describe [workflow NAME] [--output json]\n  \
     fsite project init --config finite.toml [--dry-run] [--output json]\n  \
     fsite project grant PROJECT --email EMAIL [--role editor] [--send-invite] [--output json]\n  \
     fsite project revoke PROJECT --email EMAIL [--output json]\n  \
     fsite project status PROJECT [--output json]\n  \
     fsite project list [--output json]\n  \
     fsite auth login EMAIL\n  \
     fsite auth redeem EMAIL TOKEN\n  \
     fsite auth git PROJECT [--email EMAIL] [--store] [--output json]\n  \
     fsite view URL_OR_NAME [--output json]"
        .to_string()
}

fn removed_site_first_command_help(command: &str) -> String {
    format!(
        "`fsite {command}` is not part of the current Project Repository model.\n\n\
         Use the explicit primitives instead:\n  \
         fsite project init --config finite.toml --dry-run --output json\n  \
         fsite project init --config finite.toml --output json\n  \
         fsite project grant PROJECT --email EDITOR_EMAIL --send-invite --output json\n  \
         fsite auth git PROJECT --store --output json\n  \
         git clone https://git.finite.chat/PROJECT.git\n  \
         # edit, commit, and push the configured Deploy Branch"
    )
}

fn whoami_help() -> &'static str {
    "usage: fsite whoami\n\nPrint the local User Key npub and key file path. Creates the identity if missing."
}

fn describe_help() -> &'static str {
    "usage: fsite describe [workflow NAME] [--output json]\n\nMachine-readable command and workflow discovery. Workflows: project-config, publish-static-site, edit-shared-project, grant-collaborator, revoke-collaborator."
}

fn project_help() -> &'static str {
    "usage:\n  fsite project init --config finite.toml [--dry-run] [--output json]\n  fsite project grant PROJECT --email EMAIL [--role editor] [--send-invite] [--output json]\n  fsite project revoke PROJECT --email EMAIL [--output json]\n  fsite project status PROJECT [--output json]\n  fsite project list [--output json]\n\nProject is the source primitive: init creates the Project Repository and declared outputs; git edits and publishes content; grant/revoke manage Project edit access."
}

fn project_init_help() -> &'static str {
    "usage: fsite project init --config finite.toml [--dry-run] [--output json]\n\nInitialize one Project Repository from finite.toml. This reserves the Project Slug and declared Project Outputs; it does not deploy bytes. Retry is safe only with the same finite.toml. To publish, commit finite.toml plus the selected output path to the Project Repository and push the Deploy Branch."
}

fn project_grant_help() -> &'static str {
    "usage: fsite project grant PROJECT --email EMAIL [--role editor] [--send-invite] [--output json]\n\nGrant Project Repository edit access to an External Principal email. Use --send-invite to email agent-facing auth/git instructions."
}

fn project_revoke_help() -> &'static str {
    "usage: fsite project revoke PROJECT --email EMAIL [--output json]\n\nRemove Project Repository edit access for an External Principal email and revoke active Git Credentials. Safe to replay: removed=false means the collaborator was already inactive or unknown."
}

fn project_status_help() -> &'static str {
    "usage: fsite project status PROJECT [--output json]\n\nShow Project Repository control-plane state: git remote, actor role, declared outputs, output URLs, branch/path, visibility, and active version."
}

fn project_list_help() -> &'static str {
    "usage: fsite project list [--output json]\n\nList Project Repositories this actor owns or may edit."
}

fn auth_help() -> &'static str {
    "usage:\n  fsite auth login EMAIL\n  fsite auth redeem EMAIL TOKEN\n  fsite auth git PROJECT [--email EMAIL] [--store] [--output json]\n\nAuthenticate this machine for Finite Sites. Email auth proves an External Principal; git auth mints a scoped HTTPS Git Credential for one Project Repository."
}

fn auth_git_help() -> &'static str {
    "usage: fsite auth git PROJECT [--email EMAIL] [--store] [--output json]\n\nReturns git_remote_url, username, and password for standard git clone/push against one Project Repository. With --store, configures a path-aware Git credential helper for the Finite Git host, stores the scoped credential, and omits the password from output."
}

fn auth_login_help() -> &'static str {
    "usage: fsite auth login EMAIL\n\nRequest a one-time email verification token for an External Principal."
}

fn auth_redeem_help() -> &'static str {
    "usage: fsite auth redeem EMAIL TOKEN\n\nVerify this machine's Email Key for an External Principal."
}

fn view_help() -> &'static str {
    "usage: fsite view URL_OR_NAME [--output json]\n\nInspect a served Project Output URL or routing name. This is read-only; project editing happens through git after fsite auth git."
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
                "name": "project init",
                "summary": "Initialize a Project Repository and finite.toml-described outputs.",
                "usage": "fsite project init --config finite.toml [--dry-run] [--output json]"
            },
            {
                "name": "project grant",
                "summary": "Grant Project Repository edit access to an External Principal email.",
                "usage": "fsite project grant PROJECT --email EMAIL [--role editor] [--send-invite] [--output json]"
            },
            {
                "name": "project revoke",
                "summary": "Remove Project Repository edit access and revoke active Git Credentials for that Principal.",
                "usage": "fsite project revoke PROJECT --email EMAIL [--output json]"
            },
            {
                "name": "project status",
                "summary": "Show Project Repository, output, and deploy state.",
                "usage": "fsite project status PROJECT [--output json]"
            },
            {
                "name": "project list",
                "summary": "List Project Repositories this actor owns or may edit.",
                "usage": "fsite project list [--output json]"
            },
            {
                "name": "auth login",
                "summary": "Request an email verification token for an External Principal.",
                "usage": "fsite auth login EMAIL"
            },
            {
                "name": "auth redeem",
                "summary": "Verify this machine's Email Key for an External Principal.",
                "usage": "fsite auth redeem EMAIL TOKEN"
            },
            {
                "name": "auth git",
                "summary": "Mint a scoped HTTPS Git Credential for a native Project Collaborator or verified External Principal.",
                "usage": "fsite auth git PROJECT [--email EMAIL] [--store] [--output json]"
            },
            {
                "name": "view",
                "summary": "Inspect a served Project Output URL or routing name without mutating state.",
                "usage": "fsite view URL_OR_NAME [--output json]"
            },
            {
                "name": "describe workflow",
                "summary": "Print machine-readable workflow guidance.",
                "usage": "fsite describe workflow NAME --output json"
            }
        ],
        "workflows": [
            "project-config",
            "publish-static-site",
            "edit-shared-project",
            "grant-collaborator",
            "revoke-collaborator"
        ],
        "start_here": {
            "static_site": "fsite describe workflow publish-static-site --output json",
            "existing_shared_project": "fsite describe workflow edit-shared-project --output json"
        }
    })
}

fn publish_static_site_workflow() -> serde_json::Value {
    serde_json::json!({
        "name": "publish-static-site",
        "mental_model": [
            "A Project Repository is the editable git source of truth; authorized collaborators clone the whole source tree.",
            "A Project Output is what Finite serves to users.",
            "finite.toml selects the committed output path for each Project Output.",
            "For static sites, Finite serves only committed bytes under that configured path as the website.",
            "Source, data, docs, and build logic can live outside the served output path and still be available to collaborators over git.",
            "Finite Sites does not run builds and does not accept direct file uploads in the current model."
        ],
        "steps": [
            "Keep the whole project source tree in the Project Repository.",
            "Put generated static website files in a dedicated output directory such as site/ unless the repository is deploy-only.",
            "Create finite.toml with project.slug, one output with kind=site, site_name, branch=main, path=site, and spa=false unless the app needs SPA fallback.",
            "Run fsite project init --config finite.toml --dry-run --output json and read any validation error.",
            "After human confirmation, run fsite project init --config finite.toml --output json.",
            "Run fsite auth git PROJECT --store --output json using the local native User Key, or add --email EDITOR_EMAIL only when using an External Principal.",
            "Clone the returned git_remote_url.",
            "Keep finite.toml, the selected output path, and any source/data/build files collaborators need in the Project Repository. Only the output path is served as the website.",
            "Run the project build/tests locally if there is a build step.",
            "Commit finite.toml, the selected output path, and the source files that should be shared with collaborators.",
            "Push the configured Deploy Branch. Finite Sites validates committed bytes and creates a Version."
        ],
        "must_not": [
            "Do not look for a direct publish/upload command.",
            "Do not reconstruct source from the rendered website.",
            "Do not set path='.' unless the whole repository is intended to be served.",
            "Do not print Git Credential passwords; prefer --store."
        ],
        "finite_toml_example": "[project]\nslug = \"my-project\"\n\n[outputs.site]\nkind = \"site\"\nsite_name = \"my-project\"\nbranch = \"main\"\npath = \"site\"\nspa = false\n"
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
        "publish-static-site" => publish_static_site_workflow(),
        "edit-shared-project" => serde_json::json!({
            "name": "edit-shared-project",
            "steps": [
                "If you are a native Project Collaborator, run fsite auth git PROJECT --store --output json.",
                "If you are using an External Principal email, run fsite auth login EDITOR_EMAIL if this machine is not verified.",
                "For email auth, run fsite auth redeem EDITOR_EMAIL TOKEN_FROM_EMAIL.",
                "For email auth, run fsite auth git PROJECT --email EDITOR_EMAIL --store --output json.",
                "Clone using the returned git_remote_url; the password is stored in Git's credential helper and is not printed.",
                "Clone, edit source, run the project's tests/build, commit deploy bytes, and push the Deploy Branch."
            ]
        }),
        "grant-collaborator" => serde_json::json!({
            "name": "grant-collaborator",
            "steps": [
                "Use the Project owner identity, not the collaborator email key.",
                "Run fsite project grant PROJECT --email COLLABORATOR_EMAIL --role editor --send-invite --output json.",
                "The collaborator should run fsite auth redeem from the email, then fsite auth git PROJECT --email COLLABORATOR_EMAIL --store --output json."
            ]
        }),
        "revoke-collaborator" => serde_json::json!({
            "name": "revoke-collaborator",
            "steps": [
                "Use the Project owner identity, not the collaborator email key.",
                "Run fsite project revoke PROJECT --email COLLABORATOR_EMAIL --output json.",
                "Check removed and revoked_git_credentials in the JSON response."
            ]
        }),
        other => {
            return Err(CliError::Usage(format!(
                "unknown workflow `{other}` (project-config|publish-static-site|edit-shared-project|grant-collaborator|revoke-collaborator)"
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
        "init" => project_init(rest),
        "grant" => project_grant(rest),
        "revoke" => project_revoke(rest),
        "status" => project_status(rest),
        "list" => project_list(rest),
        "apply" | "collaborator" => Err(CliError::Usage(removed_site_first_command_help(
            &format!("project {subcommand}"),
        ))),
        other => Err(CliError::Usage(format!(
            "unknown project command `{other}`"
        ))),
    }
}

fn project_init(args: &[String]) -> Result<(), CliError> {
    if help_requested(args) {
        return print_help(project_init_help());
    }
    let mut config_path: Option<PathBuf> = None;
    let mut dry_run = false;
    let mut output_json = false;
    let mut index: usize = 0;
    // Bounded by argv length.
    while index < args.len() {
        match args[index].as_str() {
            "--config" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| CliError::Usage("--config needs a path".to_string()))?;
                config_path = Some(PathBuf::from(value));
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
            other => return Err(CliError::Usage(format!("unknown flag `{other}`"))),
        }
    }

    let config_path =
        config_path.ok_or_else(|| CliError::Usage(project_init_help().to_string()))?;
    let config = read_project_config_file(&config_path)?;
    let request = ProjectInitRequest { config, dry_run };
    request
        .config
        .validate()
        .map_err(|error| CliError::Usage(error.to_string()))?;

    let identity = keys::load_or_create_identity()?;
    let client = api::Client::from_env();
    let response = client.init_project(&identity, &request)?;
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
                "output:  {} {} -> {} ({}:{})",
                output.output_id, output.kind, output.site_url, output.branch, output.path
            );
        }
        if response.dry_run {
            println!("dry-run: no server state changed");
        } else {
            println!(
                "next:    fsite auth git {} --store --output json",
                response.slug
            );
            println!(
                "publish: commit {} and push the Deploy Branch",
                config_path.display()
            );
        }
    }
    Ok(())
}

fn read_project_config_file(
    path: &Path,
) -> Result<finitesites_proto::project_config::ProjectConfig, CliError> {
    let existing = std::fs::read_to_string(path)
        .map_err(|error| CliError::Io(format!("cannot read {}: {error}", path.display())))?;
    parse_project_config_toml(&existing)
        .map_err(|error| CliError::Usage(format!("{} is invalid: {error}", path.display())))
}

fn project_grant(args: &[String]) -> Result<(), CliError> {
    if help_requested(args) {
        return print_help(project_grant_help());
    }
    let mut project: Option<String> = None;
    let mut email: Option<String> = None;
    let mut role = "editor".to_string();
    let mut send_invite = false;
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
            "--role" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| CliError::Usage("--role needs a value".to_string()))?;
                role = value.clone();
                index += 2;
            }
            "--send-invite" => {
                send_invite = true;
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
            other if other.starts_with("--") => {
                return Err(CliError::Usage(format!("unknown flag `{other}`")));
            }
            value => {
                if project.is_some() {
                    return Err(CliError::Usage(project_grant_help().to_string()));
                }
                project = Some(value.to_string());
                index += 1;
            }
        }
    }
    let project = project.ok_or_else(|| CliError::Usage(project_grant_help().to_string()))?;
    let email = email.ok_or_else(|| CliError::Usage(project_grant_help().to_string()))?;
    let identity = keys::load_or_create_identity()?;
    let client = api::Client::from_env();
    let response = client.grant_project(
        &identity,
        &project,
        &ProjectGrantRequest { email, role },
        send_invite,
    )?;
    if output_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&response).expect("response serializes")
        );
    } else {
        println!("project: {}", response.project_slug);
        println!("email:   {}", response.collaborator.email);
        println!("role:    {}", response.collaborator.role);
        println!("created: {}", response.collaborator.created);
        if !response.invited_emails.is_empty() {
            println!("invited: {}", response.invited_emails.join(", "));
        }
    }
    Ok(())
}

fn project_revoke(args: &[String]) -> Result<(), CliError> {
    if help_requested(args) {
        return print_help(project_revoke_help());
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
                    return Err(CliError::Usage(project_revoke_help().to_string()));
                }
                project = Some(value.to_string());
                index += 1;
            }
        }
    }

    let project = project.ok_or_else(|| CliError::Usage(project_revoke_help().to_string()))?;
    let email = email.ok_or_else(|| CliError::Usage(project_revoke_help().to_string()))?;
    let identity = keys::load_or_create_identity()?;
    let client = api::Client::from_env();
    let response = client.revoke_project(&identity, &project, &ProjectRevokeRequest { email })?;
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

fn project_status(args: &[String]) -> Result<(), CliError> {
    if help_requested(args) {
        return print_help(project_status_help());
    }
    let (project, output_json) = parse_project_read_args(args, project_status_help())?;
    let identity = keys::load_or_create_identity()?;
    let client = api::Client::from_env();
    let response = client.project_status(&identity, &project)?;
    if output_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&response).expect("response serializes")
        );
    } else {
        println!("project: {}", response.slug);
        println!("role:    {}", response.role);
        println!("git:     {}", response.git_remote_url);
        print_project_outputs(&response.outputs);
        if !response.collaborators.is_empty() {
            println!("collaborators:");
            for collaborator in &response.collaborators {
                println!("  {} {}", collaborator.role, collaborator.email);
            }
        }
    }
    Ok(())
}

fn project_list(args: &[String]) -> Result<(), CliError> {
    if help_requested(args) {
        return print_help(project_list_help());
    }
    let output_json = parse_output_json_only(args, project_list_help())?;
    let identity = keys::load_or_create_identity()?;
    let client = api::Client::from_env();
    let response = client.project_list(&identity)?;
    if output_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&response).expect("response serializes")
        );
    } else if response.projects.is_empty() {
        println!(
            "no projects yet; initialize one with `fsite project init --config finite.toml --dry-run --output json`"
        );
    } else {
        for project in &response.projects {
            println!(
                "{:<24} {:<8} {}",
                project.slug, project.role, project.git_remote_url
            );
        }
    }
    Ok(())
}

fn parse_project_read_args(args: &[String], help: &str) -> Result<(String, bool), CliError> {
    let mut project: Option<String> = None;
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
            value => {
                if project.is_some() {
                    return Err(CliError::Usage(help.to_string()));
                }
                project = Some(value.to_string());
                index += 1;
            }
        }
    }
    let project = project.ok_or_else(|| CliError::Usage(help.to_string()))?;
    Ok((project, output_json))
}

fn parse_output_json_only(args: &[String], help: &str) -> Result<bool, CliError> {
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
            other => return Err(CliError::Usage(format!("{help}\nunknown flag `{other}`"))),
        }
    }
    Ok(output_json)
}

fn print_project_outputs(outputs: &[finitesites_proto::dto::ProjectOutputSummary]) {
    for output in outputs {
        let version = output
            .active_version
            .map(|value| format!("v{value}"))
            .unwrap_or_else(|| "unpublished".to_string());
        println!(
            "output:  {} {} {} {} {}:{}",
            output.output_id, output.kind, output.visibility, version, output.branch, output.path
        );
        println!("url:     {}", output.site_url);
    }
}

fn auth_command(args: &[String]) -> Result<(), CliError> {
    let Some((subcommand, rest)) = args.split_first() else {
        return Err(CliError::Usage(auth_help().to_string()));
    };
    match subcommand.as_str() {
        value if is_help_arg(value) => print_help(auth_help()),
        "login" => auth_login(rest),
        "redeem" => auth_redeem(rest),
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
    println!("server grants are checked by project init and auth git, not by whoami");
    Ok(())
}

fn auth_login(args: &[String]) -> Result<(), CliError> {
    if help_requested(args) {
        return print_help(auth_login_help());
    }
    let [email] = args else {
        return Err(CliError::Usage(auth_login_help().to_string()));
    };
    let client = api::Client::from_env();
    let response = client.request_email_login(email)?;
    println!("sent email login for {}", response.email);
    println!("run the fsite auth redeem command from the email to verify this machine");
    Ok(())
}

fn auth_redeem(args: &[String]) -> Result<(), CliError> {
    if help_requested(args) {
        return print_help(auth_redeem_help());
    }
    let [email, token] = args else {
        return Err(CliError::Usage(auth_redeem_help().to_string()));
    };
    let key = keys::load_or_create_email_key(email)?;
    let client = api::Client::from_env();
    let response = client.redeem_email_login(&key, email, token)?;
    println!("verified {} for publishing", response.email);
    Ok(())
}

fn view(args: &[String]) -> Result<(), CliError> {
    if help_requested(args) {
        return print_help(view_help());
    }
    let mut target: Option<String> = None;
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
            value => {
                if target.is_some() {
                    return Err(CliError::Usage(view_help().to_string()));
                }
                target = Some(value.to_string());
                index += 1;
            }
        }
    }
    let target = target.ok_or_else(|| CliError::Usage(view_help().to_string()))?;
    let url = view_target_url(&target);
    let llms_url = append_url_path(&url, "llms.txt");
    if output_json {
        let value = serde_json::json!({
            "url": url,
            "llms_txt": llms_url,
            "read_only": true,
            "edit_hint": "Use fsite project status/list plus fsite auth git if you have Project Repository edit access."
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).expect("view json serializes")
        );
    } else {
        println!("url:      {url}");
        println!("llms.txt: {llms_url}");
        println!("note:     view is read-only; edit through the Project Repository with git");
    }
    Ok(())
}

fn view_target_url(target: &str) -> String {
    if target.starts_with("http://") || target.starts_with("https://") {
        if target.ends_with('/') {
            return target.to_string();
        }
        return format!("{target}/");
    }
    format!("https://{target}.finite.chat/")
}

fn append_url_path(base: &str, path: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    format!("{trimmed}/{path}")
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
            &["--help"][..],
            &["whoami", "--help"][..],
            &["describe", "--help"],
            &["project", "--help"],
            &["project", "init", "--help"],
            &["project", "grant", "--help"],
            &["project", "revoke", "--help"],
            &["project", "status", "--help"],
            &["project", "list", "--help"],
            &["auth", "--help"],
            &["auth", "login", "--help"],
            &["auth", "redeem", "--help"],
            &["auth", "git", "--help"],
            &["view", "--help"],
        ];
        // Bounded by the explicit command table above.
        for command in commands {
            run(&args(command)).unwrap();
        }
    }

    #[test]
    fn top_level_usage_is_project_first() {
        let text = usage();
        assert!(text.contains("shares the whole source tree through a Project Repository"));
        assert!(text.contains("cloneable by collaborators but not served as ordinary web assets"));
        assert!(text.contains("fsite describe workflow publish-static-site --output json"));
        assert!(text.contains("fsite project init --config finite.toml"));
        assert!(text.contains("fsite project grant"));
        assert!(text.contains("fsite project status"));
        assert!(text.contains("fsite auth git"));
        assert!(text.contains("fsite view"));
        assert!(!text.contains("fsite email-login"));
        assert!(!text.contains("fsite project apply"));
        assert!(!text.contains("fsite share"));
        assert!(!text.contains("fsite claim"));
        assert!(!text.contains("fsite publish "));
        assert!(!text.contains("fsite publish-app"));
        assert!(!text.contains("fsite editors"));
        assert!(!text.contains("fsite source"));
    }

    #[test]
    fn publish_static_site_workflow_guides_agents_to_git_deploy_bytes() {
        let value = describe_workflow("publish-static-site").unwrap();
        let text = serde_json::to_string(&value).unwrap();
        assert!(text.contains("authorized collaborators clone the whole source tree"));
        assert!(text.contains("fsite project init --config finite.toml --dry-run --output json"));
        assert!(text.contains("For static sites, Finite serves only committed bytes"));
        assert!(text.contains("Source, data, docs, and build logic can live outside"));
        assert!(text.contains("Do not look for a direct publish/upload command"));
        assert!(text.contains("fsite auth git PROJECT --store --output json"));
    }

    #[test]
    fn old_site_first_commands_point_to_project_repository_workflow() {
        assert!(matches!(
            run(&args(&["publish"])),
            Err(CliError::Usage(message))
                if message.contains("not part of the current Project Repository model")
                    && message.contains("fsite project init --config finite.toml")
                    && message.contains("Deploy Branch")
        ));
        assert!(matches!(
            run(&args(&["publish-app"])),
            Err(CliError::Usage(message))
                if message.contains("not part of the current Project Repository model")
                    && message.contains("fsite auth git PROJECT --store --output json")
        ));
    }

    #[test]
    fn no_arg_commands_reject_extra_non_help_arguments() {
        assert!(matches!(
            run(&args(&["whoami", "extra"])),
            Err(CliError::Usage(_))
        ));
        assert!(matches!(
            run(&args(&["project", "list", "extra"])),
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
    fn removed_commands_point_to_current_primitives() {
        assert!(matches!(
            run(&args(&["share", "demo", "--send-invite"])),
            Err(CliError::Usage(message)) if message.contains("not part of the current Project Repository model")
        ));
        assert!(matches!(
            run(&args(&["project", "apply", "--help"])),
            Err(CliError::Usage(message)) if message.contains("fsite project init --config finite.toml")
        ));
    }

    #[test]
    fn project_example_fixture_matches_committed_config() {
        let config = parse_project_config_toml(include_str!(
            "../../../examples/finitechat-native-mockup/finite.toml"
        ))
        .unwrap();

        assert_eq!(config.project.slug, "finitechat-native");
        assert_eq!(config.outputs.len(), 1);
    }

    #[test]
    fn view_target_url_supports_url_or_name() {
        assert_eq!(
            view_target_url("finitechat-native-mockup"),
            "https://finitechat-native-mockup.finite.chat/"
        );
        assert_eq!(
            append_url_path("https://finitechat-native-mockup.finite.chat/", "llms.txt"),
            "https://finitechat-native-mockup.finite.chat/llms.txt"
        );
    }
}
