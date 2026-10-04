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
    /// Agent harnesses the worker announced, e.g. `opencode`.
    pub harnesses: Vec<String>,
}

#[derive(Default)]
pub struct Registry {
    nodes: Mutex<HashMap<String, Connection>>,
    last_id: AtomicU64,
}

impl Registry {
    /// Registers a connection, replacing (and thereby closing) any previous one.
    pub fn connect(&self, node_id: &str, tx: NodeTx, harnesses: Vec<String>) -> ConnId {
        let id = self.last_id.fetch_add(1, Ordering::Relaxed) + 1;
        let connection = Connection { id, tx, harnesses };
        self.nodes
            .lock()
            .unwrap()
            .insert(node_id.to_string(), connection);
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

    /// The harnesses of the node's live connection; empty when offline.
    pub fn harnesses(&self, node_id: &str) -> Vec<String> {
        self.get(node_id).map(|c| c.harnesses).unwrap_or_default()
    }
}
