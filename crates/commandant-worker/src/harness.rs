//! The coding-agent harnesses a worker can host, behind one interface.
//!
//! A harness is a coding agent (OpenCode, say) that the worker keeps
//! running and relays prompts to. Adding one means a [`HarnessKind`] variant,
//! an implementation of [`Harness`], and a line in [`start`]; the rest of the
//! worker, the orchestrator and the clients only see the trait and the name.

use std::collections::HashSet;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail, ensure};
use commandant_proto::{
    AgentOptions, AgentPrompt, AgentSession, AuthAction, CAN_HOST, HOSTS, HistoryEntry, McpSwitch,
    ModelProvider, OutputStream, ProviderAuthResult, TaskFinished, TaskOutput, WorkerMsg,
};
use tokio::sync::{mpsc, oneshot};
use tracing::info;

use crate::claude::ClaudeCode;
use crate::opencode::Opencode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessKind {
    Opencode,
    ClaudeCode,
}

impl HarnessKind {
    /// Every harness this worker can host.
    pub const ALL: [HarnessKind; 2] = [HarnessKind::Opencode, HarnessKind::ClaudeCode];

    pub fn name(self) -> &'static str {
        match self {
            Self::Opencode => "opencode",
            Self::ClaudeCode => "claude-code",
        }
    }

    /// One line on what it is, for choosing one.
    pub fn description(self) -> &'static str {
        match self {
            Self::Opencode => "OpenCode, installed on the node if it isn't there",
            Self::ClaudeCode => {
                "Claude Code on your Claude subscription, installed if it isn't there"
            }
        }
    }

    /// How the worker announces the harness it hosts in its hello.
    pub fn capability(self) -> String {
        format!("{HOSTS}{}", self.name())
    }

    /// How the worker announces a harness it could start.
    pub fn hostable(self) -> String {
        format!("{CAN_HOST}{}", self.name())
    }
}

impl fmt::Display for HarnessKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for HarnessKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.name() == s)
            .ok_or_else(|| {
                let names: Vec<_> = Self::ALL.iter().map(|k| k.name()).collect();
                format!("unknown harness {s:?} (supported: {})", names.join(", "))
            })
    }
}

/// A coding agent the worker hosts. Each prompt is its own task, and any
/// number may run at once.
#[tonic::async_trait]
pub trait Harness: Send + Sync {
    fn kind(&self) -> HarnessKind;

    /// Runs one prompt, streaming the reply on `out` as `TaskOutput` (text on
    /// stdout, thinking on the reasoning stream, tool activity on stderr),
    /// until the agent is done or `cancel` fires or is dropped. The worker
    /// sends the result.
    async fn prompt(
        &self,
        task: AgentPrompt,
        out: &mpsc::Sender<WorkerMsg>,
        cancel: oneshot::Receiver<()>,
    ) -> Result<TaskFinished>;

    /// What a prompt can choose from, after connecting or disconnecting an
    /// MCP server if `mcp` asks to. The worker fills in the request id.
    async fn options(&self, mcp: Option<McpSwitch>) -> Result<AgentOptions>;

    /// The sessions it has saved, latest first.
    async fn sessions(&self) -> Result<Vec<AgentSession>>;

    /// What was said in a saved session, oldest first.
    async fn history(&self, session_id: &str) -> Result<Vec<HistoryEntry>>;

    /// The model providers it knows, and how to sign in to each.
    async fn providers(&self) -> Result<Vec<ModelProvider>>;

    /// One step of signing in to (or out of) a provider.
    async fn authenticate(
        &self,
        provider: &str,
        action: Option<AuthAction>,
    ) -> Result<ProviderAuthResult>;
}

/// How many of a session's latest entries its history keeps, well inside a
/// gRPC message.
const HISTORY: usize = 300;

