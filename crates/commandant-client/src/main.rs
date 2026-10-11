mod admin;
mod cli;
mod run;
mod tui;

use anyhow::Result;
use clap::Parser;
use commandant_client_core::config::{self, Client};
use tokio::runtime::{self, Handle};
use tracing_subscriber::EnvFilter;

use crate::cli::{Cli, ClientArgs, Command, NodeCommand, TaskCommand, TokenCommand};

fn main() {
    let cli = Cli::parse();
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
        .and_then(|runtime| runtime.block_on(dispatch(cli, runtime.handle().clone())));
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

/// Runs the chosen command and returns the process exit code.
async fn dispatch(cli: Cli, handle: Handle) -> Result<i32> {
    let client = || resolve_client(&cli.client);
    match cli.command {
        Command::Login(args) => admin::login(args).await?,
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
        Command::Tui(args) => tui::run(&client()?, args, handle).await?,
    }
    Ok(0)
}

fn resolve_client(args: &ClientArgs) -> Result<Client> {
    config::resolve(
        args.addr.clone(),
        args.token.clone(),
        args.token_file.clone(),
    )
}
