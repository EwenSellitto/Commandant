//! Client commands that administer the cluster.

use std::time::Duration;

use anyhow::{Context, Result};
use commandant_common::link::Link;
use commandant_common::time::ago;
use commandant_proto::*;
use unicode_width::UnicodeWidthStr;

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
    let rows = nodes.into_iter().map(|node| {
        let (status, last_seen) = match node.online {
            true => ("online", "now".to_string()),
            false => ("offline", ago(node.last_seen)),
        };
        [
            short_id(&node.id).into(),
            node.name,
            status.into(),
            node.hostname,
            format!("{}/{}", node.os, node.arch),
            commandant_common::or(&node.harnesses.join(","), "-").into(),
            last_seen,
        ]
    });
    let header = [
        "ID",
        "NAME",
        "STATUS",
        "HOSTNAME",
        "PLATFORM",
        "HARNESS",
        "LAST SEEN",
    ];
    table(header, rows);
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
    let rows = sessions.into_iter().map(|session| {
        let status = if session.busy { "running" } else { "idle" };
        let updated = ago(session.updated);
        [
            session.id,
            updated,
            status.into(),
            session.directory,
            session.title,
        ]
    });
    table(["ID", "UPDATED", "STATUS", "DIRECTORY", "TITLE"], rows);
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
    let rows = commands
        .into_iter()
        .map(|c| [c.name, c.source, c.description]);
    table(["NAME", "SOURCE", "DESCRIPTION"], rows);
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
    let rows = options
        .mcp_servers
        .into_iter()
        .map(|s| [s.name, s.status, s.error]);
    table(["NAME", "STATUS", "ERROR"], rows);
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
    let rows = tasks.into_iter().map(|task| {
        [
            short_id(&task.id).into(),
            task.node_name,
            task.status,
            task.exit_code.map_or("-".into(), |code| code.to_string()),
            ago(task.created_at),
            task.argv.join(" "),
        ]
    });
    table(["ID", "NODE", "STATUS", "EXIT", "STARTED", "COMMAND"], rows);
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

/// Prints `rows` under `header`, as [`aligned`].
fn table<const N: usize>(header: [&str; N], rows: impl IntoIterator<Item = [String; N]>) {
    print!("{}", aligned(header, rows));
}

/// `rows` under `header`, one line each, every column as wide as its widest
/// cell but the last, which is left as long as it is.
fn aligned<const N: usize>(
    header: [&str; N],
    rows: impl IntoIterator<Item = [String; N]>,
) -> String {
    let rows: Vec<[String; N]> = std::iter::once(header.map(String::from))
        .chain(rows)
        .collect();
    let widths: [usize; N] =
        std::array::from_fn(|i| rows.iter().map(|r| r[i].width()).max().unwrap_or_default());
    let mut text = String::new();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            text += cell;
            if i + 1 < N {
                text += &" ".repeat(widths[i] - cell.width() + 1);
            }
        }
        text += "\n";
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_line_up_on_their_widest_cell() {
        let rows = [
            ["a1".to_string(), "box".into(), "online".into()],
            ["b2".into(), "a-much-longer-name".into(), "-".into()],
        ];
        assert_eq!(
            aligned(["ID", "NAME", "STATUS"], rows),
            "ID NAME               STATUS\n\
             a1 box                online\n\
             b2 a-much-longer-name -\n"
        );
        assert_eq!(aligned(["ID", "NAME"], []), "ID NAME\n");
    }
}
