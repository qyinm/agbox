use std::{path::PathBuf, process::ExitCode};

use agbox_release_gate::{
    ReleaseArtifact, Thresholds,
    corpus::{CorpusSpec, manifest},
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
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("agbox release gate: {error}");
            ExitCode::from(65)
        }
    }
}

fn run(cli: Cli) -> Result<(), &'static str> {
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
    }
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<(), &'static str> {
    let encoded = serde_json::to_string(value).map_err(|_| "serialization")?;
    println!("{encoded}");
    Ok(())
}
