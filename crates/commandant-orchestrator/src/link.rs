//! NodeLink service: the persistent stream each worker keeps open.

use std::sync::Arc;
use std::time::Duration;

use commandant_proto::hello::Auth;
use commandant_proto::node_link_server::NodeLink;
use commandant_proto::{
    CAN_HOST, HOSTS, Hello, NodeCredential, OrchestratorMsg, TaskFinished, Welcome, WorkerMsg,
    worker_msg,
};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};
use tracing::{info, warn};

use crate::auth::{NODE_PREFIX, generate_token, hash_token, hashes_match};
use crate::registry::ConnId;
use crate::store::{NODE_DISCONNECTED, NodeFacts};
use crate::{Shared, internal};

const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// Three missed worker heartbeats.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest node name, in bytes; a Linux hostname fits.
const MAX_NAME: usize = 64;
/// Longest hostname, os, arch, version or capability a hello may carry.
const MAX_FACT: usize = 256;
const MAX_CAPABILITIES: usize = 64;

pub struct LinkService {
    shared: Arc<Shared>,
}

impl LinkService {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    async fn admit(&self, hello: &Hello) -> Result<Welcome, Status> {
        check_facts(hello)?;
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
        let admin = self.shared.admin_tokens.accepts(join_token);
        let hash = hash_token(join_token);
        // The token first, so a stranger learns nothing about the nodes; the
        // names next, before a single-use token is spent on a join that fails.
        if !admin && !store.join_token_usable(&hash).await.map_err(internal)? {
            return Err(bad_join_token());
        }
        if store.node_name_taken(name).await.map_err(internal)? {
            return Err(name_taken(name));
        }
        if store.node_id_starts_with(name).await.map_err(internal)? {
            return Err(Status::invalid_argument(format!(
                "{name:?} is the start of a node's id, so it would not name one node; \
                 give this worker another --name"
            )));
        }
        if !admin && !store.consume_join_token(&hash).await.map_err(internal)? {
            return Err(bad_join_token());
        }

