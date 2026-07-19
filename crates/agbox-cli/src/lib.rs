//! Native setup and command boundary for agbox.

use std::io::Write;

pub mod args;
pub mod commands;
pub mod config;
pub mod init;
pub mod paths;
pub mod platform;
pub mod tui;

pub use init::{InitOptions, InitReport, Initializer};
pub use paths::AgboxPaths;
pub use platform::{Change, Platform, PlatformError, ServiceSpec};

/// Executes the approved CLI surface.
///
/// # Errors
///
#[allow(clippy::too_many_lines)]
pub async fn run(cli: args::Cli) -> Result<(), CliError> {
    let output = cli.output;
    match cli.command {
        args::Command::Init(arguments) => {
            let executable = std::env::current_exe().map_err(|_| CliError::Unavailable)?;
            let platform = platform::macos::MacOsPlatform::for_current_user(executable)
                .map_err(|_| CliError::Unavailable)?;
            let report = Initializer::new(platform)
                .run(InitOptions {
                    quiet: arguments.quiet,
                })
                .await
                .map_err(|_| CliError::Unavailable)?;
            if !arguments.quiet {
                println!(
                    "paths: {}; claude: {}; codex: {}; service: {}",
                    report.paths, report.claude, report.codex, report.service
                );
            }
            Ok(())
        }
        args::Command::Daemon { command } => {
            commands::daemon::run(command, &AgboxPaths::from_home(&user_home()?)).await
        }
        args::Command::Status => {
            let client = human_client(cli.project_root).await?;
            let value = call(&client, agbox_core::api::AppRequest::Health).await?;
            commands::output::response(output, value)
        }
        args::Command::Doctor => {
            let paths = AgboxPaths::from_home(&user_home()?);
            let daemon_reachable = match project_root(cli.project_root) {
                Ok(root) => commands::client::scoped_client(
                    &paths,
                    &root,
                    agbox_service::ipc::WireActor::HumanCli,
                )
                .await
                .is_ok(),
                Err(_) => false,
            };
            render_doctor(
                output,
                commands::doctor::DoctorReport::inspect(&paths, daemon_reachable),
            )
        }
        args::Command::Agent { command } => run_agent(command).await,
        args::Command::Config { command } => run_config(output, command),
        args::Command::Work { command } => {
            let client = human_client(cli.project_root).await?;
            let request = match command {
                args::WorkCommand::List => agbox_core::api::AppRequest::ListWork {
                    status: None,
                    limit: 20,
                },
                args::WorkCommand::Current => agbox_core::api::AppRequest::CurrentWork,
                args::WorkCommand::Show { work_id } => agbox_core::api::AppRequest::GetWork {
                    work_id: parse_work_id(&work_id)?,
                },
            };
            commands::output::response(output, call(&client, request).await?)
        }
        args::Command::Handoff { work_id } => {
            let client = human_client(cli.project_root).await?;
            let value = call(
                &client,
                agbox_core::api::AppRequest::GetWork {
                    work_id: parse_work_id(&work_id)?,
                },
            )
            .await?;
            commands::output::response(output, value)
        }
        args::Command::Evidence { evidence_id, raw } => {
            let client = human_client(cli.project_root).await?;
            let evidence_id = agbox_core::EvidenceId::parse_wire(&evidence_id)
                .ok_or(CliError::InvalidIdentifier)?;
            let disclosure = if raw {
                agbox_core::api::EvidenceDisclosure::AuthorizedRaw
            } else {
                agbox_core::api::EvidenceDisclosure::Redacted
            };
            let value = call(
                &client,
                agbox_core::api::AppRequest::GetEvidence {
                    evidence_id,
                    disclosure,
                },
            )
            .await?;
            commands::output::response(output, value)
        }
        args::Command::Search { query, limit } => {
            let client = human_client(cli.project_root).await?;
            let value = call(
                &client,
                agbox_core::api::AppRequest::SearchWork { query, limit },
            )
            .await?;
            commands::output::response(output, value)
        }
        args::Command::Tui => {
            let client = human_client(cli.project_root).await?;
            let value = call(
                &client,
                agbox_core::api::AppRequest::ListWork {
                    status: None,
                    limit: 100,
                },
            )
            .await?;
            let agbox_core::api::AppResponse::WorkList(page) = value else {
                return Err(CliError::Unavailable);
            };
            tui::run(page.items).map_err(|_| CliError::Unavailable)
        }
        args::Command::Forget { command } => {
            let client = human_client(cli.project_root).await?;
            let request = match command {
                args::ForgetCommand::Work { work_id } => agbox_core::api::AppRequest::ForgetWork {
                    work_id: parse_work_id(&work_id)?,
                },
                args::ForgetCommand::Project => agbox_core::api::AppRequest::ForgetProject,
            };
            commands::output::response(output, call(&client, request).await?)
        }
        args::Command::Mcp { provider } => {
            let root = project_root(cli.project_root)?;
            let provider = match provider {
                args::ProviderArg::Claude => agbox_core::Provider::Claude,
                args::ProviderArg::Codex => agbox_core::Provider::Codex,
            };
            let client = commands::client::scoped_client(
                &AgboxPaths::from_home(&user_home()?),
                &root,
                agbox_service::ipc::WireActor::Agent { provider },
            )
            .await?;
            agbox_service::serve_stdio(agbox_service::HandoffMcpServer::new(std::sync::Arc::new(
                client,
            )))
            .await
            .map_err(|_| CliError::Unavailable)
        }
        args::Command::Hook { .. } => Err(CliError::Unavailable),
    }
}