/// The latest `HISTORY` entries of a session's history.
// ponytail: drops the oldest; page through them if anyone scrolls that far.
pub fn keep_latest<T>(mut entries: Vec<T>) -> Vec<T> {
    let skip = entries.len().saturating_sub(HISTORY);
    entries.split_off(skip)
}

/// Where a prompt's reply goes: the agent's text on stdout, its thinking on
/// the reasoning stream, and notes on what it does on stderr as
/// `[<harness>] …` lines, each stream's lines kept whole.
pub struct Output<'a> {
    task_id: &'a str,
    harness: HarnessKind,
    out: &'a mpsc::Sender<WorkerMsg>,
    /// The stream whose last line is unfinished.
    mid_line: Option<OutputStream>,
}

impl<'a> Output<'a> {
    pub fn new(task_id: &'a str, harness: HarnessKind, out: &'a mpsc::Sender<WorkerMsg>) -> Self {
        Self {
            task_id,
            harness,
            out,
            mid_line: None,
        }
    }

    /// Writes reply text or thinking, on a new line if the other was
    /// unfinished.
    pub async fn say(&mut self, stream: OutputStream, text: String) {
        if text.is_empty() {
            return;
        }
        if self.mid_line.is_some_and(|s| s != stream) {
            self.end_line().await;
        }
        self.mid_line = (!text.ends_with('\n')).then_some(stream);
        self.write(stream, text).await;
    }

    /// Finishes the unfinished line, if any.
    pub async fn end_line(&mut self) {
        if let Some(stream) = self.mid_line.take() {
            self.write(stream, "\n".into()).await;
        }
    }

    /// A line about what the agent is doing.
    pub async fn note(&mut self, line: &str) {
        self.end_line().await;
        let line = format!("[{}] {line}\n", self.harness);
        self.write(OutputStream::Stderr, line).await;
    }

    async fn write(&self, stream: OutputStream, text: String) {
        let output = TaskOutput {
            task_id: self.task_id.to_string(),
            stream: stream.into(),
            data: text.into_bytes(),
        };
        let _ = self.out.send(output.into()).await;
    }
}

/// Sessions answering a prompt, which take no other meanwhile.
#[derive(Default)]
pub struct Busy(Mutex<HashSet<String>>);

/// Holds a session busy until dropped.
pub struct Claim<'a> {
    busy: &'a Busy,
    session_id: String,
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        self.busy.0.lock().unwrap().remove(&self.session_id);
    }
}

impl Busy {
    /// Marks a session busy, unless it already is.
    pub fn claim(&self, session_id: &str) -> Result<Claim<'_>> {
        let fresh = self.0.lock().unwrap().insert(session_id.to_string());
        ensure!(
            fresh,
            "session {session_id} is already answering a prompt; wait for it, or cancel it"
        );
        Ok(Claim {
            busy: self,
            session_id: session_id.to_string(),
        })
    }

    pub fn contains(&self, session_id: &str) -> bool {
        self.0.lock().unwrap().contains(session_id)
    }
}

/// Finds `name` on the PATH or in `~/<installed_in>`, else installs it with
/// the shell command `install` (needs bash).
pub async fn ensure_installed(name: &str, installed_in: &str, install: &str) -> Result<PathBuf> {
    if let Some(binary) = find_binary(name, installed_in) {
        return Ok(binary);
    }
    info!("{name} not found; installing it with: {install}");
    #[cfg(unix)]
    {
        let status = tokio::process::Command::new("bash")
            .args(["-c", &format!("set -o pipefail; {install}")])
            .stdin(std::process::Stdio::null())
            .status()
            .await
            .with_context(|| format!("running the {name} installer (it needs bash and curl)"))?;
        ensure!(status.success(), "the {name} installer failed ({status})");
    }
    #[cfg(not(unix))]
    bail!("install {name} first, then restart the worker");
    #[allow(unreachable_code)]
    find_binary(name, installed_in)
        .with_context(|| format!("the {name} installer finished, but no {name} binary was found"))
}

