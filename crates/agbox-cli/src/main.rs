use clap::Parser;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match agbox_cli::run(agbox_cli::args::Cli::parse()).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::from(error.exit_code())
        }
    }
}
