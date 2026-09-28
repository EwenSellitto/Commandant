//! The Commandant worker: dials into an orchestrator and runs what it's told.

mod exec;
pub mod state;

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use commandant_proto::hello::Auth;
use commandant_proto::node_link_client::NodeLinkClient;
use commandant_proto::{
    CancelTask, Heartbeat, Hello, NodeCredential, OrchestratorMsg, Welcome, WorkerMsg,
    orchestrator_msg,
};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::{Channel, Endpoint};
use tonic::{Code, Streaming};
use tracing::{info, warn};

use crate::state::Credentials;

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Orchestrator URL, e.g. `http://10.0.0.1:7400`.
    pub server: String,
    /// Only needed until the node has credentials in `state_dir`.
    pub join_token: Option<String>,
    /// Defaults to the hostname.
    pub name: Option<String>,
    pub state_dir: PathBuf,
}

/// Why a session ended.
enum Stop {
    /// Retrying won't help (bad credentials, name conflict...).
    Fatal(anyhow::Error),
    Retry(anyhow::Error),
}

impl From<tonic::Status> for Stop {
    fn from(status: tonic::Status) -> Self {
        let err = anyhow!("{}: {}", status.code(), status.message());
        match status.code() {
            Code::Unauthenticated | Code::AlreadyExists | Code::InvalidArgument => Stop::Fatal(err),
            _ => Stop::Retry(err),
        }
    }
}

/// Stays connected to the orchestrator, reconnecting with backoff, until a
/// fatal error occurs.
pub async fn run(config: WorkerConfig) -> anyhow::Result<()> {
    let mut backoff = MIN_BACKOFF;
    loop {
        let started = Instant::now();
        let Err(stop) = session(&config).await;
        match stop {
            Stop::Fatal(e) => return Err(e),
            Stop::Retry(e) => warn!("connection to {} lost: {e:#}", config.server),
        }
        let was_healthy = started.elapsed() > MAX_BACKOFF;
        if was_healthy {
            backoff = MIN_BACKOFF;
        }
        info!("reconnecting in {}s", backoff.as_secs());
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// One connection to the orchestrator, from handshake until it breaks.
async fn session(config: &WorkerConfig) -> Result<Infallible, Stop> {
    let saved = state::load(&config.state_dir).map_err(Stop::Fatal)?;
    let hello = hello(config, saved.as_ref())?;
    let channel = connect(&config.server).await?;

    let (outbound, outbound_rx) = mpsc::channel::<WorkerMsg>(256);
    outbound
        .send(hello.into())
        .await
        .expect("receiver is alive");
    let mut inbound = NodeLinkClient::new(channel)
        .link(ReceiverStream::new(outbound_rx))
        .await?
        .into_inner();

    let welcome = match inbound.message().await? {
        Some(OrchestratorMsg {
            msg: Some(orchestrator_msg::Msg::Welcome(welcome)),
        }) => welcome,
        other => return Err(Stop::Retry(anyhow!("expected welcome, got {other:?}"))),
    };
    if let Some(updated) = credentials_to_save(config, saved, &welcome) {
        state::save(&config.state_dir, &updated).map_err(Stop::Fatal)?;
    }
    info!(node_id = %welcome.node_id, server = %config.server, "connected to orchestrator");

    serve(&mut inbound, &outbound).await
}

fn hello(config: &WorkerConfig, saved: Option<&Credentials>) -> Result<Hello, Stop> {
    let auth = match (saved, &config.join_token) {
        (Some(creds), _) => Auth::Credential(NodeCredential {
            node_id: creds.node_id.clone(),
            secret: creds.secret.clone(),
        }),
        (None, Some(token)) => Auth::JoinToken(token.clone()),
        (None, None) => {
            return Err(Stop::Fatal(anyhow!(
                "no credentials in {} and no join token given",
                config.state_dir.display()
            )));
        }
    };
    let hostname = hostname();
    Ok(Hello {
        auth: Some(auth),
        name: config.name.clone().unwrap_or_else(|| hostname.clone()),
        hostname,
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        version: commandant_common::VERSION.into(),
        capabilities: vec!["exec".into()],
    })
}

async fn connect(server: &str) -> Result<Channel, Stop> {
    let server = commandant_common::link::prefer_loopback(server).await;
    Endpoint::from_shared(server)
        .context("invalid server URL")
        .map_err(Stop::Fatal)?
        .connect_timeout(Duration::from_secs(10))
        .http2_keep_alive_interval(Duration::from_secs(20))
        .keep_alive_while_idle(true)
        .connect()
        .await
        .map_err(|e| Stop::Retry(anyhow!("connecting: {e}")))
}

/// New credentials on first join; otherwise the saved ones, if the
/// orchestrator URL they remember is out of date.
fn credentials_to_save(
    config: &WorkerConfig,
    saved: Option<Credentials>,
    welcome: &Welcome,
) -> Option<Credentials> {
    let server = Some(config.server.clone());
    if !welcome.node_secret.is_empty() {
        return Some(Credentials {
            node_id: welcome.node_id.clone(),
            secret: welcome.node_secret.clone(),
            server,
        });
    }
    saved
        .filter(|creds| creds.server != server)
        .map(|creds| Credentials { server, ..creds })
}

/// Runs and cancels tasks as told, heartbeating in between.
async fn serve(
    inbound: &mut Streaming<OrchestratorMsg>,
    outbound: &mpsc::Sender<WorkerMsg>,
) -> Result<Infallible, Stop> {
    // Dropping a cancel sender kills its task, so ending the session kills them all.
    let mut cancels: HashMap<String, oneshot::Sender<()>> = HashMap::new();
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    loop {
        tokio::select! {
            msg = inbound.message() => {
                let msg = msg?.ok_or_else(|| Stop::Retry(anyhow!("orchestrator closed the stream")))?;
                match msg.msg {
                    Some(orchestrator_msg::Msg::Run(task)) => {
                        info!(task_id = %task.task_id, argv = ?task.argv, "running task");
                        cancels.retain(|_, cancel| !cancel.is_closed());
                        let (cancel, cancelled) = oneshot::channel();
                        cancels.insert(task.task_id.clone(), cancel);
                        tokio::spawn(exec::run(task, outbound.clone(), cancelled));
                    }
                    Some(orchestrator_msg::Msg::Cancel(CancelTask { task_id })) => {
                        if let Some(cancel) = cancels.remove(&task_id) {
                            info!(%task_id, "cancelling task");
                            let _ = cancel.send(());
                        }
                    }
                    Some(orchestrator_msg::Msg::Welcome(_)) | None => {}
                }
            }
            _ = heartbeat.tick() => {
                if outbound.send(Heartbeat {}.into()).await.is_err() {
                    return Err(Stop::Retry(anyhow!("outbound stream closed")));
                }
            }
        }
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown".into())
}
