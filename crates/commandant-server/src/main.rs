mod cli;
mod serve;
mod worker;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use crate::cli::{Cli, Command};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let result = match cli.command {
        Command::Serve(args) => serve::run(args).await,
        Command::Worker(args) => worker::run(args).await,
    };
    // Exits rather than returning: dropping the runtime would wait for any
    // blocking task still running (a git clone, say) after Ctrl-C.
    let code = match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    };
    std::process::exit(code);
}
