//! Control service: the admin API used by the CLI.

use std::sync::Arc;

use commandant_proto::control_server::Control;
use commandant_proto::*;
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use tracing::info;

use commandant_common::lookup::{self, Match};

use crate::auth::{JOIN_PREFIX, generate_token, hash_token};
use crate::store::{NodeRecord, now};
use crate::tasks::Owner;
use crate::{Shared, internal};

const DEFAULT_TASK_LIMIT: u32 = 20;
const MAX_TASK_LIMIT: u32 = 1000;

type EventTx = mpsc::Sender<Result<TaskEvent, Status>>;

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
        if let Some(node) = nodes.iter().find(|n| n.name == needle) {
            return Ok(node.clone());
        }
        match lookup::find(nodes, needle, |n| &n.id) {
            Match::One(node) => Ok(node),
            Match::Ambiguous => Err(Status::invalid_argument(format!("{needle:?} is ambiguous"))),
            Match::None => Err(Status::not_found(format!("no node matches {needle:?}"))),
        }
    }
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
        let registry = &self.shared.registry;
        let nodes = self.shared.store.list_nodes().await.map_err(internal)?;
        let nodes = nodes
            .into_iter()
            .map(|n| NodeInfo {
                online: registry.is_online(&n.id),
                id: n.id,
                name: n.name,
                hostname: n.hostname,
                os: n.os,
                arch: n.arch,
                version: n.version,
                last_seen: n.last_seen,
                created_at: n.created_at,
            })
            .collect();
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
        self.shared.registry.kick(&node.id);
        info!(node_id = %node.id, name = %node.name, "node removed");
        Ok(Response::new(RemoveNodeResponse {}))
    }

    type RunCommandStream = ReceiverStream<Result<TaskEvent, Status>>;

    async fn run_command(
        &self,
        request: Request<RunCommandRequest>,
    ) -> Result<Response<Self::RunCommandStream>, Status> {
        let req = request.into_inner();
        if req.argv.is_empty() {
            return Err(Status::invalid_argument("a command is required"));
        }
        let node = self.resolve_node(&req.node).await?;
        let offline = || Status::unavailable(format!("node {} is offline", node.name));
        let conn = self.shared.registry.get(&node.id).ok_or_else(offline)?;

        let task_id = uuid::Uuid::new_v4().to_string();
        let store = &self.shared.store;
        store
            .insert_task(&task_id, &node.id, &req.argv)
            .await
            .map_err(internal)?;
        let owner = Owner {
            node_id: node.id.clone(),
            conn_id: conn.id,
        };
        let events = self.shared.hub.start(&task_id, owner);

        let run = RunTask {
            task_id: task_id.clone(),
            argv: req.argv,
            cwd: req.cwd,
            env: req.env,
        };
        if conn.tx.send(Ok(run.into())).await.is_err() {
            self.shared.hub.abandon(&task_id);
            let _ = store.lose_task(&task_id).await;
            return Err(offline());
        }
        info!(%task_id, node = %node.name, "task dispatched");

        let (tx, rx) = mpsc::channel(256);
        let started = TaskStarted {
            task_id,
            node_id: node.id,
        };
        let _ = tx.send(Ok(started.into())).await;
        tokio::spawn(relay(events, tx));
        Ok(Response::new(ReceiverStream::new(rx)))
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
            })
            .collect();
        Ok(Response::new(ListTasksResponse { tasks }))
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
