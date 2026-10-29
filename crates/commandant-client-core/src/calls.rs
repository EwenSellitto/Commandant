//! The calls to the server that effects ask for, made in the background;
//! what they come to is sent back as updates.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Result;
use commandant_common::link::Link;
use commandant_proto::*;
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinSet};

use crate::config::Client;
use crate::state::chat::Message;
use crate::state::tasks::{LIMIT, OutputId};
use crate::state::{Effect, Failure, Update};

/// How often the nodes' details are refreshed.
const NODE_REFRESH: Duration = Duration::from_secs(5);

type Tx = mpsc::UnboundedSender<Update>;

/// The server, as a core calls it.
pub(crate) struct Server {
    control: ControlClient,
    /// Where the server is, as logged in to: what a new node's link names.
    addr: String,
    /// Where calls are made.
    handle: Handle,
    /// Where what they come to goes.
    tx: Tx,
    /// Keeps the nodes' details current.
    poll: AbortHandle,
    /// What follows each output view's task.
    watches: Mutex<HashMap<OutputId, AbortHandle>>,
}

impl Server {
    /// Connects, on `handle`, and lists the nodes; what calls come to will
    /// arrive on the receiver.
    pub(crate) async fn connect(
        client: &Client,
        handle: Handle,
    ) -> Result<(Self, Vec<NodeInfo>, mpsc::UnboundedReceiver<Update>)> {
        let addr = client.addr.clone();
        let client = client.clone();
        // On the handle, so this can be awaited without a tokio runtime.
        let connect = handle.spawn(async move {
            let mut control = client.connect().await?;
            let nodes = control.list_nodes(ListNodesRequest {}).await?;
            anyhow::Ok((control, nodes.into_inner().nodes))
        });
        let (control, nodes) = connect.await??;
        let (tx, rx) = mpsc::unbounded_channel();
        let poll = handle
            .spawn(poll_nodes(control.clone(), tx.clone()))
            .abort_handle();
        let server = Self {
            control,
            addr,
            handle,
            tx,
            poll,
            watches: Mutex::default(),
        };
        Ok((server, nodes, rx))
    }

    /// Stops polling, and cancels `task_ids`, waiting for the server to say so.
    pub(crate) async fn shutdown(&self, task_ids: Vec<String>) {
        self.poll.abort();
        let _ = self
            .handle
            .spawn(cancel_all(self.control.clone(), task_ids))
            .await;
    }

    /// Keeps what follows an output view's task, to stop it when the view
    /// closes; those of tasks that have ended go.
    fn watched(&self, output: OutputId, watch: AbortHandle) {
        let mut watches = self.watches.lock().unwrap();
        watches.retain(|_, w| !w.is_finished());
        watches.insert(output, watch);
    }

