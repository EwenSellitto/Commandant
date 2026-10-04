//! `commandant tui`: chat with a node's coding agent in a terminal UI.
//!
//! It opens on the list of nodes. Each node can have several chats, each its
//! own agent session, and chats on any node work at the same time. Each
//! prompt is an ordinary prompt task; a chat keeps its session id from one
//! reply to the next so the conversation continues.

mod app;
mod chat;
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

use self::app::{Action, App, ChatId, Update};
use self::chat::{Message, Settings};
use crate::cli::TuiArgs;
use crate::config::Client;

/// How often the screen refreshes on its own, for the busy spinner.
const TICK: Duration = Duration::from_millis(100);
/// How often the nodes' details are refreshed.
const NODE_REFRESH: Duration = Duration::from_secs(5);
/// How long to wait before asking again for commands that were loading.
const OPTIONS_RETRY: Duration = Duration::from_secs(3);

pub async fn run(client: &Client, args: TuiArgs) -> Result<()> {
    let mut control = client.connect().await?;
    let nodes = control
        .list_nodes(ListNodesRequest {})
        .await?
        .into_inner()
        .nodes;
    let settings = Settings {
        session_id: String::new(),
        cwd: args.cwd.unwrap_or_default(),
        model: args.model.unwrap_or_default(),
        agent: args.agent.unwrap_or_default(),
        effort: args.effort.unwrap_or_default(),
    };
    let first = match (args.node.as_deref(), args.session) {
        (Some(needle), session) => Some((find_node(&nodes, needle)?, session.unwrap_or_default())),
        (None, Some(_)) => bail!("--session needs the node it is on"),
        (None, None) => None,
    };
    let mut app = App::new(nodes, settings.clone());

    let (tx, rx) = mpsc::unbounded_channel();
    let watcher = tokio::spawn(watch_nodes(control.clone(), tx.clone()));
    if let Some((node, session_id)) = first {
        let settings = Settings {
            session_id,
            ..settings
        };
        for action in app.open_node_with(node, settings) {
            perform(action, &mut app, &mut control, &tx).await;
        }
    }
    let mut terminal = ratatui::init();
    execute!(stdout(), EnableBracketedPaste)?;
    let result = event_loop(&mut terminal, &mut app, &mut control, tx, rx).await;
    execute!(stdout(), DisableBracketedPaste)?;
    ratatui::restore();
    watcher.abort();
    result?;

    for chat in &app.chats {
        if !chat.settings.session_id.is_empty() {
            eprintln!(
                "commandant: continue with: commandant tui {} -s {}",
                chat.node.name, chat.settings.session_id
            );
        }
    }
    Ok(())
}

async fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    control: &mut ControlClient,
    tx: mpsc::UnboundedSender<Update>,
    mut rx: mpsc::UnboundedReceiver<Update>,
) -> Result<()> {
    let mut input = EventStream::new();
    let mut tick = tokio::time::interval(TICK);
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;
        let actions = tokio::select! {
            event = input.next() => match event {
                Some(event) => app.on_input(event?),
                None => vec![Action::Quit],
            },
            Some(update) = rx.recv() => app.on_update(update),
            _ = tick.tick() => Vec::new(),
        };
        for action in actions {
            if let Action::Quit = action {
                // Don't leave agents working for nobody.
                for task_id in app.running_tasks() {
                    let _ = control.cancel_task(CancelTaskRequest { task_id }).await;
                }
                return Ok(());
            }
            perform(action, app, control, &tx).await;
        }
    }
}

