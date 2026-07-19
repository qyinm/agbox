use clap::Parser;

#[derive(Debug, Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Debug, clap::Subcommand)]
enum Command {
    Contract,
}
fn main() {
    let _ = Cli::parse();
}
