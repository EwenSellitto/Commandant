//! The Commandant orchestrator: control plane that workers dial into.

mod auth;
mod control;
mod link;
mod queries;
mod registry;
mod store;
mod tasks;

use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
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
use crate::queries::Queries;
use crate::registry::Registry;
use crate::store::Store;
use crate::tasks::TaskHub;

pub const ADMIN_TOKEN_FILE: &str = "admin.token";
/// The database, in the data directory.
pub const DB_FILE: &str = "commandant.db";

pub(crate) struct Shared {
    pub store: Store,
    /// Also accepted as join tokens, so one link serves workers and admins.
    pub admin_tokens: AdminTokens,
    pub registry: Registry,
    pub hub: TaskHub,
    pub queries: Queries,
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
        let db = data_dir.join(DB_FILE);
        let store = Store::open(&db)
            .await
            .with_context(|| format!("opening database {}", db.display()))?;
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
            queries: Queries::default(),
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
        // Worker links never end on their own, and a graceful shutdown waits
        // for every open stream.
        let shutdown = async move {
            shutdown.await;
            shared.registry.disconnect_all();
        };
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

/// The database keeps the token, so it alone brings the same link back. The
/// file is a copy for `COMMANDANT_TOKEN_FILE`, and the source for databases
/// from before, which only kept its hash.
async fn load_or_create_admin_token(store: &Store, token_file: &Path) -> Result<String> {
    if let Some(token) = store.admin_token().await? {
        write_private(token_file, &format!("{token}\n"))?;
        return Ok(token);
    }
    if !store.admin_token_hashes().await?.is_empty() {
        let token = read_trimmed(token_file)
            .context("the database only holds the admin token's hash, so its file is needed")?;
        if !store.keep_admin_token(&hash_token(&token), &token).await? {
            bail!(
                "{} doesn't hold this database's admin token; restore it, or start \
                 afresh with --reset",
                token_file.display()
            );
        }
        return Ok(token);
    }
    let token = generate_token(ADMIN_PREFIX);
    store.add_admin_token(&hash_token(&token), &token).await?;
    write_private(token_file, &format!("{token}\n"))?;
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_database_alone_keeps_the_admin_token() {
        let tmp = std::env::temp_dir().join(format!("commandant-admin-{}", std::process::id()));
        let data_dir = tmp.join("data");
        let db = data_dir.join(DB_FILE);
        let token_file = data_dir.join(ADMIN_TOKEN_FILE);

        let first = Orchestrator::open(&data_dir).await.unwrap();
        let token = first.admin_token().to_string();
        drop(first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&db).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        // Losing the file doesn't lose the token; the file comes back.
        std::fs::remove_file(&token_file).unwrap();
        let again = Orchestrator::open(&data_dir).await.unwrap();
        assert_eq!(again.admin_token(), token);
        assert_eq!(read_trimmed(&token_file).unwrap(), token);

        // A database that only kept the hash takes the token from the file.
        let store = &again.shared.store;
        sqlx::query("UPDATE admin_tokens SET token = NULL")
            .execute(store.pool())
            .await
            .unwrap();
        assert_eq!(
            load_or_create_admin_token(store, &token_file)
                .await
                .unwrap(),
            token
        );
        assert_eq!(
            store.admin_token().await.unwrap().as_deref(),
            Some(token.as_str())
        );

        // ...but not a token it doesn't know.
        sqlx::query("UPDATE admin_tokens SET token = NULL")
            .execute(store.pool())
            .await
            .unwrap();
        write_private(&token_file, "cmda_wrong\n").unwrap();
        assert!(
            load_or_create_admin_token(store, &token_file)
                .await
                .is_err()
        );

        std::fs::remove_dir_all(tmp).unwrap();
    }
}
