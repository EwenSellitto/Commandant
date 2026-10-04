mod admin;
mod cli;
mod config;
mod run;
mod server;
mod tui;
mod worker;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use crate::cli::{Cli, ClientArgs, Command, NodeCommand, TaskCommand, TokenCommand};
use crate::config::Client;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    match dispatch(cli).await {
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
async fn dispatch(cli: Cli) -> Result<i32> {
    let client = || resolve_client(&cli.client);
    match cli.command {
        Command::Server(args) => server::run(args).await?,
        Command::Worker(args) => worker::run(args).await?,
        Command::Login(args) => admin::login(args).await?,
        Command::Token(TokenCommand::Create { ttl, reusable }) => {
            admin::create_token(&client()?, ttl, reusable).await?
        }
        Command::Node(NodeCommand::Ls) => admin::list_nodes(&client()?).await?,
        Command::Node(NodeCommand::Rm { node }) => admin::remove_node(&client()?, node).await?,
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
        Command::Task(TaskCommand::Ls { limit }) => admin::list_tasks(&client()?, limit).await?,
        Command::Task(TaskCommand::Cancel { task_id }) => {
            admin::cancel_task(&client()?, task_id).await?
        }
        Command::Run(args) => return run::run(&client()?, args).await,
        Command::Prompt(args) => return run::prompt(&client()?, args).await,
        Command::Tui(args) => tui::run(&client()?, args).await?,
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