/// Bounded public CLI failures.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("agbox daemon is unavailable; run `agbox daemon start`")]
    Unavailable,
    #[error("project root is not a safe Git repository")]
    InvalidProject,
    #[error("owner home directory is unavailable")]
    HomeUnavailable,
    #[error("identifier is invalid")]
    InvalidIdentifier,
    #[error("unable to write bounded CLI output")]
    Output,
    #[error("configuration value is invalid")]
    InvalidConfig,
}

fn project_root(configured: Option<std::path::PathBuf>) -> Result<std::path::PathBuf, CliError> {
    configured
        .map_or_else(std::env::current_dir, Ok)
        .map_err(|_| CliError::InvalidProject)
}

fn user_home() -> Result<std::path::PathBuf, CliError> {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or(CliError::HomeUnavailable)
}

impl CliError {
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        69
    }
}

async fn human_client(
    configured: Option<std::path::PathBuf>,
) -> Result<agbox_service::IpcAppClient, CliError> {
    let root = project_root(configured)?;
    commands::client::scoped_client(
        &AgboxPaths::from_home(&user_home()?),
        &root,
        agbox_service::ipc::WireActor::HumanCli,
    )
    .await
}

async fn run_agent(command: args::AgentCommand) -> Result<(), CliError> {
    let executable = std::env::current_exe().map_err(|_| CliError::Unavailable)?;
    let platform = platform::macos::MacOsPlatform::for_current_user(executable)
        .map_err(|_| CliError::Unavailable)?;
    match command {
        args::AgentCommand::List => {
            let home = platform.home().map_err(|_| CliError::Unavailable)?;
            println!(
                "claude: {}\ncodex: {}",
                if home.join(".claude.json").is_file() {
                    "configured"
                } else {
                    "not configured"
                },
                if home.join(".codex/config.toml").is_file() {
                    "configured"
                } else {
                    "not configured"
                }
            );
            Ok(())
        }
        args::AgentCommand::Connect => Initializer::new(platform)
            .run(InitOptions { quiet: true })
            .await
            .map(|_| ())
            .map_err(|_| CliError::Unavailable),
        args::AgentCommand::Disconnect => commands::agent::disconnect(&platform),
    }
}

fn run_config(output: args::Output, command: args::ConfigCommand) -> Result<(), CliError> {
    let executable = std::env::current_exe().map_err(|_| CliError::Unavailable)?;
    let platform = platform::macos::MacOsPlatform::for_current_user(executable)
        .map_err(|_| CliError::Unavailable)?;
    let settings = commands::config::run(&platform, command)?;
    match output {
        args::Output::Json => {
            let encoded = serde_json::to_vec(&settings).map_err(|_| CliError::Output)?;
            std::io::stdout()
                .lock()
                .write_all(&encoded)
                .and_then(|()| std::io::stdout().lock().write_all(b"\n"))
                .map_err(|_| CliError::Output)
        }
        args::Output::Text => {
            println!("retention_days={}", settings.retention_days);
            Ok(())
        }
    }
}

async fn call(
    client: &agbox_service::IpcAppClient,
    request: agbox_core::api::AppRequest,
) -> Result<agbox_core::api::AppResponse, CliError> {
    use agbox_service::AppClient;

    client
        .call(request)
        .await
        .map_err(|_| CliError::Unavailable)
}

fn parse_work_id(value: &str) -> Result<agbox_core::WorkId, CliError> {
    agbox_core::WorkId::parse_wire(value).ok_or(CliError::InvalidIdentifier)
}

fn render_doctor(
    output: args::Output,
    report: commands::doctor::DoctorReport,
) -> Result<(), CliError> {
    match output {
        args::Output::Json => {
            let encoded = serde_json::to_vec(&report).map_err(|_| CliError::Output)?;
            std::io::stdout()
                .lock()
                .write_all(&encoded)
                .and_then(|()| std::io::stdout().lock().write_all(b"\n"))
                .map_err(|_| CliError::Output)
        }
        args::Output::Text => {
            for check in report.checks {
                println!("{:?}: {}", check.severity, check.code);
                if !check.remediation.is_empty()
                    && check.severity != commands::doctor::DoctorSeverity::Healthy
                {
                    println!("  {}", check.remediation);
                }
            }
            Ok(())
        }
    }
}
