//! Live worker connections, keyed by node id.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use commandant_proto::OrchestratorMsg;
use tokio::sync::mpsc;
use tonic::Status;

/// Unique per connection, so a stale connection closing can't evict a newer
/// one for the same node.
pub type ConnId = u64;

pub type NodeTx = mpsc::Sender<Result<OrchestratorMsg, Status>>;

#[derive(Clone)]
pub struct Connection {
    pub id: ConnId,
    pub tx: NodeTx,
    /// Agent harnesses the worker hosts, e.g. `opencode`.
    pub harnesses: Vec<String>,
    /// Harnesses it could start when asked.
    pub can_host: Vec<String>,
}

#[derive(Default)]
pub struct Registry {
    nodes: Mutex<HashMap<String, Connection>>,
    last_id: AtomicU64,
}

impl Registry {
    /// Registers a connection, replacing (and thereby closing) any previous one.
    pub fn connect(
        &self,
        node_id: &str,
        tx: NodeTx,
        harnesses: Vec<String>,
        can_host: Vec<String>,
    ) -> ConnId {
        let id = self.last_id.fetch_add(1, Ordering::Relaxed) + 1;
        let connection = Connection {
            id,
            tx,
            harnesses,
            can_host,
        };
        let replaced = self
            .nodes
            .lock()
            .unwrap()
            .insert(node_id.to_string(), connection);
        // A worker reconnecting has already let go of its old stream; one
        // still listening is another worker with the same credentials. Telling
        // it why stops it, where reconnecting would knock this one off in turn.
        if let Some(old) = replaced {
            let why = "another worker connected as this node, with the same credentials; \
                       give each worker its own state directory";
            let _ = old.tx.try_send(Err(Status::already_exists(why)));
        }
        id
    }

    /// Removes the node's connection unless it has already been replaced.
    pub fn disconnect(&self, node_id: &str, conn_id: ConnId) {
        let mut nodes = self.nodes.lock().unwrap();
        if nodes.get(node_id).is_some_and(|c| c.id == conn_id) {
            nodes.remove(node_id);
        }
    }

    /// Drops every connection, which ends their streams.
    pub fn disconnect_all(&self) {
        self.nodes.lock().unwrap().clear();
    }

    /// Drops the node's connection, whichever it is, telling the worker why
    /// rather than leaving it to notice at its next heartbeat.
    pub fn kick(&self, node_id: &str, why: &str) {
        let removed = self.nodes.lock().unwrap().remove(node_id);
        if let Some(connection) = removed {
            let _ = connection.tx.try_send(Err(Status::unauthenticated(why)));
        }
    }

    pub fn get(&self, node_id: &str) -> Option<Connection> {
        self.nodes.lock().unwrap().get(node_id).cloned()
    }

    pub fn is_current(&self, node_id: &str, conn_id: ConnId) -> bool {
        self.get(node_id).is_some_and(|c| c.id == conn_id)
    }

    pub fn is_online(&self, node_id: &str) -> bool {
        self.nodes.lock().unwrap().contains_key(node_id)
    }

    /// Records a harness the connection's worker started.
    pub fn set_harnesses(&self, node_id: &str, conn_id: ConnId, harnesses: Vec<String>) {
        let mut nodes = self.nodes.lock().unwrap();
        if let Some(connection) = nodes.get_mut(node_id).filter(|c| c.id == conn_id) {
            connection.harnesses = harnesses;
        }
    }
}
