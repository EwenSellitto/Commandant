//! Control service: the admin API used by the CLI.

use std::sync::Arc;
use std::time::Duration;

use commandant_proto::control_server::Control;
use commandant_proto::*;
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use tracing::info;

use commandant_common::lookup::{self, Match};

use crate::auth::{JOIN_PREFIX, generate_token, hash_token};
use crate::registry::Connection;
use crate::store::{NodeRecord, now};
use crate::tasks::{Owner, Watch};
use crate::{Shared, internal};

const DEFAULT_TASK_LIMIT: u32 = 20;
const MAX_TASK_LIMIT: u32 = 1000;
/// How long a worker gets to answer a question.
const QUERY_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a worker gets to start a harness, which it may have to install.
const START_TIMEOUT: Duration = Duration::from_secs(600);

type EventTx = mpsc::Sender<Result<TaskEvent, Status>>;
type TaskStream = ReceiverStream<Result<TaskEvent, Status>>;

pub struct ControlService {
    shared: Arc<Shared>,
}

impl ControlService {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    /// Finds a node by exact name, exact id, or unambiguous id prefix.
    async fn resolve_node(&self, needle: &str) -> Result<NodeRecord, Status> {
        if needle.is_empty() {
            return Err(Status::invalid_argument("node is required"));
        }
        let nodes = self.shared.store.list_nodes().await.map_err(internal)?;
        let node = lookup::find_named(nodes, needle, |n| &n.name, |n| &n.id);
        matched_or_status(node, "node", needle)
    }

