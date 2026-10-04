//! NodeLink service: the persistent stream each worker keeps open.

use std::sync::Arc;
use std::time::Duration;

use commandant_proto::hello::Auth;
use commandant_proto::node_link_server::NodeLink;
use commandant_proto::{
    Hello, NodeCredential, OrchestratorMsg, TaskFinished, Welcome, WorkerMsg, worker_msg,
};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};
use tracing::{info, warn};

use crate::auth::{NODE_PREFIX, generate_token, hash_token, hashes_match};
use crate::queries::Answer;
use crate::registry::ConnId;
use crate::store::{InsertNodeError, NODE_DISCONNECTED, NodeFacts, TaskStatus};
use crate::{Shared, internal};

const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// Three missed worker heartbeats.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

pub struct LinkService {
    shared: Arc<Shared>,
}

impl LinkService {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    async fn admit(&self, hello: &Hello) -> Result<Welcome, Status> {
        let facts = NodeFacts {
            hostname: &hello.hostname,
            os: &hello.os,
            arch: &hello.arch,
            version: &hello.version,
        };
        match &hello.auth {
            Some(Auth::JoinToken(token)) => self.enrol(token, node_name(hello)?, &facts).await,
            Some(Auth::Credential(credential)) => self.reconnect(credential, &facts).await,
            None => Err(Status::invalid_argument("hello carries no credentials")),
        }
    }

    /// Creates a new node and hands it a secret to reconnect with.
    async fn enrol(
        &self,
        join_token: &str,
        name: &str,
        facts: &NodeFacts<'_>,
    ) -> Result<Welcome, Status> {
        let store = &self.shared.store;
        // Checked before spending a single-use token on a join that would fail.
        if store.node_name_taken(name).await.map_err(internal)? {
            return Err(name_taken(name));
        }
        let accepted = self.shared.admin_tokens.accepts(join_token)
            || store
                .consume_join_token(&hash_token(join_token))
                .await
                .map_err(internal)?;
        if !accepted {
            return Err(Status::unauthenticated(
                "invalid, expired or already used join token",
            ));
        }

        let node_id = uuid::Uuid::new_v4().to_string();
        let node_secret = generate_token(NODE_PREFIX);
        store
            .insert_node(&node_id, name, &hash_token(&node_secret), facts)
            .await
            .map_err(|e| match e {
                InsertNodeError::NameTaken => name_taken(name),
                InsertNodeError::Other(e) => internal(e),
            })?;
        info!(%node_id, %name, "node joined");
        Ok(Welcome {
            node_id,
            node_secret,
        })
    }

    async fn reconnect(
        &self,
        credential: &NodeCredential,
        facts: &NodeFacts<'_>,
    ) -> Result<Welcome, Status> {
        let store = &self.shared.store;
        let stored_hash = store
            .node_secret_hash(&credential.node_id)
            .await
            .map_err(internal)?;
        let valid =
            stored_hash.is_some_and(|hash| hashes_match(&hash, &hash_token(&credential.secret)));
        if !valid {
            return Err(Status::unauthenticated("unknown node or bad credentials"));
        }
        store
            .update_node_facts(&credential.node_id, facts)
            .await
            .map_err(internal)?;
        Ok(Welcome {
            node_id: credential.node_id.clone(),
            node_secret: String::new(),
        })
    }
}

fn node_name(hello: &Hello) -> Result<&str, Status> {
    let name = if hello.name.is_empty() {
        &hello.hostname
    } else {
        &hello.name
    };
    if name.is_empty() {
        return Err(Status::invalid_argument("node name is required"));
    }
    Ok(name)
}

/// `harness:opencode` in the hello's capabilities becomes `opencode`.
fn harnesses(hello: &Hello) -> Vec<String> {
    hello
        .capabilities
        .iter()
        .filter_map(|c| c.strip_prefix("harness:"))
        .map(String::from)
        .collect()
}

fn name_taken(name: &str) -> Status {
    Status::already_exists(format!("a node named {name:?} already exists"))
}

#[tonic::async_trait]
impl NodeLink for LinkService {
    type LinkStream = ReceiverStream<Result<OrchestratorMsg, Status>>;

