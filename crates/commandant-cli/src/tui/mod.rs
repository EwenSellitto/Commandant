//! `commandant tui`: chat with a node's coding agent in a terminal UI.
//!
//! Each prompt is an ordinary prompt task; the UI keeps the session id from
//! one reply to the next so the conversation continues.

mod app;
mod picker;
mod text;
mod ui;

use std::io::stdout;
use std::time::Duration;

use anyhow::{Result, bail};
use commandant_common::lookup::{self, Match};
use commandant_proto::*;
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste, EventStream};
use crossterm::execute;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;

use self::app::{Action, App, Message, Settings};
use crate::cli::TuiArgs;
use crate::config::Client;

/// How often the screen refreshes on its own, for the busy spinner.
const TICK: Duration = Duration::from_millis(100);
/// How often the node's details are refreshed.
const NODE_REFRESH: Duration = Duration::from_secs(5);

pub async fn run(client: &Client, args: TuiArgs) -> Result<()> {
    let mut control = client.connect().await?;
    let node = pick_node(&mut control, args.node.as_deref()).await?;
    let mut app = App::new(
        node,
        Settings {
            session_id: args.session.unwrap_or_default(),
            cwd: args.cwd.unwrap_or_default(),
            model: args.model.unwrap_or_default(),
            agent: args.agent.unwrap_or_default(),
            effort: args.effort.unwrap_or_default(),
        },
    );

    let (tx, rx) = mpsc::unbounded_channel();
    let watcher = tokio::spawn(watch_node(control.clone(), app.node.id.clone(), tx.clone()));
    tokio::spawn(fetch_options(
        control.clone(),
        app.node.id.clone(),
        tx.clone(),
    ));
    let mut terminal = ratatui::init();
    execute!(stdout(), EnableBracketedPaste)?;
    let result = event_loop(&mut terminal, &mut app, &mut control, tx, rx).await;
    execute!(stdout(), DisableBracketedPaste)?;
    ratatui::restore();
    watcher.abort();
    result?;

    if !app.settings.session_id.is_empty() {
        eprintln!(
            "commandant: continue with --session {}",
            app.settings.session_id
        );
    }
    Ok(())
}

async fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    control: &mut ControlClient,
    tx: mpsc::UnboundedSender<Message>,
    mut rx: mpsc::UnboundedReceiver<Message>,
) -> Result<()> {
    let mut input = EventStream::new();
    let mut tick = tokio::time::interval(TICK);
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;
        let action = tokio::select! {
            event = input.next() => match event {
                Some(event) => app.on_input(event?),
                None => Some(Action::Quit),
            },
            Some(message) = rx.recv() => app.on_message(message),
            _ = tick.tick() => None,
        };
        match action {
            Some(Action::Send(request)) => {
                tokio::spawn(stream_prompt(control.clone(), request, tx.clone()));
            }
            Some(Action::FetchOptions) => {
                tokio::spawn(fetch_options(
                    control.clone(),
                    app.node.id.clone(),
                    tx.clone(),
                ));
            }
            Some(Action::SwitchMcp { name, connect }) => {
                tokio::spawn(switch_mcp(
                    control.clone(),
                    SwitchMcpServerRequest {
                        node: app.node.id.clone(),
                        name,
                        connect,
                    },
                    tx.clone(),
                ));
            }
            Some(Action::Cancel(task_id)) => {
                if let Err(status) = control.cancel_task(CancelTaskRequest { task_id }).await {
                    app.on_message(Message::Failed(status.message().to_string()));
                }
            }
            Some(Action::Quit) => {
                // Don't leave the agent working for nobody.
                if let Some(task_id) = app.running_task() {
                    let _ = control.cancel_task(CancelTaskRequest { task_id }).await;
                }
                return Ok(());
            }
            None => {}
        }
    }
}

/// Sends a prompt and relays its events to the UI.
async fn stream_prompt(
    mut control: ControlClient,
    request: PromptRequest,
    tx: mpsc::UnboundedSender<Message>,
) {
    let mut events = match control.prompt(request).await {
        Ok(response) => response.into_inner(),
        Err(status) => {
            let _ = tx.send(Message::Failed(status.message().to_string()));
            return;
        }
    };
    loop {
        match events.message().await {
            Ok(Some(TaskEvent { event: Some(event) })) => {
                let finished = matches!(event, task_event::Event::Finished(_));
                if tx.send(Message::Task(event)).is_err() || finished {
                    return;
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(status) => {
                let _ = tx.send(Message::Failed(status.message().to_string()));
                return;
            }
        }
    }
    let _ = tx.send(Message::Failed(
        "the stream ended before the agent finished".into(),
    ));
}

/// Asks the node which agents, models and efforts its harness offers.
async fn fetch_options(
    mut control: ControlClient,
    node: String,
    tx: mpsc::UnboundedSender<Message>,
) {
    let options = control
        .get_agent_options(GetAgentOptionsRequest { node })
        .await
        .map(tonic::Response::into_inner)
        .map_err(|status| status.message().to_string());
    let _ = tx.send(Message::Options(options));
}

/// Connects or disconnects an MCP server; the node answers with its options.
async fn switch_mcp(
    mut control: ControlClient,
    request: SwitchMcpServerRequest,
    tx: mpsc::UnboundedSender<Message>,
) {
    let options = control
        .switch_mcp_server(request)
        .await
        .map(tonic::Response::into_inner)
        .map_err(|status| status.message().to_string());
    let _ = tx.send(Message::Options(options));
}

/// Keeps the node's details (online, harnesses) current.
async fn watch_node(mut control: ControlClient, id: String, tx: mpsc::UnboundedSender<Message>) {
    let mut every = tokio::time::interval(NODE_REFRESH);
    loop {
        every.tick().await;
        let Ok(nodes) = control.list_nodes(ListNodesRequest {}).await else {
            continue;
        };
        let node = nodes.into_inner().nodes.into_iter().find(|n| n.id == id);
        if let Some(node) = node
            && tx.send(Message::Node(node)).is_err()
        {
            return;
        }
    }
}

/// The node named by `needle`, else the only online node hosting an agent.
async fn pick_node(control: &mut ControlClient, needle: Option<&str>) -> Result<NodeInfo> {
    let nodes = control
        .list_nodes(ListNodesRequest {})
        .await?
        .into_inner()
        .nodes;
    if let Some(needle) = needle {
        if let Some(node) = nodes.iter().find(|n| n.name == needle) {
            return Ok(node.clone());
        }
        return match lookup::find(nodes, needle, |n| &n.id) {
            Match::One(node) => Ok(node),
            Match::Ambiguous => bail!("{needle:?} is ambiguous"),
            Match::None => bail!("no node matches {needle:?}"),
        };
    }
    let mut agents: Vec<_> = nodes
        .into_iter()
        .filter(|n| n.online && !n.harnesses.is_empty())
        .collect();
    match agents.len() {
        0 => bail!("no online node runs an agent harness; start a worker with --harness opencode"),
        1 => Ok(agents.remove(0)),
        _ => {
            let names: Vec<_> = agents.iter().map(|n| n.name.as_str()).collect();
            bail!("several nodes run an agent, pick one: {}", names.join(", "))
        }
    }
}
