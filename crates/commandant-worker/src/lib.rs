//! The Commandant worker: dials into an orchestrator and runs what it's told.

mod claude;
mod exec;
mod harness;
mod opencode;
mod process;
mod project;
pub mod state;

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use commandant_proto::hello::Auth;
use commandant_proto::node_link_client::NodeLinkClient;
use commandant_proto::{
    AgentPrompt, AgentProviders, AgentSessions, CancelTask, GetSessionHistory, HarnessStarted,
    Heartbeat, Hello, ListAgentOptions, ListAgentSessions, ListProjects, ListProviders,
    NodeCredential, OrchestratorMsg, PrepareProject, ProjectReady, Projects, ProviderAuth, Reply,
    SessionHistory, StartHarness, TaskFinished, Welcome, WorkerMsg, orchestrator_msg,
};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;
use tonic::{Code, Streaming};
use tracing::{info, warn};

use crate::harness::Host;
pub use crate::harness::{Harness, HarnessKind};
use crate::state::Credentials;

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const NO_HARNESS: &str = "this node runs no agent harness";

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Orchestrator URL, e.g. `http://10.0.0.1:7400`; when unset, the one the
    /// saved credentials remember.
    pub server: Option<String>,
    /// Only needed until the node has credentials in `state_dir`.
    pub join_token: Option<String>,
    /// Defaults to the hostname.
    pub name: Option<String>,
    pub state_dir: PathBuf,
    /// When another worker runs from `state_dir`, use the first free
    /// `<state_dir>-2`, `-3`… (each its own node) instead of failing.
    pub pick_free_state_dir: bool,
    /// The coding agent to host, if any.
    pub harness: Option<HarnessKind>,
    /// The binary of the harness to run (`opencode`, `claude`); when unset
    /// it is looked for, or installed.
    pub harness_bin: Option<PathBuf>,
}

/// Who the worker is, once `run` has claimed a state directory.
struct Node {
    server: String,
    join_token: Option<String>,
    name: Option<String>,
    state_dir: PathBuf,
}

/// Why a session ended.
enum Stop {
    /// Retrying won't help (bad credentials, name conflict...).
    Fatal(anyhow::Error),
    Retry(anyhow::Error),
    /// The orchestrator doesn't know the saved credentials, but there is a
    /// join token to join with instead.
    Stale(anyhow::Error),
}

/// A status as an error: `Unauthenticated: invalid join token`.
fn status_error(status: &tonic::Status) -> anyhow::Error {
    anyhow!("{}: {}", status.code(), status.message())
}

impl From<tonic::Status> for Stop {
    fn from(status: tonic::Status) -> Self {
        let err = status_error(&status);
        match status.code() {
            Code::Unauthenticated | Code::AlreadyExists | Code::InvalidArgument => Stop::Fatal(err),
            _ => Stop::Retry(err),
        }
    }
}