/// Starts what an action asks for; answers come back as updates.
async fn perform(
    action: Action,
    app: &mut App,
    control: &mut ControlClient,
    tx: &mpsc::UnboundedSender<Update>,
) {
    let control_ = control.clone();
    let tx_ = tx.clone();
    let later = matches!(action, Action::FetchOptionsLater(_));
    match action {
        Action::Send(chat, request) => {
            tokio::spawn(stream_prompt(control_, chat, request, tx_));
        }
        Action::FetchOptions(node) | Action::FetchOptionsLater(node) => {
            let wait = match later {
                true => OPTIONS_RETRY,
                false => Duration::ZERO,
            };
            tokio::spawn(async move {
                tokio::time::sleep(wait).await;
                let options = ask(control_
                    .clone()
                    .get_agent_options(GetAgentOptionsRequest { node: node.clone() }))
                .await;
                let _ = tx_.send(Update::Options(node, options));
            });
        }
        Action::SwitchMcp {
            node,
            name,
            connect,
        } => {
            tokio::spawn(async move {
                let request = SwitchMcpServerRequest {
                    node: node.clone(),
                    name,
                    connect,
                };
                let options = ask(control_.clone().switch_mcp_server(request)).await;
                let _ = tx_.send(Update::Options(node, options));
            });
        }
        Action::FetchSessions(node) => {
            tokio::spawn(async move {
                let request = ListAgentSessionsRequest { node: node.clone() };
                let sessions = ask(control_.clone().list_agent_sessions(request))
                    .await
                    .map(|s| s.sessions);
                let _ = tx_.send(Update::Sessions(node, sessions));
            });
        }
        Action::Cancel(task_id) => {
            if let Err(status) = control.cancel_task(CancelTaskRequest { task_id }).await
                && let Some(chat) = app.chat().map(|c| c.id)
            {
                let failed = Message::Failed(status.message().to_string());
                app.on_update(Update::Chat(chat, failed));
            }
        }
        Action::StartHarness { node, harness } => {
            tokio::spawn(async move {
                let request = StartHarnessRequest {
                    node: node.clone(),
                    harness,
                };
                let started = ask(control_.clone().start_harness(request)).await;
                let _ = tx_.send(Update::HarnessStarted(node, started));
            });
        }
        // The app's own, or quitting, which the loop does.
        Action::NewChat
        | Action::ShowSessions
        | Action::ShowNodes
        | Action::CloseChat
        | Action::Quit => {}
    }
}

/// A unary call's answer, or what went wrong.
async fn ask<T>(
    call: impl Future<Output = Result<tonic::Response<T>, tonic::Status>>,
) -> Result<T, String> {
    call.await
        .map(tonic::Response::into_inner)
        .map_err(|status| status.message().to_string())
}

/// Sends a prompt and relays its events to its chat.
async fn stream_prompt(
    mut control: ControlClient,
    chat: ChatId,
    request: PromptRequest,
    tx: mpsc::UnboundedSender<Update>,
) {
    let send = |message| tx.send(Update::Chat(chat, message)).is_ok();
    let mut events = match control.prompt(request).await {
        Ok(response) => response.into_inner(),
        Err(status) => {
            send(Message::Failed(status.message().to_string()));
            return;
        }
    };
    loop {
        match events.message().await {
            Ok(Some(TaskEvent { event: Some(event) })) => {
                let finished = matches!(event, task_event::Event::Finished(_));
                if !send(Message::Task(event)) || finished {
                    return;
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(status) => {
                send(Message::Failed(status.message().to_string()));
                return;
            }
        }
    }
    send(Message::Failed(
        "the stream ended before the agent finished".into(),
    ));
}

/// Keeps the nodes' details (online, harnesses) current.
async fn watch_nodes(mut control: ControlClient, tx: mpsc::UnboundedSender<Update>) {
    let mut every = tokio::time::interval(NODE_REFRESH);
    every.tick().await;
    loop {
        every.tick().await;
        let Ok(nodes) = control.list_nodes(ListNodesRequest {}).await else {
            continue;
        };
        if tx.send(Update::Nodes(nodes.into_inner().nodes)).is_err() {
            return;
        }
    }
}

/// The node named by `needle`: a name, an id or an id prefix.
fn find_node(nodes: &[NodeInfo], needle: &str) -> Result<NodeInfo> {
    if let Some(node) = nodes.iter().find(|n| n.name == needle) {
        return Ok(node.clone());
    }
    match lookup::find(nodes.to_vec(), needle, |n| &n.id) {
        Match::One(node) => Ok(node),
        Match::Ambiguous => bail!("{needle:?} is ambiguous"),
        Match::None => bail!("no node matches {needle:?}"),
    }
}