    async fn link(
        &self,
        request: Request<Streaming<WorkerMsg>>,
    ) -> Result<Response<Self::LinkStream>, Status> {
        let mut inbound = request.into_inner();
        let hello = receive_hello(&mut inbound).await?;
        let welcome = self.admit(&hello).await?;
        let node_id = welcome.node_id.clone();

        let (tx, rx) = mpsc::channel(256);
        tx.send(Ok(welcome.into())).await.map_err(internal)?;
        let conn_id = self
            .shared
            .registry
            .connect(&node_id, tx, harnesses(&hello));
        info!(%node_id, conn_id, hostname = %hello.hostname, "node connected");

        let shared = self.shared.clone();
        tokio::spawn(async move {
            let reason = handle_messages(&shared, &node_id, conn_id, &mut inbound).await;
            info!(%node_id, conn_id, %reason, "node disconnected");
            forget_connection(&shared, &node_id, conn_id).await;
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

async fn receive_hello(inbound: &mut Streaming<WorkerMsg>) -> Result<Hello, Status> {
    match timeout(HELLO_TIMEOUT, inbound.message()).await {
        Err(_) => Err(Status::deadline_exceeded("no hello received")),
        Ok(Err(status)) => Err(status),
        Ok(Ok(Some(WorkerMsg {
            msg: Some(worker_msg::Msg::Hello(hello)),
        }))) => Ok(hello),
        Ok(Ok(_)) => Err(Status::invalid_argument("first message must be a hello")),
    }
}

/// Handles worker messages until the stream ends; returns why it ended.
async fn handle_messages(
    shared: &Shared,
    node_id: &str,
    conn_id: ConnId,
    inbound: &mut Streaming<WorkerMsg>,
) -> String {
    loop {
        let msg = match timeout(IDLE_TIMEOUT, inbound.message()).await {
            Err(_) => return "heartbeat timeout".into(),
            Ok(Err(status)) => return format!("stream error: {}", status.message()),
            Ok(Ok(None)) => return "stream closed".into(),
            Ok(Ok(Some(WorkerMsg { msg: None }))) => continue,
            Ok(Ok(Some(WorkerMsg { msg: Some(msg) }))) => msg,
        };
        if !shared.registry.is_current(node_id, conn_id) {
            return "replaced by a newer connection, or node removed".into();
        }
        match msg {
            worker_msg::Msg::Heartbeat(_) => {
                let _ = shared.store.touch_node(node_id).await;
            }
            worker_msg::Msg::Output(output) => shared.hub.output(conn_id, output),
            worker_msg::Msg::Finished(finished) => record_finished(shared, conn_id, finished).await,
            worker_msg::Msg::Options(options) => shared.queries.answer(Answer::Options(options)),
            worker_msg::Msg::Sessions(sessions) => {
                shared.queries.answer(Answer::Sessions(sessions))
            }
            worker_msg::Msg::Hello(_) => warn!(%node_id, "ignoring duplicate hello"),
        }
    }
}

async fn record_finished(shared: &Shared, conn_id: ConnId, finished: TaskFinished) {
    let task_id = finished.task_id.clone();
    let status = TaskStatus::of(&finished);
    let exit_code = finished.exit_code;
    let error = Some(finished.error.clone()).filter(|e| !e.is_empty());
    if !shared.hub.finish(conn_id, finished) {
        return;
    }
    if let Err(e) = shared
        .store
        .finish_task(&task_id, status, exit_code, error.as_deref())
        .await
    {
        warn!(%task_id, "failed to record task result: {e}");
    }
}

/// Marks the node offline and its unfinished tasks lost.
async fn forget_connection(shared: &Shared, node_id: &str, conn_id: ConnId) {
    shared.registry.disconnect(node_id, conn_id);
    for task_id in shared.hub.fail_connection(conn_id, NODE_DISCONNECTED) {
        if let Err(e) = shared.store.lose_task(&task_id).await {
            warn!(%task_id, "failed to record lost task: {e}");
        }
    }
    let _ = shared.store.touch_node(node_id).await;
}