fn find_binary(name: &str, installed_in: &str) -> Option<PathBuf> {
    let binary = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(std::env::home_dir().map(|home| home.join(installed_in)))
        .map(|dir| dir.join(&binary))
        .find(|candidate| candidate.is_file())
}

/// Starts a harness, installing it first if need be.
async fn start(
    kind: HarnessKind,
    harness_bin: Option<PathBuf>,
    state_dir: PathBuf,
) -> Result<Arc<dyn Harness>> {
    Ok(match kind {
        HarnessKind::Opencode => Arc::new(Opencode::start(harness_bin).await?),
        HarnessKind::ClaudeCode => Arc::new(ClaudeCode::start(harness_bin, &state_dir).await?),
    })
}

/// What the worker hosts. It outlives connections.
#[derive(Default)]
enum Hosting {
    #[default]
    Nothing,
    /// Being started, which may take a while.
    Starting(HarnessKind),
    Running(Arc<dyn Harness>),
}

/// The harness the worker hosts, if any: started with `--harness`, or later
/// when a client asks.
#[derive(Default)]
pub struct Host {
    hosting: Mutex<Hosting>,
    harness_bin: Option<PathBuf>,
    /// The worker's own directory, for what a harness keeps (credentials).
    state_dir: PathBuf,
}

impl Host {
    pub fn new(harness_bin: Option<PathBuf>, state_dir: PathBuf) -> Self {
        Self {
            hosting: Mutex::default(),
            harness_bin,
            state_dir,
        }
    }

    pub fn current(&self) -> Option<Arc<dyn Harness>> {
        match &*self.hosting.lock().unwrap() {
            Hosting::Running(harness) => Some(harness.clone()),
            _ => None,
        }
    }

    /// The names of what it hosts, as clients see them.
    pub fn hosted(&self) -> Vec<String> {
        self.current()
            .map(|h| h.kind().name().to_string())
            .into_iter()
            .collect()
    }

    /// Starts `kind` unless it is already hosted. A worker hosts one
    /// harness, so another one is refused, as is a second start at once.
    pub async fn start(&self, kind: HarnessKind) -> Result<()> {
        {
            let mut hosting = self.hosting.lock().unwrap();
            match &*hosting {
                Hosting::Running(current) if current.kind() == kind => return Ok(()),
                Hosting::Running(current) => bail!("this node already hosts {}", current.kind()),
                Hosting::Starting(other) => bail!("this node is already starting {other}"),
                Hosting::Nothing => *hosting = Hosting::Starting(kind),
            }
        }
        let started = start(kind, self.harness_bin.clone(), self.state_dir.clone()).await;
        let mut hosting = self.hosting.lock().unwrap();
        match started {
            Ok(harness) => {
                *hosting = Hosting::Running(harness);
                Ok(())
            }
            Err(e) => {
                *hosting = Hosting::Nothing;
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for kind in HarnessKind::ALL {
            assert_eq!(kind.name().parse::<HarnessKind>(), Ok(kind));
            assert!(!kind.description().is_empty());
        }
        let err = "claude".parse::<HarnessKind>().unwrap_err();
        assert!(err.contains("supported: opencode"), "{err}");
    }

    #[tokio::test]
    async fn a_host_refuses_a_second_start_and_keeps_none_on_failure() {
        let host = Host::new(Some(PathBuf::from("/nonexistent/opencode")), PathBuf::new());
        assert!(host.start(HarnessKind::Opencode).await.is_err());
        assert!(host.current().is_none());
        assert!(host.hosted().is_empty());
        // A failed start doesn't block the next one...
        assert!(matches!(*host.hosting.lock().unwrap(), Hosting::Nothing));

        // ...but one under way does.
        *host.hosting.lock().unwrap() = Hosting::Starting(HarnessKind::Opencode);
        let err = host.start(HarnessKind::Opencode).await.unwrap_err();
        assert!(err.to_string().contains("already starting"), "{err}");
    }
}
