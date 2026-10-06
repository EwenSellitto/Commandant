//! Client commands that administer the cluster.

use std::time::Duration;

use anyhow::{Context, Result};
use commandant_common::link::Link;
use commandant_common::time::ago;
use commandant_proto::*;

use crate::cli::LoginArgs;
use crate::config::{self, Client, FileConfig};

pub async fn login(args: LoginArgs) -> Result<()> {
    let client = match (args.target.parse::<Link>(), args.admin_token) {
        (Ok(link), None) => Client {
            addr: link.addr(),
            token: link.token,
        },
        (_, Some(token)) => Client {
            addr: args.target,
            token,
        },
        (Err(e), None) => {
            return Err(e.context("expected a connection link (or a URL with --with-token)"));
        }
    };
    client
        .connect()
        .await?
        .list_nodes(ListNodesRequest {})
        .await
        .context("checking the token")?;
    let path = config::save(&FileConfig {
        addr: Some(client.addr),
        token: Some(client.token),
    })?;
    println!("Logged in; saved to {}", path.display());
    Ok(())
}

pub async fn create_token(client: &Client, ttl: Duration, reusable: bool) -> Result<()> {
    let created = client
        .connect()
        .await?
        .create_join_token(CreateJoinTokenRequest {
            ttl_secs: ttl.as_secs(),
            reusable,
        })
        .await?
        .into_inner();
    println!("{}", created.token);
    let uses = if reusable { "reusable" } else { "single use" };
    let lifetime = match created.expires_at {
        Some(_) => format!("expires in {}s", ttl.as_secs()),
        None => "never expires".into(),
    };
    eprintln!("({uses}, {lifetime})");
    let link = Link::for_addr(&created.token, &client.addr);
    eprintln!("Worker-only link: commandant-server worker {link}");
    Ok(())
}

pub async fn list_nodes(client: &Client) -> Result<()> {
    let nodes = client
        .connect()
        .await?
        .list_nodes(ListNodesRequest {})
        .await?
        .into_inner()
        .nodes;
    if nodes.is_empty() {
        eprintln!("No nodes yet. Create a join token with `commandant token create`.");
        return Ok(());
    }
    println!(
        "{:<10} {:<20} {:<8} {:<20} {:<16} {:<10} LAST SEEN",
        "ID", "NAME", "STATUS", "HOSTNAME", "PLATFORM", "HARNESS"
    );
    for node in nodes {
        let (status, last_seen) = if node.online {
            ("online", "now".to_string())
        } else {
            ("offline", ago(node.last_seen))
        };
        let harnesses = match node.harnesses.is_empty() {
            true => "-".to_string(),
            false => node.harnesses.join(","),
        };
        println!(
            "{:<10} {:<20} {:<8} {:<20} {:<16} {:<10} {}",
            short_id(&node.id),
            node.name,
            status,
            node.hostname,
            format!("{}/{}", node.os, node.arch),
            harnesses,
            last_seen
        );
    }
    Ok(())
}

pub async fn remove_node(client: &Client, node: String) -> Result<()> {
    client
        .connect()
        .await?
        .remove_node(RemoveNodeRequest { node: node.clone() })
        .await?;
    println!("Removed {node}");
    Ok(())
}

pub async fn start_agent(client: &Client, node: String, harness: String) -> Result<()> {
    eprintln!("Starting {harness} on {node}; installing it first can take a few minutes…");
    let node = client
        .connect()
        .await?
        .start_harness(StartHarnessRequest { node, harness })
        .await?
        .into_inner();
    println!("{} now hosts {}", node.name, node.harnesses.join(", "));
    Ok(())
}

pub async fn list_sessions(client: &Client, node: String) -> Result<()> {
    let sessions = client
        .connect()
        .await?
        .list_agent_sessions(ListAgentSessionsRequest { node })
        .await?
        .into_inner()
        .sessions;
    if sessions.is_empty() {
        eprintln!("The agent has no sessions yet.");
        return Ok(());
    }
    println!(
        "{:<32} {:<9} {:<8} {:<40} TITLE",
        "ID", "UPDATED", "STATUS", "DIRECTORY"
    );
    for session in sessions {
        let status = if session.busy { "running" } else { "idle" };
        println!(
            "{:<32} {:<9} {:<8} {:<40} {}",
            session.id,
            ago(session.updated),
            status,
            session.directory,
            session.title
        );
    }
    Ok(())
}

pub async fn list_commands(client: &Client, node: String) -> Result<()> {
    let commands = client
        .connect()
        .await?
        .get_agent_options(GetAgentOptionsRequest { node })
        .await?
        .into_inner();
    if commands.loading {
        eprintln!(
            "The agent's commands are still loading (its MCP servers are connecting); try again in a moment."
        );
        return Ok(());
    }
    let commands = commands.commands;
    if commands.is_empty() {
        eprintln!("The agent has no commands or skills.");
        return Ok(());
    }
    println!("{:<24} {:<8} DESCRIPTION", "NAME", "SOURCE");
    for command in commands {
        println!(
            "{:<24} {:<8} {}",
            command.name, command.source, command.description
        );
    }
    Ok(())
}

/// Lists the MCP servers, after connecting or disconnecting one if asked.
pub async fn mcp(
    client: &Client,
    node: String,
    connect: Option<String>,
    disconnect: Option<String>,
) -> Result<()> {
    let mut control = client.connect().await?;
    let switch = match (connect, disconnect) {
        (Some(name), _) => Some((name, true)),
        (None, Some(name)) => Some((name, false)),
        (None, None) => None,
    };
    let options = match switch {
        Some((name, connect)) => {
            let request = SwitchMcpServerRequest {
                node,
                name,
                connect,
            };
            control.switch_mcp_server(request).await?
        }
        None => {
            control
                .get_agent_options(GetAgentOptionsRequest { node })
                .await?
        }
    }
    .into_inner();
    if options.loading {
        eprintln!("The agent's MCP servers are still connecting; try again in a moment.");
        return Ok(());
    }
    if options.mcp_servers.is_empty() {
        eprintln!("The agent has no MCP servers configured.");
        return Ok(());
    }
    println!("{:<24} {:<12} ERROR", "NAME", "STATUS");
    for server in options.mcp_servers {
        println!("{:<24} {:<12} {}", server.name, server.status, server.error);
    }
    Ok(())
}

pub async fn list_tasks(client: &Client, limit: u32) -> Result<()> {
    let tasks = client
        .connect()
        .await?
        .list_tasks(ListTasksRequest { limit })
        .await?
        .into_inner()
        .tasks;
    println!(
        "{:<10} {:<16} {:<10} {:<5} {:<9} COMMAND",
        "ID", "NODE", "STATUS", "EXIT", "STARTED"
    );
    for task in tasks {
        let exit_code = task.exit_code.map_or("-".into(), |code| code.to_string());
        println!(
            "{:<10} {:<16} {:<10} {:<5} {:<9} {}",
            short_id(&task.id),
            task.node_name,
            task.status,
            exit_code,
            ago(task.created_at),
            task.argv.join(" ")
        );
    }
    Ok(())
}

pub async fn cancel_task(client: &Client, task_id: String) -> Result<()> {
    client
        .connect()
        .await?
        .cancel_task(CancelTaskRequest {
            task_id: task_id.clone(),
        })
        .await?;
    println!("Cancellation requested for {task_id}");
    Ok(())
}

fn short_id(id: &str) -> &str {
    &id[..id.len().min(8)]
}
