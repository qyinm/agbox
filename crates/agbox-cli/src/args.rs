//! Exact, non-executing command-line surface for agbox.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "agbox", version, about = "Local cross-agent work handoff")]
pub struct Cli {
    #[arg(long, global = true, value_enum, default_value_t = Output::Text)]
    pub output: Output,
    #[arg(long, global = true)]
    pub project_root: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum Output {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ProviderArg {
    Claude,
    Codex,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Init(InitArgs),
    Status,
    Doctor,
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    Work {
        #[command(subcommand)]
        command: WorkCommand,
    },
    Handoff {
        work_id: String,
    },
    Evidence {
        evidence_id: String,
        #[arg(long)]
        raw: bool,
    },
    Search {
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: u16,
    },
    Tui,
    Mcp {
        #[arg(long, value_enum)]
        provider: ProviderArg,
    },
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Forget {
        #[command(subcommand)]
        command: ForgetCommand,
    },
    #[command(hide = true)]
    Hook {
        #[command(subcommand)]
        command: HookCommand,
    },
}

#[derive(Debug, Args)]
pub struct InitArgs {
    #[arg(long)]
    pub quiet: bool,
}
#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    Start {
        #[arg(long)]
        foreground: bool,
    },
    Stop,
    Logs {
        #[arg(long)]
        follow: bool,
    },
}
#[derive(Debug, Subcommand)]
pub enum AgentCommand {
    List,
    Connect,
    Disconnect,
}
#[derive(Debug, Subcommand)]
pub enum WorkCommand {
    List,
    Current,
    Show { work_id: String },
}
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    Show,
    Set { key: ConfigKey, value: String },
}
#[derive(Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum ConfigKey {
    RetentionDays,
}
#[derive(Debug, Subcommand)]
pub enum ForgetCommand {
    Work { work_id: String },
    Project,
}
#[derive(Debug, Subcommand)]
pub enum HookCommand {
    Ingest {
        #[arg(long, value_enum)]
        provider: ProviderArg,
        #[arg(long, default_value_t = 65_536)]
        max_bytes: usize,
    },
    ActiveIndex {
        #[arg(long, value_enum)]
        provider: ProviderArg,
        #[arg(long, default_value_t = 10)]
        max_items: u16,
    },
}
