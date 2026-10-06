//! The command-line interface definition.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use commandant_common::harness::HarnessKind;
use commandant_common::link::Link;

#[derive(Parser)]
#[command(
    name = "commandant-server",
    version,
    about = "Run a Commandant orchestrator or worker node",
    arg_required_else_help = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the orchestrator.
    Serve(ServeArgs),
    /// Run a worker node that joins an orchestrator.
    Worker(WorkerArgs),
}

#[derive(Args)]
pub struct ServeArgs {
    #[arg(long, env = "COMMANDANT_LISTEN", default_value = "0.0.0.0:7400")]
    pub listen: SocketAddr,
    /// Defaults to the platform data dir (e.g. ~/.local/share/commandant/server).
    #[arg(long, env = "COMMANDANT_DATA_DIR")]
    pub data_dir: Option<PathBuf>,
    /// Host (or host:port) to put in the connection link, e.g. a DNS or
    /// Tailscale name; several, comma-separated, are tried in turn by
    /// clients. Remembered; this machine's primary IP is always added last.
    #[arg(long, env = "COMMANDANT_ADVERTISE")]
    pub advertise: Option<String>,
    /// Also run a worker on this machine.
    #[arg(long, env = "COMMANDANT_LOCAL_WORKER")]
    pub local_worker: bool,
    /// Coding agent for the local worker to host (with --local-worker).
    #[arg(long, env = "COMMANDANT_HARNESS", requires = "local_worker")]
    pub harness: Option<HarnessKind>,
    /// The binary of the agent to host (opencode, claude), instead of the
    /// one on PATH (or installed).
    #[arg(
        long,
        alias = "opencode-bin",
        env = "COMMANDANT_HARNESS_BIN",
        requires = "harness"
    )]
    pub harness_bin: Option<PathBuf>,
    /// Delete the database (nodes, tokens, task history) and start afresh
    /// with a new admin token. Asks twice first.
    #[arg(long)]
    pub reset: bool,
}

#[derive(Args)]
pub struct WorkerArgs {
    /// Connection link printed by `commandant-server serve`. Only needed the
    /// first time; afterwards the worker reconnects on its own.
    #[arg(env = "COMMANDANT_LINK", hide_env_values = true)]
    pub link: Option<Link>,
    /// Orchestrator URL, e.g. http://10.0.0.1:7400 (instead of a link).
    #[arg(long, env = "COMMANDANT_SERVER")]
    pub server: Option<String>,
    /// Join token; only needed the first time.
    #[arg(long, env = "COMMANDANT_JOIN_TOKEN", hide_env_values = true)]
    pub join_token: Option<String>,
    /// Node name; defaults to the hostname.
    #[arg(long, env = "COMMANDANT_NODE_NAME")]
    pub name: Option<String>,
    /// Where node credentials are kept.
    #[arg(long, env = "COMMANDANT_STATE_DIR")]
    pub state_dir: Option<PathBuf>,
    /// Coding agent to host (installed if missing): opencode or claude-code.
    #[arg(long, env = "COMMANDANT_HARNESS")]
    pub harness: Option<HarnessKind>,
    /// The binary of the agent to host (opencode, claude), instead of the
    /// one on PATH (or installed).
    #[arg(
        long,
        alias = "opencode-bin",
        env = "COMMANDANT_HARNESS_BIN",
        requires = "harness"
    )]
    pub harness_bin: Option<PathBuf>,
}