    /// Records a task, sends it to the node's worker, and streams its events
    /// back. `record` is what `task ls` shows; `message` builds what the
    /// worker receives from the new task id.
    async fn dispatch(
        &self,
        node: NodeRecord,
        conn: Connection,
        record: &[String],
        message: impl FnOnce(String) -> OrchestratorMsg,
    ) -> Result<Response<TaskStream>, Status> {
        let task_id = uuid::Uuid::new_v4().to_string();
        let (store, hub) = (&self.shared.store, &self.shared.hub);
        let owner = Owner {
            node_id: node.id.clone(),
            conn_id: conn.id,
        };
        // In the hub before the store, so a watcher never finds it in
        // neither, running but not followable.
        let events = hub.start(&task_id, owner);
        if let Err(e) = store.insert_task(&task_id, &node.id, record).await {
            hub.abandon(&task_id, &e.to_string());
            return Err(internal(e));
        }

        if conn.tx.send(Ok(message(task_id.clone()))).await.is_err() {
            let _ = store.lose_task(&task_id, &[]).await;
            let offline = offline(&node);
            hub.abandon(&task_id, offline.message());
            return Err(offline);
        }
        info!(%task_id, node = %node.name, "task dispatched");

        let (tx, rx) = mpsc::channel(256);
        let started = TaskStarted {
            task_id,
            node_id: node.id,
            ..Default::default()
        };
        let _ = tx.send(Ok(started.into())).await;
        tokio::spawn(relay(events, tx));
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    /// Resolves a node and its live connection.
    async fn connected_node(&self, needle: &str) -> Result<(NodeRecord, Connection), Status> {
        let node = self.resolve_node(needle).await?;
        let conn = self
            .shared
            .registry
            .get(&node.id)
            .ok_or_else(|| offline(&node))?;
        Ok((node, conn))
    }

    /// Asks a node's harness what it offers, after switching an MCP server
    /// if `mcp` is set.
    async fn ask_options(
        &self,
        needle: &str,
        mcp: Option<McpSwitch>,
    ) -> Result<Response<AgentOptions>, Status> {
        let ask = |request_id| ListAgentOptions { request_id, mcp }.into();
        self.ask_harness(needle, ask, "say what its agent offers", QUERY_TIMEOUT)
            .await
    }

    /// Puts a question to the harness of the node `needle` names, which must
    /// be online and host one.
    async fn ask_harness<T: Reply>(
        &self,
        needle: &str,
        question: impl FnOnce(String) -> OrchestratorMsg,
        doing: &str,
        wait: Duration,
    ) -> Result<Response<T>, Status> {
        let (node, conn) = self.connected_node(needle).await?;
        harness(&node, &conn)?;
        let answer = self.ask(&node, &conn, question, doing, wait).await?;
        Ok(Response::new(answer))
    }

    /// Puts a question to a node's worker and waits up to `wait` for its
    /// answer, which fails if it carries an error.
    async fn ask<T: Reply>(
        &self,
        node: &NodeRecord,
        conn: &Connection,
        question: impl FnOnce(String) -> OrchestratorMsg,
        doing: &str,
        wait: Duration,
    ) -> Result<T, Status> {
        let queries = &self.shared.queries;
        let (request_id, answer) = queries.open();
        if conn
            .tx
            .send(Ok(question(request_id.clone())))
            .await
            .is_err()
        {
            queries.close(&request_id);
            return Err(offline(node));
        }
        let answer = tokio::time::timeout(wait, answer).await;
        queries.close(&request_id);
        let Ok(Ok(answer)) = answer else {
            return Err(Status::deadline_exceeded(format!(
                "node {} didn't {doing}",
                node.name
            )));
        };
        let answer = T::try_from(answer)
            .map_err(|_| Status::internal("the node answered another question"))?;
        match answer.error() {
            "" => Ok(answer),
            error => Err(Status::unavailable(error)),
        }
    }

    /// A node as clients see it.
    fn node_info(&self, node: NodeRecord) -> NodeInfo {
        let connection = self.shared.registry.get(&node.id);
        let online = connection.is_some();
        let (harnesses, can_host) = connection
            .map(|c| (c.harnesses, c.can_host))
            .unwrap_or_default();
        NodeInfo {
            online,
            harnesses,
            can_host,
            id: node.id,
            name: node.name,
            hostname: node.hostname,
            os: node.os,
            arch: node.arch,
            version: node.version,
            last_seen: node.last_seen,
            created_at: node.created_at,
        }
    }
}

/// The node's agent harness; a prompt needs one.
fn harness(node: &NodeRecord, conn: &Connection) -> Result<String, Status> {
    conn.harnesses.first().cloned().ok_or_else(|| {
        Status::failed_precondition(format!(
            "node {} runs no agent harness; start one with `commandant node start-agent {} <harness>`",
            node.name, node.name
        ))
    })
}

/// The one `what` that `needle` matched, else the error saying why there is none.
fn matched_or_status<T>(found: Match<T>, what: &str, needle: &str) -> Result<T, Status> {
    match found {
        Match::One(item) => Ok(item),
        Match::Ambiguous => Err(Status::invalid_argument(format!("{needle:?} is ambiguous"))),
        Match::None => Err(Status::not_found(format!("no {what} matches {needle:?}"))),
    }
}

fn offline(node: &NodeRecord) -> Status {
    Status::unavailable(format!("node {} is offline", node.name))
}

#[tonic::async_trait]
impl Control for ControlService {
    async fn create_join_token(
        &self,
        request: Request<CreateJoinTokenRequest>,
    ) -> Result<Response<CreateJoinTokenResponse>, Status> {
        let req = request.into_inner();
        let token = generate_token(JOIN_PREFIX);
        let expires_at = (req.ttl_secs > 0).then(|| now() + req.ttl_secs as i64);
        self.shared
            .store
            .add_join_token(&hash_token(&token), expires_at, req.reusable)
            .await
            .map_err(internal)?;
        Ok(Response::new(CreateJoinTokenResponse { token, expires_at }))
    }

