//! The command-line interface definition.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};

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
    /// Send a prompt to a node's coding agent and stream its reply.
    Prompt(PromptArgs),
    /// Chat with a node's coding agent in a terminal UI.
    Tui(TuiArgs),
    /// Inspect tasks.
    #[command(subcommand)]
    Task(TaskCommand),
}

#[derive(Args)]
pub struct LoginArgs {
    /// Connection link printed by `commandant-server serve`, or an orchestrator URL.
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

#[derive(Args)]
pub struct PromptArgs {
    /// Node name, id or id prefix.
    pub node: String,
    /// What to ask the agent; with --command, the command's arguments.
    #[arg(required_unless_present = "command", num_args = 1..)]
    pub prompt: Vec<String>,
    /// Run one of the agent's commands or skills (see `commandant node
    /// commands`), e.g. `--command review`.
    #[arg(long, short = 'c')]
    pub command: Option<String>,
    /// Continue this session instead of starting a new one.
    #[arg(long, short)]
    pub session: Option<String>,
    /// Working directory on the node; defaults to the session's, or the worker's.
    #[arg(long)]
    pub cwd: Option<String>,
    /// Model as provider/model, e.g. anthropic/claude-sonnet-5.
    #[arg(long, short)]
    pub model: Option<String>,
    /// Agent to use, e.g. build or plan.
    #[arg(long)]
    pub agent: Option<String>,
    /// Thinking effort, e.g. low or high; the model's own names apply.
    #[arg(long)]
    pub effort: Option<String>,
}

#[derive(Args)]
pub struct TuiArgs {
    /// Node name, id or id prefix; defaults to the only online node with an
    /// agent harness.
    pub node: Option<String>,
    /// Continue this session instead of starting a new one.
    #[arg(long, short)]
    pub session: Option<String>,
    /// Working directory on the node; defaults to the session's, or the worker's.
    #[arg(long)]
    pub cwd: Option<String>,
    /// Model as provider/model, e.g. anthropic/claude-sonnet-5.
    #[arg(long, short)]
    pub model: Option<String>,
    /// Agent to use, e.g. build or plan.
    #[arg(long)]
    pub agent: Option<String>,
    /// Thinking effort, e.g. low or high; the model's own names apply.
    #[arg(long)]
    pub effort: Option<String>,
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
    /// Start a coding agent on a node that has none (installed if missing);
    /// it runs until the worker stops.
    StartAgent {
        node: String,
        /// The harness, e.g. opencode.
        harness: String,
    },
    /// List the agent sessions saved on a node, latest first.
    Sessions { node: String },
    /// List the commands and skills of a node's agent.
    Commands { node: String },
    /// List a node's MCP servers, or connect or disconnect one.
    Mcp {
        node: String,
        /// Connect this server.
        #[arg(long, conflicts_with = "disconnect")]
        connect: Option<String>,
        /// Disconnect this server.
        #[arg(long)]
        disconnect: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum TaskCommand {
    /// List recent tasks.
    #[command(alias = "list")]
    Ls {
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Print a task's output, and follow it while it runs.
    Watch {
        /// Task id or unambiguous prefix.
        task_id: String,
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
