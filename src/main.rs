use clap::Parser;

/// Specio CLI. Commands are added from Phase 07.
#[derive(Parser)]
#[command(name = "specio", version)]
struct Cli {}

fn main() {
    Cli::parse();
}
