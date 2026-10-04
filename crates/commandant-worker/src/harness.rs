//! The coding-agent harnesses a worker can host, behind one interface.
//!
//! A harness is a coding agent (OpenCode, say) that the worker keeps
//! running and relays prompts to. Adding one means a [`HarnessKind`] variant,
//! an implementation of [`Harness`], and a line in [`start`]; the rest of the
//! worker, the orchestrator and the clients only see the trait and the name.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use commandant_proto::{
    AgentOptions, AgentPrompt, AgentSession, CAN_HOST, HOSTS, McpSwitch, TaskFinished, WorkerMsg,
};
use tokio::sync::{mpsc, oneshot};

use crate::opencode::Opencode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessKind {
    Opencode,
}

impl HarnessKind {
    /// Every harness this worker can host.
    pub const ALL: [HarnessKind; 1] = [HarnessKind::Opencode];

    pub fn name(self) -> &'static str {
        match self {
            Self::Opencode => "opencode",
        }
    }

    /// One line on what it is, for choosing one.
    pub fn description(self) -> &'static str {
        match self {
            Self::Opencode => "OpenCode, installed on the node if it isn't there",
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
}

/// Starts a harness, installing it first if need be.
async fn start(kind: HarnessKind, opencode_bin: Option<PathBuf>) -> Result<Arc<dyn Harness>> {
    Ok(match kind {
        HarnessKind::Opencode => Arc::new(Opencode::start(opencode_bin).await?),
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
    opencode_bin: Option<PathBuf>,
}

impl Host {
    pub fn new(opencode_bin: Option<PathBuf>) -> Self {
        Self {
            hosting: Mutex::default(),
            opencode_bin,
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
        let started = start(kind, self.opencode_bin.clone()).await;
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
        let host = Host::new(Some(PathBuf::from("/nonexistent/opencode")));
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
