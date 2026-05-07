use anyhow::Result;
use clap::Parser;
use switchboard::cli;

fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    cli::dispatch(cli)
}
