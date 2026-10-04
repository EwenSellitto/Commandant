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

use self::app::{Action, App, ChatId, Failure, Update};
use self::chat::{Message, Settings};
use crate::cli::TuiArgs;
use crate::config::Client;

/// How often the screen refreshes on its own, for the busy spinner.
const TICK: Duration = Duration::from_millis(100);
/// How often the nodes' details are refreshed.
const NODE_REFRESH: Duration = Duration::from_secs(5);

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
        let mut actions = tokio::select! {
            event = input.next() => match event {
                Some(event) => app.on_input(event?),
                None => vec![Action::Quit],
            },
            Some(update) = rx.recv() => app.on_update(update),
            // Only the spinners need redrawing on their own.
            _ = tick.tick(), if app.busy() => Vec::new(),
        };
        // A reply streams in many small pieces: take what has come before
        // drawing again.
        while let Ok(update) = rx.try_recv() {
            actions.extend(app.on_update(update));
        }
        for action in actions {
            if let Action::Quit = action {
                // Don't leave agents working for nobody.
                let cancels = app.running_tasks().into_iter().map(|task_id| {
                    let mut control = control.clone();
                    async move { control.cancel_task(CancelTaskRequest { task_id }).await }
                });
                futures_join_all(cancels).await;
                return Ok(());
            }
            perform(action, app, control, &tx).await;
        }
    }
}

/// Runs the futures at once and waits for them all.
async fn futures_join_all<F: Future + Send + 'static>(futures: impl Iterator<Item = F>)
where
    F::Output: Send,
{
    let mut set = tokio::task::JoinSet::new();
    for future in futures {
        set.spawn(future);
    }
    while set.join_next().await.is_some() {}
}

/// Starts what an action asks for; answers come back as updates.
async fn perform(
    action: Action,
    app: &mut App,
    control: &mut ControlClient,
    tx: &mpsc::UnboundedSender<Update>,
) {
    let mut control_ = control.clone();
    let tx_ = tx.clone();
    match action {
        Action::Send(chat, request) => {
            tokio::spawn(stream_prompt(control_, chat, request, tx_));
        }
        Action::FetchOptions { node, after } => {
            tokio::spawn(async move {
                tokio::time::sleep(after).await;
                let request = GetAgentOptionsRequest { node: node.clone() };
                let options = control_
                    .get_agent_options(request)
                    .await
                    .map(tonic::Response::into_inner)
                    .map_err(|status| Failure {
                        message: status.message().to_string(),
                        transient: matches!(
                            status.code(),
                            tonic::Code::DeadlineExceeded | tonic::Code::Unavailable
                        ),
                    });
                let _ = tx_.send(Update::Options(node, options));
            });
        }
        Action::SwitchMcp {
            chat,
            node,
            name,
            connect,
        } => {
            let request = SwitchMcpServerRequest {
                node: node.clone(),
                name: name.clone(),
                connect,
            };
            let call = async move { control_.switch_mcp_server(request).await };
            spawn_ask(tx, call, move |options| Update::McpSwitched {
                chat,
                node,
                name,
                options,
            });
        }
        Action::FetchSessions(node) => {
            let request = ListAgentSessionsRequest { node: node.clone() };
            let call = async move { control_.list_agent_sessions(request).await };
            spawn_ask(tx, call, |sessions| {
                Update::Sessions(node, sessions.map(|s| s.sessions))
            });
        }
        Action::FetchHistory {
            chat,
            node,
            session_id,
        } => {
            let request = GetSessionHistoryRequest { node, session_id };
            let call = async move { control_.get_session_history(request).await };
            spawn_ask(tx, call, move |history| {
                Update::Chat(chat, Message::History(history.map(|h| h.entries)))
            });
        }
        Action::FetchProviders { chat, node } => {
            let request = ListProvidersRequest { node };
            let call = async move { control_.list_providers(request).await };
            spawn_ask(tx, call, move |providers| {
                Update::Chat(chat, Message::Providers(providers.map(|p| p.providers)))
            });
        }
        Action::Authenticate {
            chat,
            node,
            provider,
            action,
        } => {
            let request = AuthenticateProviderRequest {
                node,
                provider,
                action: Some(action),
            };
            let call = async move { control_.authenticate_provider(request).await };
            spawn_ask(tx, call, move |result| {
                Update::Chat(chat, Message::Auth(result))
            });
        }
        Action::Cancel(task_id) => {
            let chat = app
                .chats
                .iter()
                .find(|c| c.running_task().as_deref() == Some(task_id.as_str()))
                .map(|c| c.id);
            if let Err(status) = control.cancel_task(CancelTaskRequest { task_id }).await
                && let Some(chat) = chat
            {
                let failed = Message::Failed(status.message().to_string());
                app.on_update(Update::Chat(chat, failed));
            }
        }
        Action::StartHarness { node, harness } => {
            let request = StartHarnessRequest {
                node: node.clone(),
                harness,
            };
            let call = async move { control_.start_harness(request).await };
            spawn_ask(tx, call, |started| Update::HarnessStarted(node, started));
        }
        // The app's own, or quitting, which the loop does.
        Action::NewChat
        | Action::ShowSessions
        | Action::ShowNodes
        | Action::CloseChat
        | Action::Quit => {}
    }
}

/// Makes a unary call in the background, and sends what `update` makes of
/// its answer, or of what went wrong.
fn spawn_ask<T: Send + 'static>(
    tx: &mpsc::UnboundedSender<Update>,
    call: impl Future<Output = Result<tonic::Response<T>, tonic::Status>> + Send + 'static,
    update: impl FnOnce(Result<T, String>) -> Update + Send + 'static,
) {
    let tx = tx.clone();
    tokio::spawn(async move {
        let answer = call
            .await
            .map(tonic::Response::into_inner)
            .map_err(|status| status.message().to_string());
        let _ = tx.send(update(answer));
    });
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
