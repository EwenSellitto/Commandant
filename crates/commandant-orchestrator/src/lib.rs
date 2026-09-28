//! The Commandant orchestrator: control plane that workers dial into.

mod auth;
mod control;
mod link;
mod registry;
mod store;
mod tasks;

use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use commandant_proto::control_server::ControlServer;
use commandant_proto::node_link_server::NodeLinkServer;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;
use tracing::info;

use commandant_common::fs::{read_trimmed, write_private};

use crate::auth::{ADMIN_PREFIX, AdminTokens, generate_token, hash_token};
use crate::control::ControlService;
use crate::link::LinkService;
use crate::registry::Registry;
use crate::store::Store;
use crate::tasks::TaskHub;

pub const ADMIN_TOKEN_FILE: &str = "admin.token";

pub(crate) struct Shared {
    pub store: Store,
    /// Also accepted as join tokens, so one link serves workers and admins.
    pub admin_tokens: AdminTokens,
    pub registry: Registry,
    pub hub: TaskHub,
}

pub struct Orchestrator {
    shared: Arc<Shared>,
    admin_token: String,
}

/// Maps store and other unexpected failures to an opaque gRPC error.
pub(crate) fn internal(e: impl std::fmt::Display) -> tonic::Status {
    tonic::Status::internal(e.to_string())
}

impl Orchestrator {
    /// Opens (or initialises) the data directory.
    pub async fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("creating data dir {}", data_dir.display()))?;
        let store = Store::open(&data_dir.join("commandant.db")).await?;
        let lost = store.mark_running_tasks_lost().await?;
        if lost > 0 {
            info!(lost, "marked tasks from a previous run as lost");
        }
        let admin_token =
            load_or_create_admin_token(&store, &data_dir.join(ADMIN_TOKEN_FILE)).await?;
        let admin_tokens = AdminTokens(store.admin_token_hashes().await?);

        let shared = Arc::new(Shared {
            store,
            admin_tokens,
            registry: Registry::default(),
            hub: TaskHub::default(),
        });
        Ok(Self {
            shared,
            admin_token,
        })
    }

    /// The admin token (also accepted as a join token).
    pub fn admin_token(&self) -> &str {
        &self.admin_token
    }

    /// Serves both gRPC services on `listener` until `shutdown` resolves.
    pub async fn serve(
        self,
        listener: TcpListener,
        shutdown: impl Future<Output = ()> + Send,
    ) -> Result<()> {
        info!(addr = %listener.local_addr()?, "orchestrator listening");
        let shared = self.shared.clone();
        let control =
            ControlServer::with_interceptor(ControlService::new(self.shared.clone()), move |req| {
                shared.admin_tokens.check(req)
            });
        Server::builder()
            .http2_keepalive_interval(Some(Duration::from_secs(20)))
            .add_service(NodeLinkServer::new(LinkService::new(self.shared)))
            .add_service(control)
            .serve_with_incoming_shutdown(TcpListenerStream::new(listener), shutdown)
            .await?;
        Ok(())
    }
}

/// Only the hash is kept in the database, so the token itself lives in a file.
async fn load_or_create_admin_token(store: &Store, token_file: &Path) -> Result<String> {
    if !store.admin_token_hashes().await?.is_empty() {
        return read_trimmed(token_file);
    }
    let token = generate_token(ADMIN_PREFIX);
    store.add_admin_token(&hash_token(&token)).await?;
    write_private(token_file, &format!("{token}\n"))?;
    Ok(token)
}
