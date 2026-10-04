//! The Commandant worker: dials into an orchestrator and runs what it's told.

mod exec;
mod harness;
mod opencode;
pub mod state;

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use commandant_proto::hello::Auth;
use commandant_proto::node_link_client::NodeLinkClient;
use commandant_proto::{
    AgentOptions, AgentPrompt, AgentSessions, CancelTask, Heartbeat, Hello, ListAgentOptions,
    ListAgentSessions, McpSwitch, NodeCredential, OrchestratorMsg, TaskFinished, Welcome,
    WorkerMsg, orchestrator_msg,
};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::{Channel, Endpoint};
use tonic::{Code, Streaming};
use tracing::{info, warn};

pub use crate::harness::HarnessKind;
use crate::opencode::Opencode;
use crate::state::Credentials;

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const NO_HARNESS: &str = "this node runs no agent harness";

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Orchestrator URL, e.g. `http://10.0.0.1:7400`.
    pub server: String,
    /// Only needed until the node has credentials in `state_dir`.
    pub join_token: Option<String>,
    /// Defaults to the hostname.
    pub name: Option<String>,
    pub state_dir: PathBuf,
    /// The coding agent to host, if any.
    pub harness: Option<HarnessKind>,
    /// The `opencode` to run; when unset it is looked for, or installed.
    pub opencode_bin: Option<PathBuf>,
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

/// Sets up the harness, then stays connected to the orchestrator,
/// reconnecting with backoff, until a fatal error occurs.
pub async fn run(config: WorkerConfig) -> anyhow::Result<()> {
    let harness = match config.harness {
        Some(HarnessKind::Opencode) => Some(Arc::new(
            Opencode::start(config.opencode_bin.clone())
                .await
                .context("setting up opencode")?,
        )),
        None => None,
    };
    let mut backoff = MIN_BACKOFF;
    loop {
        let started = Instant::now();
        let Err(stop) = session(&config, harness.as_ref()).await;
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
async fn session(
    config: &WorkerConfig,
    harness: Option<&Arc<Opencode>>,
) -> Result<Infallible, Stop> {
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

    serve(&mut inbound, &outbound, harness).await
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
        capabilities: std::iter::once("exec".into())
            .chain(config.harness.map(HarnessKind::capability))
            .collect(),
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
    harness: Option<&Arc<Opencode>>,
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
                        let cancelled = track(&mut cancels, &task.task_id);
                        tokio::spawn(exec::run(task, outbound.clone(), cancelled));
                    }
                    Some(orchestrator_msg::Msg::Prompt(task)) => {
                        info!(task_id = %task.task_id, "prompting the agent");
                        let cancelled = track(&mut cancels, &task.task_id);
                        match harness {
                            Some(opencode) => {
                                tokio::spawn(opencode::run(opencode.clone(), task, outbound.clone(), cancelled));
                            }
                            None => refuse_prompt(task, outbound).await,
                        }
                    }
                    Some(orchestrator_msg::Msg::ListOptions(ListAgentOptions { request_id, mcp })) => {
                        tokio::spawn(answer_options(harness.cloned(), request_id, mcp, outbound.clone()));
                    }
                    Some(orchestrator_msg::Msg::ListSessions(ListAgentSessions { request_id })) => {
                        tokio::spawn(answer_sessions(harness.cloned(), request_id, outbound.clone()));
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

/// Returns what fires when the task is cancelled (or the session ends).
fn track(
    cancels: &mut HashMap<String, oneshot::Sender<()>>,
    task_id: &str,
) -> oneshot::Receiver<()> {
    cancels.retain(|_, cancel| !cancel.is_closed());
    let (cancel, cancelled) = oneshot::channel();
    cancels.insert(task_id.to_string(), cancel);
    cancelled
}

async fn refuse_prompt(task: AgentPrompt, outbound: &mpsc::Sender<WorkerMsg>) {
    let refused = TaskFinished {
        task_id: task.task_id,
        error: NO_HARNESS.into(),
        ..Default::default()
    };
    let _ = outbound.send(refused.into()).await;
}

/// Tells the orchestrator which agents, models and efforts the harness offers.
async fn answer_options(
    harness: Option<Arc<Opencode>>,
    request_id: String,
    mcp: Option<McpSwitch>,
    outbound: mpsc::Sender<WorkerMsg>,
) {
    let options = match harness {
        Some(opencode) => opencode::list_options(&opencode, request_id, mcp).await,
        None => AgentOptions {
            request_id,
            error: NO_HARNESS.into(),
            ..Default::default()
        },
    };
    let _ = outbound.send(options.into()).await;
}

/// Tells the orchestrator which sessions the harness has saved.
async fn answer_sessions(
    harness: Option<Arc<Opencode>>,
    request_id: String,
    outbound: mpsc::Sender<WorkerMsg>,
) {
    let sessions = match harness {
        Some(opencode) => opencode::list_sessions(&opencode, request_id).await,
        None => AgentSessions {
            request_id,
            error: NO_HARNESS.into(),
            ..Default::default()
        },
    };
    let _ = outbound.send(sessions.into()).await;
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