    async fn list_nodes(
        &self,
        _request: Request<ListNodesRequest>,
    ) -> Result<Response<ListNodesResponse>, Status> {
        let nodes = self.shared.store.list_nodes().await.map_err(internal)?;
        let nodes = nodes.into_iter().map(|n| self.node_info(n)).collect();
        Ok(Response::new(ListNodesResponse { nodes }))
    }

    async fn remove_node(
        &self,
        request: Request<RemoveNodeRequest>,
    ) -> Result<Response<RemoveNodeResponse>, Status> {
        let node = self.resolve_node(&request.into_inner().node).await?;
        self.shared
            .store
            .delete_node(&node.id)
            .await
            .map_err(internal)?;
        self.shared.registry.kick(&node.id, "this node was removed");
        info!(node_id = %node.id, name = %node.name, "node removed");
        Ok(Response::new(RemoveNodeResponse {}))
    }

    type RunCommandStream = TaskStream;

    async fn run_command(
        &self,
        request: Request<RunCommandRequest>,
    ) -> Result<Response<TaskStream>, Status> {
        let req = request.into_inner();
        if req.argv.is_empty() {
            return Err(Status::invalid_argument("a command is required"));
        }
        let (node, conn) = self.connected_node(&req.node).await?;
        let record = req.argv.clone();
        self.dispatch(node, conn, &record, |task_id| {
            RunTask {
                task_id,
                argv: req.argv,
                cwd: req.cwd,
                env: req.env,
            }
            .into()
        })
        .await
    }

    type PromptStream = TaskStream;

    async fn prompt(
        &self,
        request: Request<PromptRequest>,
    ) -> Result<Response<TaskStream>, Status> {
        let req = request.into_inner();
        if req.prompt.trim().is_empty() && req.command.is_empty() {
            return Err(Status::invalid_argument("a prompt is required"));
        }
        let (node, conn) = self.connected_node(&req.node).await?;
        let harness = harness(&node, &conn)?;
        let prompt = match req.command.as_str() {
            "" => req.prompt.clone(),
            command => format!("/{command} {}", req.prompt).trim_end().to_string(),
        };
        let record = [harness, prompt];
        self.dispatch(node, conn, &record, |task_id| {
            AgentPrompt {
                task_id,
                prompt: req.prompt,
                session_id: req.session_id,
                cwd: req.cwd,
                model: req.model,
                agent: req.agent,
                variant: req.variant,
                command: req.command,
            }
            .into()
        })
        .await
    }

    async fn get_agent_options(
        &self,
        request: Request<GetAgentOptionsRequest>,
    ) -> Result<Response<AgentOptions>, Status> {
        self.ask_options(&request.into_inner().node, None).await
    }

    async fn switch_mcp_server(
        &self,
        request: Request<SwitchMcpServerRequest>,
    ) -> Result<Response<AgentOptions>, Status> {
        let req = request.into_inner();
        if req.name.is_empty() {
            return Err(Status::invalid_argument("an MCP server name is required"));
        }
        let switch = McpSwitch {
            name: req.name,
            connect: req.connect,
        };
        self.ask_options(&req.node, Some(switch)).await
    }

    async fn list_agent_sessions(
        &self,
        request: Request<ListAgentSessionsRequest>,
    ) -> Result<Response<AgentSessions>, Status> {
        let ask = |request_id| ListAgentSessions { request_id }.into();
        self.ask_harness(
            &request.into_inner().node,
            ask,
            "list its sessions",
            QUERY_TIMEOUT,
        )
        .await
    }

    async fn get_session_history(
        &self,
        request: Request<GetSessionHistoryRequest>,
    ) -> Result<Response<SessionHistory>, Status> {
        let GetSessionHistoryRequest { node, session_id } = request.into_inner();
        let ask = |request_id| {
            GetSessionHistory {
                request_id,
                session_id,
            }
            .into()
        };
        self.ask_harness(&node, ask, "show the session", QUERY_TIMEOUT)
            .await
    }

