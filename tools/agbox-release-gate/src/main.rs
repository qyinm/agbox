use std::{path::PathBuf, process::ExitCode, time::Duration};

use agbox_release_gate::{
    ReleaseArtifact, Thresholds,
    corpus::{CorpusSpec, manifest},
    run::{Profile, RunOptions, execute},
};
use clap::Parser;

#[derive(Debug, Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Prints the immutable release thresholds used by CI and cutover.
    Contract,
    /// Prints only sanitized deterministic corpus metadata.
    Manifest,
    /// Validates that a full release artifact authorizes the named candidate.
    Verify {
        #[arg(long)]
        report: PathBuf,
        #[arg(long)]
        commit: String,
        #[arg(long)]
        binary_sha256: String,
    },
    /// Runs the isolated corpus, ingestion, IPC, recovery, and RSS gate.
    Run {
        #[arg(long, value_parser = parse_profile)]
        profile: Profile,
        #[arg(long, value_parser = parse_duration)]
        duration: Duration,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        commit: String,
        #[arg(long, default_value = "aarch64-apple-darwin")]
        target: String,
        #[arg(long)]
        binary: PathBuf,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("agbox release gate: {error}");
            ExitCode::from(65)
        }
    }
}

async fn run(cli: Cli) -> Result<(), &'static str> {
    match cli.command {
        Command::Contract => print_json(&Thresholds::release()),
        Command::Manifest => print_json(&manifest(&CorpusSpec::release())),
        Command::Verify {
            report,
            commit,
            binary_sha256,
        } => {
            let metadata = std::fs::symlink_metadata(&report).map_err(|_| "report_unavailable")?;
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.len() > 1_048_576
            {
                return Err("report_unsafe");
            }
            let bytes = std::fs::read(report).map_err(|_| "report_unavailable")?;
            let artifact: ReleaseArtifact =
                serde_json::from_slice(&bytes).map_err(|_| "report_invalid")?;
            artifact.verify_for_cutover(&commit, &binary_sha256)?;
            println!("release artifact verified");
            Ok(())
        }
        Command::Run {
            profile,
            duration,
            output,
            commit,
            target,
            binary,
        } => {
            let artifact = execute(RunOptions {
                profile,
                duration,
                output_directory: output,
                commit_sha: commit,
                target,
                binary,
            })
            .await
            .map_err(|error| {
                eprintln!("agbox release gate run failure: {error}");
                "run_failed"
            })?;
            print_json(&artifact)
        }
    }
}

fn parse_profile(value: &str) -> Result<Profile, String> {
    match value {
        "ci-smoke" => Ok(Profile::CiSmoke),
        "release" => Ok(Profile::Release),
        _ => Err("profile must be ci-smoke or release".into()),
    }
}

fn parse_duration(value: &str) -> Result<Duration, String> {
    let (number, multiplier) = match value.as_bytes().last().copied() {
        Some(b'm') => (&value[..value.len() - 1], 60_u64),
        Some(b'h') => (&value[..value.len() - 1], 60 * 60),
        Some(b's') => (&value[..value.len() - 1], 1),
        _ => return Err("duration must end in s, m, or h".into()),
    };
    let count = number
        .parse::<u64>()
        .map_err(|_| "duration must have a positive integer value".to_owned())?;
    if count == 0 {
        return Err("duration must be positive".into());
    }
    count
        .checked_mul(multiplier)
        .map(Duration::from_secs)
        .ok_or_else(|| "duration is too large".into())
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<(), &'static str> {
    let encoded = serde_json::to_string(value).map_err(|_| "serialization")?;
    println!("{encoded}");
    Ok(())
}
