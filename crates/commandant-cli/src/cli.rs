//! The command-line interface definition.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use commandant_common::link::Link;

#[derive(Parser)]
#[command(
    name = "commandant",
    version,
    about = "Orchestrate AI coding agents across worker nodes"
)]
pub struct Cli {
    #[command(flatten)]
    pub client: ClientArgs,
    #[command(subcommand)]
    pub command: Command,
}

/// How client commands reach the orchestrator.
#[derive(Args)]
pub struct ClientArgs {
    /// Orchestrator URL.
    #[arg(long, global = true, env = "COMMANDANT_ADDR")]
    pub addr: Option<String>,
    /// Admin token.
    #[arg(long, global = true, env = "COMMANDANT_TOKEN", hide_env_values = true)]
    pub token: Option<String>,
    /// File containing the admin token.
    #[arg(long, global = true, env = "COMMANDANT_TOKEN_FILE")]
    pub token_file: Option<PathBuf>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the orchestrator.
    Server(ServerArgs),
    /// Run a worker node that joins an orchestrator.
    Worker(WorkerArgs),
    /// Save the orchestrator address and admin token to the config file.
    Login(LoginArgs),
    /// Manage join tokens.
    #[command(subcommand)]
    Token(TokenCommand),
    /// Manage nodes.
    #[command(subcommand)]
    Node(NodeCommand),
    /// Run a command on a node and stream its output.
    Run(RunArgs),
    /// Inspect tasks.
    #[command(subcommand)]
    Task(TaskCommand),
}

#[derive(Args)]
pub struct ServerArgs {
    #[arg(long, env = "COMMANDANT_LISTEN", default_value = "0.0.0.0:7400")]
    pub listen: SocketAddr,
    /// Defaults to the platform data dir (e.g. ~/.local/share/commandant/server).
    #[arg(long, env = "COMMANDANT_DATA_DIR")]
    pub data_dir: Option<PathBuf>,
    /// Host (or host:port) to put in the connection link, e.g. a DNS or
    /// Tailscale name. Remembered; defaults to this machine's primary IP.
    #[arg(long, env = "COMMANDANT_ADVERTISE")]
    pub advertise: Option<String>,
    /// Also run a worker on this machine.
    #[arg(long, env = "COMMANDANT_LOCAL_WORKER")]
    pub local_worker: bool,
}

#[derive(Args)]
pub struct WorkerArgs {
    /// Connection link printed by `commandant server`. Only needed the
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
}

#[derive(Args)]
pub struct LoginArgs {
    /// Connection link printed by `commandant server`, or an orchestrator URL.
    pub target: String,
    /// Admin token, when `target` is a URL.
    #[arg(long = "with-token")]
    pub admin_token: Option<String>,
}

#[derive(Args)]
pub struct RunArgs {
    /// Node name, id or id prefix.
    pub node: String,
    /// Working directory on the node.
    #[arg(long)]
    pub cwd: Option<String>,
    /// Extra environment variables (KEY=VALUE).
    #[arg(short, long = "env", value_parser = parse_key_val)]
    pub env: Vec<(String, String)>,
    #[arg(last = true, required = true)]
    pub argv: Vec<String>,
}

#[derive(Subcommand)]
pub enum TokenCommand {
    /// Create a join token.
    Create {
        /// Lifetime, e.g. 30m, 1h, 7d. 0 = never expires.
        #[arg(long, default_value = "1h", value_parser = parse_duration)]
        ttl: Duration,
        /// Allow the token to be used by several nodes.
        #[arg(long)]
        reusable: bool,
    },
}

#[derive(Subcommand)]
pub enum NodeCommand {
    /// List nodes.
    #[command(alias = "list")]
    Ls,
    /// Forget a node; it must rejoin with a new token.
    Rm { node: String },
}

#[derive(Subcommand)]
pub enum TaskCommand {
    /// List recent tasks.
    #[command(alias = "list")]
    Ls {
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Cancel a running task.
    Cancel {
        /// Task id or unambiguous prefix.
        task_id: String,
    },
}

fn parse_key_val(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .ok_or_else(|| format!("expected KEY=VALUE, got {s:?}"))
}

fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let unit_start = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (amount, unit) = s.split_at(unit_start);
    let amount: u64 = amount
        .parse()
        .map_err(|_| format!("invalid duration {s:?}"))?;
    let seconds_per_unit = match unit {
        "" | "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        _ => return Err(format!("invalid duration unit {unit:?} (use s, m, h or d)")),
    };
    Ok(Duration::from_secs(amount * seconds_per_unit))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("90"), Ok(Duration::from_secs(90)));
        assert_eq!(parse_duration("30m"), Ok(Duration::from_secs(1800)));
        assert_eq!(parse_duration("7d"), Ok(Duration::from_secs(7 * 86400)));
        assert!(parse_duration("1w").is_err());
        assert!(parse_duration("h").is_err());
    }
}