    async fn list_providers(
        &self,
        request: Request<ListProvidersRequest>,
    ) -> Result<Response<AgentProviders>, Status> {
        let ask = |request_id| ListProviders { request_id }.into();
        self.ask_harness(
            &request.into_inner().node,
            ask,
            "list its providers",
            QUERY_TIMEOUT,
        )
        .await
    }

    async fn authenticate_provider(
        &self,
        request: Request<AuthenticateProviderRequest>,
    ) -> Result<Response<ProviderAuthResult>, Status> {
        let AuthenticateProviderRequest {
            node,
            provider,
            action,
        } = request.into_inner();
        if provider.is_empty() {
            return Err(Status::invalid_argument("a provider is required"));
        }
        // Finishing in the browser waits for the user.
        let wait = match action.as_ref().and_then(|a| a.action.as_ref()) {
            Some(auth_action::Action::OauthFinish(finish)) if finish.code.is_empty() => {
                START_TIMEOUT
            }
            _ => QUERY_TIMEOUT,
        };
        let ask = |request_id| {
            ProviderAuth {
                request_id,
                provider,
                action,
            }
            .into()
        };
        self.ask_harness(&node, ask, "sign in to the provider", wait)
            .await
    }

    async fn prepare_project(
        &self,
        request: Request<PrepareProjectRequest>,
    ) -> Result<Response<ProjectReady>, Status> {
        let PrepareProjectRequest { node, repository } = request.into_inner();
        if repository.trim().is_empty() {
            return Err(Status::invalid_argument("a repository is required"));
        }
        // Any worker can clone; it needs no harness.
        let (node, conn) = self.connected_node(&node).await?;
        let ask = |request_id| {
            PrepareProject {
                request_id,
                repository,
            }
            .into()
        };
        let ready = self
            .ask(&node, &conn, ask, "prepare the project", START_TIMEOUT)
            .await?;
        Ok(Response::new(ready))
    }

    async fn list_projects(
        &self,
        request: Request<ListProjectsRequest>,
    ) -> Result<Response<Projects>, Status> {
        let (node, conn) = self.connected_node(&request.into_inner().node).await?;
        let ask = |request_id| ListProjects { request_id }.into();
        let projects = self
            .ask(&node, &conn, ask, "list its projects", QUERY_TIMEOUT)
            .await?;
        Ok(Response::new(projects))
    }

    async fn start_harness(
        &self,
        request: Request<StartHarnessRequest>,
    ) -> Result<Response<NodeInfo>, Status> {
        let req = request.into_inner();
        let (node, conn) = self.connected_node(&req.node).await?;
        if req.harness.is_empty() {
            return Err(Status::invalid_argument("harness is required"));
        }
        if conn.can_host.is_empty() {
            return Err(Status::failed_precondition(format!(
                "node {}'s worker can't start a harness when asked; update it, or restart it with --harness",
                node.name
            )));
        }
        if !conn.can_host.contains(&req.harness) {
            return Err(Status::invalid_argument(format!(
                "node {} can't host {:?}; it can host {}",
                node.name,
                req.harness,
                conn.can_host.join(", ")
            )));
        }
        info!(node = %node.name, harness = %req.harness, "starting a harness");
        let harness = req.harness;
        let ask = |request_id| {
            StartHarness {
                request_id,
                harness,
            }
            .into()
        };
        let doing = "start its harness in time (it may still be installing it)";
        // The worker refuses another harness than the one it hosts, if any.
        self.ask::<HarnessStarted>(&node, &conn, ask, doing, START_TIMEOUT)
            .await?;
        Ok(Response::new(self.node_info(node)))
    }

