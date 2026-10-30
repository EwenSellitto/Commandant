//! The command-line interface definition.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use clap::error::ErrorKind;
use clap::parser::ValueSource;
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "commandant",
    version,
    about = "Orchestrate AI coding agents across worker nodes"
)]
pub struct Cli {
    #[command(flatten)]
    pub client: ClientArgs,
    /// None for a bare `commandant`: help in a terminal, the GUI elsewhere.
    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    /// Parses the command line, refusing flags on a bare `commandant` (the
    /// environment may still set them).
    pub fn try_parse_args<I, T>(args: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let mut command = Self::command();
        let matches = command.try_get_matches_from_mut(args)?;
        let cli = Self::from_arg_matches(&matches)?;
        let flagged = ["addr", "token", "token_file"]
            .into_iter()
            .any(|id| matches.value_source(id) == Some(ValueSource::CommandLine));
        if cli.command.is_none() && flagged {
            return Err(command.error(
                ErrorKind::MissingSubcommand,
                "--addr, --token and --token-file need a command",
            ));
        }
        Ok(cli)
    }
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
    /// Print the server's connection link, to connect another client.
    Link,
    /// Forget the saved server.
    Logout,
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
    Tui(UiArgs),
    /// Chat with nodes' coding agents in a window.
    Gui(GuiArgs),
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

/// Where a front end starts; `tui` and `gui` take the same.
#[derive(Debug, Default, PartialEq, Args)]
pub struct UiArgs {
    /// Node name, id or id prefix; defaults to the only online node with an
    /// agent harness.
    pub node: Option<String>,
    /// Continue this session, on that node, instead of starting a new one.
    #[arg(long, short, requires = "node")]
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

#[derive(Default, Args)]
pub struct GuiArgs {
    #[command(flatten)]
    pub ui: UiArgs,
    /// Keep the window attached to this terminal (always so on Linux).
    #[arg(long)]
    pub foreground: bool,
}

#[derive(Subcommand)]
pub enum TokenCommand {
    /// Create a join token.
    Create {
        /// Lifetime, e.g. 30m, 1h, 7d. 0 = never expires.
        #[arg(long, default_value = "1h", value_parser = commandant_common::time::parse_duration)]
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
        /// Only this node's tasks (id, id prefix or name).
        #[arg(long)]
        node: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn try_parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_args(["commandant"].iter().chain(args))
    }

    fn parse(args: &[&str]) -> Option<Command> {
        try_parse(args).unwrap().command
    }

    #[test]
    fn tui_and_gui_take_the_same_arguments() {
        let args = [
            "box", "-s", "ses_1", "--cwd", "/src", "-m", "a/b", "--agent", "plan", "--effort",
            "high",
        ];
        let Some(Command::Tui(tui)) = parse(&[&["tui"], &args[..]].concat()) else {
            panic!("not tui");
        };
        let Some(Command::Gui(gui)) = parse(&[&["gui"], &args[..]].concat()) else {
            panic!("not gui");
        };
        assert_eq!(tui, gui.ui);
        assert_eq!(tui.node.as_deref(), Some("box"));
        assert_eq!(tui.effort.as_deref(), Some("high"));
        assert!(!gui.foreground);
    }

    #[test]
    fn a_session_needs_the_node_it_is_on() {
        for front in ["tui", "gui"] {
            assert!(try_parse(&[front, "-s", "ses_1"]).is_err(), "{front}");
            assert!(try_parse(&[front, "box", "-s", "ses_1"]).is_ok(), "{front}");
        }
    }

    #[test]
    fn gui_can_stay_in_the_foreground() {
        let Some(Command::Gui(gui)) = parse(&["gui", "--foreground"]) else {
            panic!("not gui");
        };
        assert!(gui.foreground);
    }

    #[test]
    fn the_link_is_shown_and_the_server_forgotten_from_the_shell() {
        assert!(matches!(parse(&["link"]), Some(Command::Link)));
        assert!(matches!(parse(&["logout"]), Some(Command::Logout)));
    }

    #[test]
    fn bare_commandant_has_no_command() {
        assert!(parse(&[]).is_none());
    }

    #[test]
    fn bare_commandant_takes_no_flags() {
        assert!(try_parse(&["--addr", "http://box:7400"]).is_err());
        assert!(try_parse(&["--token-file", "/t"]).is_err());
        assert!(parse(&["node", "ls", "--addr", "http://box:7400"]).is_some());
        assert!(parse(&["--addr", "http://box:7400", "node", "ls"]).is_some());
    }
}
