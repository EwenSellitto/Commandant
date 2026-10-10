//! The calls to the server that effects ask for, made in the background;
//! what they come to is sent back as updates.

use std::time::Duration;

use commandant_proto::*;
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::state::chat::Message;
use crate::state::{ChatId, Effect, Failure, Update};

/// How often the nodes' details are refreshed.
const NODE_REFRESH: Duration = Duration::from_secs(5);

type Tx = mpsc::UnboundedSender<Update>;

/// Starts the call an effect asks for, on `handle`. A cancel that fails is
/// told to `chat`, if it was that chat's task.
pub(crate) fn make(
    effect: Effect,
    chat: Option<ChatId>,
    mut control: ControlClient,
    handle: &Handle,
    tx: Tx,
) {
    match effect {
        Effect::Send(chat, request) => {
            handle.spawn(stream_prompt(control, chat, request, tx));
        }
        Effect::FetchOptions { node, after } => {
            handle.spawn(async move {
                tokio::time::sleep(after).await;
                let request = GetAgentOptionsRequest { node: node.clone() };
                let options = control
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
                let _ = tx.send(Update::Options(node, options));
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
            let call = async move { control.switch_mcp_server(request).await };
            spawn_ask(handle, tx, call, move |options| Update::McpSwitched {
                chat,
                node,
                name,
                options,
            });
        }
        Effect::FetchSessions(node) => {
            let request = ListAgentSessionsRequest { node: node.clone() };
            let call = async move { control.list_agent_sessions(request).await };
            spawn_ask(handle, tx, call, |sessions| {
                Update::Sessions(node, sessions.map(|s| s.sessions))
            });
        }
        Effect::FetchHistory {
            chat,
            node,
            session_id,
        } => {
            let request = GetSessionHistoryRequest { node, session_id };
            let call = async move { control.get_session_history(request).await };
            spawn_ask(handle, tx, call, move |history| {
                Update::Chat(chat, Message::History(history.map(|h| h.entries)))
            });
        }
        Effect::PrepareProject {
            chat,
            node,
            repository,
        } => {
            let request = PrepareProjectRequest { node, repository };
            let call = async move { control.prepare_project(request).await };
            spawn_ask(handle, tx, call, move |ready| {
                Update::Chat(chat, Message::Project(ready))
            });
        }
        Effect::FetchProjects { chat, node } => {
            let request = ListProjectsRequest { node };
            let call = async move { control.list_projects(request).await };
            spawn_ask(handle, tx, call, move |projects| {
                Update::Chat(chat, Message::Projects(projects.map(|p| p.projects)))
            });
        }
        Effect::FetchProviders { chat, node } => {
            let request = ListProvidersRequest { node };
            let call = async move { control.list_providers(request).await };
            spawn_ask(handle, tx, call, move |providers| {
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
            let call = async move { control.authenticate_provider(request).await };
            spawn_ask(handle, tx, call, move |result| {
                Update::Chat(chat, Message::Auth(result))
            });
        }
        Effect::Cancel(task_id) => {
            handle.spawn(async move {
                if let Err(status) = control.cancel_task(CancelTaskRequest { task_id }).await
                    && let Some(chat) = chat
                {
                    let failed = Message::Failed(status.message().to_string());
                    let _ = tx.send(Update::Chat(chat, failed));
                }
            });
        }
        Effect::StartHarness { node, harness } => {
            let request = StartHarnessRequest {
                node: node.clone(),
                harness,
            };
            let call = async move { control.start_harness(request).await };
            spawn_ask(handle, tx, call, |started| {
                Update::HarnessStarted(node, started)
            });
        }
    }
}

/// Makes a unary call in the background, and sends what `update` makes of
/// its answer, or of what went wrong.
fn spawn_ask<T: Send + 'static>(
    handle: &Handle,
    tx: Tx,
    call: impl Future<Output = Result<tonic::Response<T>, tonic::Status>> + Send + 'static,
    update: impl FnOnce(Result<T, String>) -> Update + Send + 'static,
) {
    handle.spawn(async move {
        let answer = call
            .await
            .map(tonic::Response::into_inner)
            .map_err(|status| status.message().to_string());
        let _ = tx.send(update(answer));
    });
}

/// Sends a prompt and relays its events to its chat.
async fn stream_prompt(mut control: ControlClient, chat: ChatId, request: PromptRequest, tx: Tx) {
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
pub(crate) async fn poll_nodes(mut control: ControlClient, tx: Tx) {
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

/// Cancels every task in `task_ids`, and waits for the server to say so.
pub(crate) async fn cancel_all(control: ControlClient, task_ids: Vec<String>) {
    let cancels: JoinSet<_> = task_ids
        .into_iter()
        .map(|task_id| {
            let mut control = control.clone();
            async move { control.cancel_task(CancelTaskRequest { task_id }).await }
        })
        .collect();
    cancels.join_all().await;
}