    async fn list_tasks(
        &self,
        request: Request<ListTasksRequest>,
    ) -> Result<Response<ListTasksResponse>, Status> {
        let limit = match request.into_inner().limit {
            0 => DEFAULT_TASK_LIMIT,
            n => n.min(MAX_TASK_LIMIT),
        };
        let tasks = self
            .shared
            .store
            .list_tasks(limit)
            .await
            .map_err(internal)?;
        let tasks = tasks
            .into_iter()
            .map(|t| TaskInfo {
                id: t.id,
                node_id: t.node_id,
                node_name: t.node_name,
                argv: t.argv,
                status: t.status,
                exit_code: t.exit_code,
                error: t.error.unwrap_or_default(),
                created_at: t.created_at,
                finished_at: t.finished_at,
                output_pruned: t.output_pruned,
            })
            .collect();
        Ok(Response::new(ListTasksResponse { tasks }))
    }

    type WatchTaskStream = TaskStream;

    async fn watch_task(
        &self,
        request: Request<WatchTaskRequest>,
    ) -> Result<Response<TaskStream>, Status> {
        let needle = request.into_inner().task_id;
        let store = &self.shared.store;
        let task = store.find_task(&needle).await.map_err(internal)?;
        let task = matched_or_status(task, "task", &needle)?;
        let mut started = TaskStarted {
            task_id: task.id.clone(),
            node_id: task.node_id.clone(),
            output_pruned: task.output_pruned,
        };
        let watch = match self.shared.hub.watch(&task.id) {
            Some(watch) => watch,
            // Over and stored. Read it again: it may have ended since.
            None => {
                let gone = || Status::not_found(format!("task {} is gone", task.id));
                let task = store.task(&task.id).await.map_err(internal)?;
                let task = task.ok_or_else(gone)?;
                started.output_pruned = task.output_pruned;
                let output = store.task_output(&task.id).await.map_err(internal)?;
                let mut backlog: Vec<TaskEvent> = output.into_iter().map(Into::into).collect();
                backlog.push(task.finished().into());
                Watch {
                    backlog,
                    live: None,
                }
            }
        };

        let (tx, rx) = mpsc::channel(256);
        tokio::spawn(async move {
            let backlog = std::iter::once(started.into()).chain(watch.backlog);
            for event in backlog {
                if tx.send(Ok(event)).await.is_err() {
                    return;
                }
            }
            if let Some(live) = watch.live {
                relay(live, tx).await;
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    async fn cancel_task(
        &self,
        request: Request<CancelTaskRequest>,
    ) -> Result<Response<CancelTaskResponse>, Status> {
        let needle = request.into_inner().task_id;
        let not_running = || Status::not_found(format!("no running task matches {needle:?}"));
        let (task_id, owner) = self.shared.hub.find(&needle).ok_or_else(not_running)?;
        let conn = self
            .shared
            .registry
            .get(&owner.node_id)
            .filter(|c| c.id == owner.conn_id)
            .ok_or_else(not_running)?;
        conn.tx
            .send(Ok(CancelTask { task_id }.into()))
            .await
            .map_err(|_| not_running())?;
        Ok(Response::new(CancelTaskResponse {}))
    }
}

/// Forwards a task's events to one CLI until the task finishes or the CLI leaves.
async fn relay(mut events: broadcast::Receiver<TaskEvent>, tx: EventTx) {
    loop {
        let event = tokio::select! {
            _ = tx.closed() => return,
            event = events.recv() => event,
        };
        let event = match event {
            Ok(event) => event,
            Err(RecvError::Closed) => return,
            Err(RecvError::Lagged(skipped)) => dropped_output_notice(skipped),
        };
        let is_last = matches!(event.event, Some(task_event::Event::Finished(_)));
        if tx.send(Ok(event)).await.is_err() || is_last {
            return;
        }
    }
}

/// Shown on stderr to a CLI too slow to keep up with the task's output.
fn dropped_output_notice(skipped: u64) -> TaskEvent {
    TaskOutput {
        task_id: String::new(),
        stream: OutputStream::Stderr.into(),
        data: format!("\n[commandant: {skipped} output chunks dropped]\n").into_bytes(),
    }
    .into()
}
