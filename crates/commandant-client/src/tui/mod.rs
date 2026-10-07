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
use tokio::task::JoinSet;
use tokio_stream::StreamExt;

use self::app::App;
use crate::cli::TuiArgs;
use crate::config::Client;
use crate::state::chat::{Message, Settings};
use crate::state::{ChatId, Effect, Failure, Update};

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
        for effect in app.open_node_with(node, settings) {
            perform(effect, &mut app, &mut control, &tx).await;
        }
    }
    let mut terminal = ratatui::init();
    execute!(stdout(), EnableBracketedPaste)?;
    let result = event_loop(&mut terminal, &mut app, &mut control, tx, rx).await;
    execute!(stdout(), DisableBracketedPaste)?;
    ratatui::restore();
    watcher.abort();
    result?;

    for chat in &app.state.chats {
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
        let mut effects = tokio::select! {
            event = input.next() => match event {
                Some(event) => app.on_input(event?),
                None => {
                    app.quit = true;
                    Vec::new()
                }
            },
            Some(update) = rx.recv() => app.on_update(update),
            // Only the spinners need redrawing on their own.
            _ = tick.tick(), if app.state.busy() => Vec::new(),
        };
        // A reply streams in many small pieces: take what has come before
        // drawing again, or before quitting, so a task that has just
        // started is known and cancelled.
        while let Ok(update) = rx.try_recv() {
            effects.extend(app.on_update(update));
        }
        if app.quit {
            // Don't leave agents working for nobody.
            let cancels: JoinSet<_> = app
                .state
                .running_tasks()
                .into_iter()
                .map(|task_id| {
                    let mut control = control.clone();
                    async move { control.cancel_task(CancelTaskRequest { task_id }).await }
                })
                .collect();
            cancels.join_all().await;
            return Ok(());
        }
        for effect in effects {
            perform(effect, app, control, &tx).await;
        }
    }
}

/// Starts the call an effect asks for; answers come back as updates.
async fn perform(
    effect: Effect,
    app: &mut App,
    control: &mut ControlClient,
    tx: &mpsc::UnboundedSender<Update>,
) {
    let mut control_ = control.clone();
    let tx_ = tx.clone();
    match effect {
        Effect::Send(chat, request) => {
            tokio::spawn(stream_prompt(control_, chat, request, tx_));
        }
        Effect::FetchOptions { node, after } => {
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
        Effect::SwitchMcp {
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
        Effect::FetchSessions(node) => {
            let request = ListAgentSessionsRequest { node: node.clone() };
            let call = async move { control_.list_agent_sessions(request).await };
            spawn_ask(tx, call, |sessions| {
                Update::Sessions(node, sessions.map(|s| s.sessions))
            });
        }
        Effect::FetchHistory {
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
        Effect::PrepareProject {
            chat,
            node,
            repository,
        } => {
            let request = PrepareProjectRequest { node, repository };
            let call = async move { control_.prepare_project(request).await };
            spawn_ask(tx, call, move |ready| {
                Update::Chat(chat, Message::Project(ready))
            });
        }
        Effect::FetchProjects { chat, node } => {
            let request = ListProjectsRequest { node };
            let call = async move { control_.list_projects(request).await };
            spawn_ask(tx, call, move |projects| {
                Update::Chat(chat, Message::Projects(projects.map(|p| p.projects)))
            });
        }
        Effect::FetchProviders { chat, node } => {
            let request = ListProvidersRequest { node };
            let call = async move { control_.list_providers(request).await };
            spawn_ask(tx, call, move |providers| {
                Update::Chat(chat, Message::Providers(providers.map(|p| p.providers)))
            });
        }
        Effect::Authenticate {
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
        Effect::Cancel(task_id) => {
            let chat = app
                .state
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
        Effect::StartHarness { node, harness } => {
            let request = StartHarnessRequest {
                node: node.clone(),
                harness,
            };
            let call = async move { control_.start_harness(request).await };
            spawn_ask(tx, call, |started| Update::HarnessStarted(node, started));
        }
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
    match lookup::find_named(nodes.to_vec(), needle, |n| &n.name, |n| &n.id) {
        Match::One(node) => Ok(node),
        Match::Ambiguous => bail!("{needle:?} is ambiguous"),
        Match::None => bail!("no node matches {needle:?}"),
    }
}
