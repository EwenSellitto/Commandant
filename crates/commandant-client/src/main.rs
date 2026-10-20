mod admin;
mod cli;
#[cfg(feature = "gui")]
mod gui;
mod picker;
mod run;
mod start;
mod tui;

use std::io::IsTerminal;

use anyhow::Result;
use clap::CommandFactory;
use commandant_client_core::config::{self, Client};
use tokio::runtime::{self, Handle, Runtime};
use tracing_subscriber::EnvFilter;

use crate::cli::{Cli, ClientArgs, Command, GuiArgs, NodeCommand, TaskCommand, TokenCommand};

fn main() {
    let cli = Cli::try_parse_args(std::env::args_os()).unwrap_or_else(|e| e.exit());
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let result = runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(anyhow::Error::from)
        .and_then(|runtime| launch(cli, &runtime));
    match result {
        Ok(exit_code) => std::process::exit(exit_code),
        Err(e) => {
            match e.downcast_ref::<tonic::Status>() {
                Some(status) => eprintln!("error: {}", status.message()),
                None => eprintln!("error: {e:#}"),
            }
            std::process::exit(1);
        }
    }
}

/// Runs what was asked and returns the process exit code. The GUI takes the
/// main thread; everything else runs on the runtime.
fn launch(cli: Cli, runtime: &Runtime) -> Result<i32> {
    match cli.command {
        Some(Command::Gui(args)) => open_gui(args, &cli.client, runtime),
        // A double-click or a launcher has no terminal to print help to.
        None if cfg!(feature = "gui") && !attached_to_terminal() => {
            open_gui(GuiArgs::default(), &cli.client, runtime)
        }
        None => {
            Cli::command().print_help()?;
            Ok(0)
        }
        Some(command) => runtime.block_on(dispatch(command, &cli.client, runtime.handle().clone())),
    }
}

fn attached_to_terminal() -> bool {
    std::io::stdin().is_terminal()
        || std::io::stdout().is_terminal()
        || std::io::stderr().is_terminal()
}

#[cfg(feature = "gui")]
fn open_gui(args: GuiArgs, client: &ClientArgs, runtime: &Runtime) -> Result<i32> {
    gui::run(args, client, runtime.handle().clone())?;
    Ok(0)
}

#[cfg(not(feature = "gui"))]
fn open_gui(_args: GuiArgs, _client: &ClientArgs, _runtime: &Runtime) -> Result<i32> {
    anyhow::bail!("this build has no GUI; rebuild with the `gui` feature")
}

/// Runs the chosen command and returns the process exit code.
async fn dispatch(command: Command, client_args: &ClientArgs, handle: Handle) -> Result<i32> {
    let client = || client_args.resolve();
    match command {
        Command::Login(args) => admin::login(args).await?,
        Command::Link => println!("{}", client()?.link()),
        Command::Logout => admin::logout(client_args)?,
        Command::Token(TokenCommand::Create { ttl, reusable }) => {
            admin::create_token(&client()?, ttl, reusable).await?
        }
        Command::Node(NodeCommand::Ls) => admin::list_nodes(&client()?).await?,
        Command::Node(NodeCommand::Rm { node }) => admin::remove_node(&client()?, node).await?,
        Command::Node(NodeCommand::StartAgent { node, harness }) => {
            admin::start_agent(&client()?, node, harness).await?
        }
        Command::Node(NodeCommand::Sessions { node }) => {
            admin::list_sessions(&client()?, node).await?
        }
        Command::Node(NodeCommand::Commands { node }) => {
            admin::list_commands(&client()?, node).await?
        }
        Command::Node(NodeCommand::Mcp {
            node,
            connect,
            disconnect,
        }) => admin::mcp(&client()?, node, connect, disconnect).await?,
        Command::Task(TaskCommand::Ls { limit, node }) => {
            admin::list_tasks(&client()?, limit, node).await?
        }
        Command::Task(TaskCommand::Cancel { task_id }) => {
            admin::cancel_task(&client()?, task_id).await?
        }
        Command::Task(TaskCommand::Watch { task_id }) => {
            return run::watch(&client()?, task_id).await;
        }
        Command::Run(args) => return run::run(&client()?, args).await,
        Command::Prompt(args) => return run::prompt(&client()?, args).await,
        Command::Tui(args) => tui::run(client_args.find()?, args, handle).await?,
        Command::Gui(_) => unreachable!("the GUI runs outside the runtime"),
    }
    Ok(0)
}

impl ClientArgs {
    /// The server, which must be named: by these, or by a log in.
    fn resolve(&self) -> Result<Client> {
        config::resolve(
            self.addr.clone(),
            self.token.clone(),
            self.token_file.clone(),
        )
    }

    /// The server, if anything names it yet; none before the first log in.
    fn find(&self) -> Result<Option<Client>> {
        config::find(
            self.addr.clone(),
            self.token.clone(),
            self.token_file.clone(),
        )
    }
}
