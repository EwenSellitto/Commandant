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
    eprintln!("Worker-only link: commandant worker {link}");
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
        "{:<10} {:<20} {:<8} {:<20} {:<16} LAST SEEN",
        "ID", "NAME", "STATUS", "HOSTNAME", "PLATFORM"
    );
    for node in nodes {
        let (status, last_seen) = if node.online {
            ("online", "now".to_string())
        } else {
            ("offline", ago(node.last_seen))
        };
        println!(
            "{:<10} {:<20} {:<8} {:<20} {:<16} {}",
            short_id(&node.id),
            node.name,
            status,
            node.hostname,
            format!("{}/{}", node.os, node.arch),
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