        let node_id = uuid::Uuid::new_v4().to_string();
        let node_secret = generate_token(NODE_PREFIX);
        let inserted = store
            .insert_node(&node_id, name, &hash_token(&node_secret), facts)
            .await
            .map_err(internal)?;
        if !inserted {
            return Err(name_taken(name));
        }
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

/// The name a joining worker asks for: letters, digits, `.`, `_` and `-`,
/// so it prints safely and reads as one word on a command line.
fn node_name(hello: &Hello) -> Result<&str, Status> {
    let name = if hello.name.is_empty() {
        &hello.hostname
    } else {
        &hello.name
    };
    if name.is_empty() {
        return Err(Status::invalid_argument("node name is required"));
    }
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
    if name.len() > MAX_NAME || !name.chars().all(allowed) {
        return Err(Status::invalid_argument(format!(
            "node name {name:?} is not allowed: use up to {MAX_NAME} letters, digits, \
             '.', '_' or '-' (set it with --name)"
        )));
    }
    Ok(name)
}

/// Refuses a hello whose facts are too long to keep or list.
fn check_facts(hello: &Hello) -> Result<(), Status> {
    let fields = [
        ("hostname", &hello.hostname),
        ("os", &hello.os),
        ("arch", &hello.arch),
        ("version", &hello.version),
    ];
    let capabilities = hello.capabilities.iter().map(|c| ("capability", c));
    for (field, value) in fields.into_iter().chain(capabilities) {
        if value.len() > MAX_FACT {
            return Err(Status::invalid_argument(format!(
                "{field} is longer than {MAX_FACT} bytes"
            )));
        }
    }
    if hello.capabilities.len() > MAX_CAPABILITIES {
        return Err(Status::invalid_argument(format!(
            "more than {MAX_CAPABILITIES} capabilities"
        )));
    }
    Ok(())
}

fn bad_join_token() -> Status {
    Status::unauthenticated("invalid, expired or already used join token")
}

/// With `prefix` [`HOSTS`], `harness:opencode` in the hello's capabilities
/// becomes `opencode`.
fn capabilities(hello: &Hello, prefix: &str) -> Vec<String> {
    hello
        .capabilities
        .iter()
        .filter_map(|c| c.strip_prefix(prefix))
        .map(String::from)
        .collect()
}

fn name_taken(name: &str) -> Status {
    Status::already_exists(format!(
        "a node named {name:?} already exists; give this worker another --name"
    ))
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
        let conn_id = self.shared.registry.connect(
            &node_id,
            tx,
            capabilities(&hello, HOSTS),
            capabilities(&hello, CAN_HOST),
        );
        info!(%node_id, ?conn_id, hostname = %hello.hostname, "node connected");

        let shared = self.shared.clone();
        tokio::spawn(async move {
            let reason = handle_messages(&shared, &node_id, conn_id, &mut inbound).await;
            info!(%node_id, ?conn_id, %reason, "node disconnected");
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
            worker_msg::Msg::HarnessStarted(started) => {
                if started.error.is_empty() {
                    let harnesses = started.harnesses.clone();
                    shared.registry.set_harnesses(node_id, conn_id, harnesses);
                }
                shared
                    .queries
                    .answer(conn_id, worker_msg::Msg::HarnessStarted(started));
            }
            answer @ (worker_msg::Msg::Options(_)
            | worker_msg::Msg::Sessions(_)
            | worker_msg::Msg::History(_)
            | worker_msg::Msg::Providers(_)
            | worker_msg::Msg::AuthResult(_)
            | worker_msg::Msg::ProjectReady(_)
            | worker_msg::Msg::Projects(_)) => shared.queries.answer(conn_id, answer),
            worker_msg::Msg::Hello(_) => warn!(%node_id, "ignoring duplicate hello"),
        }
    }
}

async fn record_finished(shared: &Shared, conn_id: ConnId, finished: TaskFinished) {
    let Some(output) = shared.hub.finish(conn_id, finished.clone()) else {
        return;
    };
    if let Err(e) = shared.store.finish_task(&finished, &output).await {
        warn!(task_id = %finished.task_id, "failed to record task result: {e}");
    }
    shared.hub.close(&finished.task_id);
}

/// Marks the node offline and its unfinished tasks lost.
async fn forget_connection(shared: &Shared, node_id: &str, conn_id: ConnId) {
    shared.registry.disconnect(node_id, conn_id);
    for (task_id, output) in shared.hub.fail_connection(conn_id, NODE_DISCONNECTED) {
        if let Err(e) = shared.store.lose_task(&task_id, &output).await {
            warn!(%task_id, "failed to record lost task: {e}");
        }
        shared.hub.close(&task_id);
    }
    let _ = shared.store.touch_node(node_id).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hellos_carry_short_facts_and_plain_names() {
        let hello = |hostname: &str, capabilities: usize| Hello {
            hostname: hostname.into(),
            capabilities: vec!["exec".into(); capabilities],
            ..Default::default()
        };
        assert!(check_facts(&hello("box", MAX_CAPABILITIES)).is_ok());
        assert!(check_facts(&hello(&"h".repeat(MAX_FACT + 1), 1)).is_err());
        assert!(check_facts(&hello("box", MAX_CAPABILITIES + 1)).is_err());

        // Without --name, the hostname is the name, and a Linux one fits.
        assert_eq!(node_name(&hello("Box-2.lan", 1)).unwrap(), "Box-2.lan");
        assert!(node_name(&hello(&"h".repeat(MAX_NAME), 1)).is_ok());
        for bad in ["", "a b", "a\u{1b}[2J", "café", &"h".repeat(MAX_NAME + 1)] {
            assert!(node_name(&hello(bad, 1)).is_err(), "{bad:?}");
        }
    }
}