/// Sets up the harness, then stays connected to the orchestrator,
/// reconnecting with backoff, until a fatal error occurs.
pub async fn run(config: WorkerConfig) -> anyhow::Result<()> {
    #[cfg(unix)]
    // SAFETY: plain syscall, no arguments.
    normal_user(unsafe { libc::geteuid() })?;
    // Held until the worker stops: no other worker can be this node.
    let claim = state::claim(&config.state_dir, config.pick_free_state_dir)?;
    if claim.instance > 1 {
        info!(
            "another worker runs from {}, so this one uses {}",
            config.state_dir.display(),
            claim.dir.display()
        );
    }
    let remembered = state::load(&claim.dir)?.and_then(|c| c.server);
    let Some(server) = config.server.or(remembered) else {
        bail!(
            "pass the connection link printed by `commandant-server serve`: commandant-server worker commandant://..."
        );
    };
    let node = Node {
        server,
        join_token: config.join_token,
        // Side by side on one machine, each takes its own name.
        name: config
            .name
            .or_else(|| (claim.instance > 1).then(|| format!("{}-{}", hostname(), claim.instance))),
        state_dir: claim.dir.clone(),
    };
    let host = Arc::new(Host::new(config.harness_bin, claim.dir.clone()));
    // Otherwise one is started when a client asks for it.
    if let Some(kind) = config.harness {
        host.start(kind)
            .await
            .with_context(|| format!("setting up {kind}"))?;
    }
    let mut backoff = MIN_BACKOFF;
    // Saved credentials the orchestrator rejects are given up only on the
    // first connection, when the token was just given (say after the
    // orchestrator was reset). A node removed later stays removed.
    let mut first = true;
    loop {
        let started = Instant::now();
        let Err(stop) = session(&node, &host, first).await;
        match stop {
            Stop::Fatal(e) => return Err(e),
            Stop::Stale(e) => {
                warn!(
                    "{e:#}: the orchestrator doesn't know the node saved in {}; joining again with the token",
                    node.state_dir.display()
                );
                state::forget(&node.state_dir)?;
                continue;
            }
            Stop::Retry(e) => warn!("connection to {} lost: {e:#}", node.server),
        }
        first = false;
        let was_healthy = started.elapsed() > MAX_BACKOFF;
        if was_healthy {
            backoff = MIN_BACKOFF;
        }
        info!("reconnecting in {}s", backoff.as_secs());
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// Refuses root: a worker runs whatever the admin and its agent ask, so it
/// gets no more than one user's rights (and Claude Code won't skip its
/// permission prompts as root).
fn normal_user(uid: u32) -> anyhow::Result<()> {
    if uid == 0 {
        bail!("workers run as a normal user, not root: start this one as another user");
    }
    Ok(())
}

/// One connection to the orchestrator, from handshake until it breaks.
async fn session(node: &Node, host: &Arc<Host>, first: bool) -> Result<Infallible, Stop> {
    let saved = state::load(&node.state_dir).map_err(Stop::Fatal)?;
    let hosted = host.current().map(|h| h.kind());
    let hello = hello(node, saved.as_ref(), hosted)?;
    let channel = connect(&node.server).await?;
    let can_rejoin = first && saved.is_some() && node.join_token.is_some();
    let rejected = |status: tonic::Status| match status.code() {
        Code::Unauthenticated if can_rejoin => Stop::Stale(status_error(&status)),
        _ => status.into(),
    };

    let (outbound, outbound_rx) = mpsc::channel::<WorkerMsg>(256);
    outbound
        .send(hello.into())
        .await
        .expect("receiver is alive");
    let mut inbound = NodeLinkClient::new(channel)
        .link(ReceiverStream::new(outbound_rx))
        .await
        .map_err(rejected)?
        .into_inner();

    let welcome = match inbound.message().await.map_err(rejected)? {
        Some(OrchestratorMsg {
            msg: Some(orchestrator_msg::Msg::Welcome(welcome)),
        }) => welcome,
        other => return Err(Stop::Retry(anyhow!("expected welcome, got {other:?}"))),
    };
    if let Some(updated) = credentials_to_save(node, saved, &welcome) {
        state::save(&node.state_dir, &updated).map_err(Stop::Fatal)?;
    }
    info!(node_id = %welcome.node_id, server = %node.server, "connected to orchestrator");

    serve(&mut inbound, &outbound, host, &node.state_dir).await
}

fn hello(
    node: &Node,
    saved: Option<&Credentials>,
    hosted: Option<HarnessKind>,
) -> Result<Hello, Stop> {
    let auth = match (saved, &node.join_token) {
        (Some(creds), _) => Auth::Credential(NodeCredential {
            node_id: creds.node_id.clone(),
            secret: creds.secret.clone(),
        }),
        (None, Some(token)) => Auth::JoinToken(token.clone()),
        (None, None) => {
            return Err(Stop::Fatal(anyhow!(
                "no credentials in {} and no join token given",
                node.state_dir.display()
            )));
        }
    };
    let hostname = hostname();
    Ok(Hello {
        auth: Some(auth),
        name: node.name.clone().unwrap_or_else(|| hostname.clone()),
        hostname,
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        version: commandant_common::VERSION.into(),
        capabilities: std::iter::once("exec".into())
            .chain(hosted.map(harness::capability))
            .chain(HarnessKind::ALL.map(harness::hostable))
            .collect(),
    })
}

async fn connect(server: &str) -> Result<Channel, Stop> {
    commandant_proto::channel(server)
        .await
        .map_err(|e| Stop::Retry(e.context("connecting")))
}

/// New credentials on first join; otherwise the saved ones, if the
/// orchestrator URL they remember is out of date.
fn credentials_to_save(
    node: &Node,
    saved: Option<Credentials>,
    welcome: &Welcome,
) -> Option<Credentials> {
    let server = Some(node.server.clone());
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
    host: &Arc<Host>,
    state_dir: &std::path::Path,
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
                        tokio::spawn(prompt(host.clone(), task, outbound.clone(), cancelled));
                    }
                    Some(orchestrator_msg::Msg::ListOptions(ListAgentOptions { request_id, mcp })) => {
                        ask_harness(host, outbound, request_id, |h| async move { h.options(mcp).await });
                    }
                    Some(orchestrator_msg::Msg::ListSessions(ListAgentSessions { request_id })) => {
                        ask_harness(host, outbound, request_id, |h| async move {
                            let sessions = h.sessions().await?;
                            Ok(AgentSessions { sessions, ..Default::default() })
                        });
                    }
                    Some(orchestrator_msg::Msg::GetHistory(GetSessionHistory { request_id, session_id })) => {
                        ask_harness(host, outbound, request_id, |h| async move {
                            let entries = h.history(&session_id).await?;
                            Ok(SessionHistory { entries, ..Default::default() })
                        });
                    }
                    Some(orchestrator_msg::Msg::ListProviders(ListProviders { request_id })) => {
                        ask_harness(host, outbound, request_id, |h| async move {
                            let providers = h.providers().await?;
                            Ok(AgentProviders { providers, ..Default::default() })
                        });
                    }
                    Some(orchestrator_msg::Msg::ProviderAuth(ProviderAuth { request_id, provider, action })) => {
                        info!(%provider, "signing the agent in or out of a provider");
                        ask_harness(host, outbound, request_id, |h| async move {
                            h.authenticate(&provider, action).await
                        });
                    }
                    Some(orchestrator_msg::Msg::PrepareProject(PrepareProject { request_id, repository })) => {
                        info!(%repository, "making a copy of a project");
                        let state_dir = state_dir.to_path_buf();
                        answer(outbound, request_id, async move {
                            let (id, path) = project::prepare(&state_dir, &repository).await?;
                            let path = path.to_string_lossy().into_owned();
                            Ok(ProjectReady { id, path, ..Default::default() })
                        });
                    }
                    Some(orchestrator_msg::Msg::ListProjects(ListProjects { request_id })) => {
                        let (host, state_dir) = (host.clone(), state_dir.to_path_buf());
                        answer(outbound, request_id, async move {
                            // Without an agent, or if it can't say, copies just list no sessions.
                            let sessions = match host.current() {
                                Some(harness) => harness.sessions().await.unwrap_or_default(),
                                None => Vec::new(),
                            };
                            let listed = tokio::task::spawn_blocking(move || project::list(&state_dir, &sessions));
                            Ok(Projects { projects: listed.await?, ..Default::default() })
                        });
                    }
                    Some(orchestrator_msg::Msg::StartHarness(StartHarness { request_id, harness })) => {
                        let host = host.clone();
                        answer(outbound, request_id, async move { start_harness(&host, &harness).await });
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

/// Runs a prompt on the hosted harness and reports how it ended.
async fn prompt(
    host: Arc<Host>,
    task: AgentPrompt,
    outbound: mpsc::Sender<WorkerMsg>,
    cancel: oneshot::Receiver<()>,
) {
    let task_id = task.task_id.clone();
    let finished = async { harness(&host)?.prompt(task, &outbound, cancel).await }
        .await
        .unwrap_or_else(|e| TaskFinished {
            task_id,
            error: format!("{e:#}"),
            ..Default::default()
        });
    let _ = outbound.send(finished.into()).await;
}

/// The harness the worker hosts, for a question that needs one.
fn harness(host: &Host) -> anyhow::Result<Arc<dyn Harness>> {
    host.current().ok_or_else(|| anyhow!(NO_HARNESS))
}

/// Answers question `request_id` in the background with what the hosted
/// harness says, or why it can't.
fn ask_harness<T, F, Fut>(
    host: &Arc<Host>,
    outbound: &mpsc::Sender<WorkerMsg>,
    request_id: String,
    ask: F,
) where
    T: Reply + Send + 'static,
    F: FnOnce(Arc<dyn Harness>) -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<T>> + Send,
{
    let host = host.clone();
    answer(
        outbound,
        request_id,
        async move { ask(harness(&host)?).await },
    );
}

/// Answers question `request_id` in the background with what `answer` comes to.
fn answer<T: Reply + Send + 'static>(
    outbound: &mpsc::Sender<WorkerMsg>,
    request_id: String,
    answer: impl Future<Output = anyhow::Result<T>> + Send + 'static,
) {
    let outbound = outbound.clone();
    tokio::spawn(async move { reply(&outbound, request_id, answer.await).await });
}

/// Answers the orchestrator's question `request_id`: with `answer`, or with
/// why there is none.
async fn reply<T: Reply>(
    outbound: &mpsc::Sender<WorkerMsg>,
    request_id: String,
    answer: anyhow::Result<T>,
) {
    let mut answer = answer.unwrap_or_else(|e| {
        let mut failed = T::default();
        *failed.fields().1 = format!("{e:#}");
        failed
    });
    *answer.fields().0 = request_id;
    let _ = outbound.send(answer.into()).await;
}

/// Starts the harness a client asked for, until the worker stops.
async fn start_harness(host: &Host, name: &str) -> anyhow::Result<HarnessStarted> {
    let kind: HarnessKind = name.parse().map_err(|e: String| anyhow!(e))?;
    info!(harness = %kind, "starting a harness");
    if let Err(e) = host.start(kind).await {
        warn!("couldn't start {kind}: {e:#}");
        return Err(e);
    }
    Ok(HarnessStarted {
        harnesses: host.hosted(),
        ..Default::default()
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_is_refused() {
        assert!(normal_user(0).is_err());
        assert!(normal_user(1000).is_ok());
    }
}