    /// Starts the call an effect asks for.
    pub(crate) fn call(&self, effect: Effect) {
        let (mut control, handle, tx) = (self.control.clone(), &self.handle, self.tx.clone());
        // A chat's turn: its events, or why it broke, go to the chat.
        let to_chat = |chat, tx: Tx| {
            move |event: Result<_, _>| {
                let message = event.map_or_else(Message::Failed, Message::Task);
                tx.send(Update::Chat(chat, message)).is_ok()
            }
        };
        let to_output =
            |output, tx: Tx| move |event| tx.send(Update::Output(output, event)).is_ok();
        match effect {
            Effect::Send(chat, request) => {
                let call = async move { control.prompt(request).await };
                handle.spawn(stream_task(call, to_chat(chat, tx)));
            }
            Effect::WatchTurn { chat, task_id } => {
                let call = async move { control.watch_task(WatchTaskRequest { task_id }).await };
                handle.spawn(stream_task(call, to_chat(chat, tx)));
            }
            Effect::RunCommand { chat, run, request } => {
                let call = async move { control.run_command(request).await };
                handle.spawn(stream_task(call, move |event| {
                    tx.send(Update::Chat(chat, Message::Run(run, event)))
                        .is_ok()
                }));
            }
            Effect::Watch { output, task_id } => {
                let call = async move { control.watch_task(WatchTaskRequest { task_id }).await };
                let watch = handle.spawn(stream_task(call, to_output(output, tx)));
                self.watched(output, watch.abort_handle());
            }
            Effect::RunOn { output, request } => {
                let call = async move { control.run_command(request).await };
                let run = handle.spawn(stream_task(call, to_output(output, tx)));
                self.watched(output, run.abort_handle());
            }
            Effect::StopWatching(output) => {
                let watch = self.watches.lock().unwrap().remove(&output);
                if let Some(watch) = watch {
                    watch.abort();
                }
            }
            Effect::CancelTask(task_id) => {
                let request = CancelTaskRequest {
                    task_id: task_id.clone(),
                };
                let call = async move { control.cancel_task(request).await };
                spawn_ask(handle, tx, call, |cancelled| {
                    let error = cancelled.err()?;
                    Some(Update::CancelFailed { task_id, error })
                });
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
            Effect::Cancel(chat, task_id) => {
                let request = CancelTaskRequest {
                    task_id: task_id.clone(),
                };
                let call = async move { control.cancel_task(request).await };
                spawn_ask(handle, tx, call, move |cancelled| {
                    let error = cancelled.err()?;
                    Some(Update::Chat(chat, Message::CancelFailed { task_id, error }))
                });
            }
            Effect::FetchTasks { node, after, list } => {
                let request = ListTasksRequest {
                    limit: LIMIT,
                    node: node.unwrap_or_default(),
                };
                let call = async move {
                    tokio::time::sleep(after).await;
                    control.list_tasks(request).await
                };
                spawn_ask(handle, tx, call, move |listed| Update::Tasks {
                    list,
                    tasks: listed.map(|l| l.tasks),
                });
            }
            Effect::CreateToken { ttl } => {
                let addr = self.addr.clone();
                let request = CreateJoinTokenRequest {
                    ttl_secs: ttl.as_secs(),
                    reusable: false,
                };
                let call = async move { control.create_join_token(request).await };
                spawn_ask(handle, tx, call, move |created| Update::TokenCreated {
                    ttl,
                    link: created.map(|c| Link::for_addr(&c.token, &addr).to_string()),
                });
            }
            Effect::RemoveNode(node) => {
                let request = RemoveNodeRequest { node: node.clone() };
                let call = async move { control.remove_node(request).await };
                spawn_ask(handle, tx, call, |removed| {
                    Update::NodeRemoved(node, removed.map(|_| ()))
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
}

impl Drop for Server {
    fn drop(&mut self) {
        self.poll.abort();
    }
}

/// Makes a unary call in the background, and sends what `update` makes of
/// its answer, or of what went wrong, if anything.
fn spawn_ask<T: Send + 'static, U: Into<Option<Update>>>(
    handle: &Handle,
    tx: Tx,
    call: impl Future<Output = Result<tonic::Response<T>, tonic::Status>> + Send + 'static,
    update: impl FnOnce(Result<T, String>) -> U + Send + 'static,
) {
    handle.spawn(async move {
        let answer = call
            .await
            .map(tonic::Response::into_inner)
            .map_err(|status| status.message().to_string());
        if let Some(update) = update(answer).into() {
            let _ = tx.send(update);
        }
    });
}

/// Starts a task with `call` and relays its events to `send` until it
/// finishes, or why it couldn't start or its stream broke; `send` says
/// whether anyone still listens.
async fn stream_task(
    call: impl Future<Output = Result<tonic::Response<tonic::Streaming<TaskEvent>>, tonic::Status>>,
    send: impl Fn(Result<task_event::Event, String>) -> bool,
) {
    let mut events = match call.await {
        Ok(response) => response.into_inner(),
        Err(status) => {
            send(Err(status.message().to_string()));
            return;
        }
    };
    loop {
        match events.message().await {
            Ok(Some(TaskEvent { event: Some(event) })) => {
                let finished = matches!(event, task_event::Event::Finished(_));
                if !send(Ok(event)) || finished {
                    return;
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(status) => {
                send(Err(status.message().to_string()));
                return;
            }
        }
    }
    send(Err("the stream ended before the task finished".into()));
}

/// Keeps the nodes' details (online, harnesses) current.
async fn poll_nodes(mut control: ControlClient, tx: Tx) {
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
async fn cancel_all(control: ControlClient, task_ids: Vec<String>) {
    let cancels: JoinSet<_> = task_ids
        .into_iter()
        .map(|task_id| {
            let mut control = control.clone();
            async move { control.cancel_task(CancelTaskRequest { task_id }).await }
        })
        .collect();
    cancels.join_all().await;
}
