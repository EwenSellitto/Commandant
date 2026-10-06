//! The OpenCode harness: installs `opencode` if needed, keeps its HTTP server
//! running on loopback, and relays prompts to it.

mod api;
mod auth;
mod options;
mod prompt;
mod sessions;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock, mpsc};
use tracing::{debug, info, warn};

use commandant_proto::{
    AgentOptions, AgentPrompt, AgentSession, AuthAction, HistoryEntry, McpSwitch, ModelProvider,
    ProviderAuthResult, TaskFinished, WorkerMsg,
};
use tokio::sync::oneshot;

use crate::harness::{Busy, Harness, HarnessKind, ensure_installed};
use crate::process::{Signal, signal_group};

use self::api::Api;

/// Installs into `~/.opencode/bin`, leaving shell profiles alone.
const INSTALL: &str = "curl -fsSL https://opencode.ai/install | bash -s -- --no-modify-path";
/// The first start can be slow: OpenCode fetches its plugins.
const START_TIMEOUT: Duration = Duration::from_secs(120);
/// What `opencode serve` prints once it accepts requests.
const LISTENING: &str = "listening on ";

pub struct Opencode {
    binary: PathBuf,
    server: Mutex<Option<Server>>,
    /// Sessions answering a prompt: OpenCode would queue a second one, and
    /// the two replies would mix.
    busy: Busy,
    /// Set when credentials changed. OpenCode only reads them again on a
    /// reload, which aborts the prompts running, so it waits for none to be.
    stale: AtomicBool,
    /// Held shared by every running prompt, and exclusively by a reload.
    quiet: RwLock<()>,
}

struct Server {
    child: Child,
    api: Api,
}

impl Opencode {
    /// Starts the server of `binary`, else of the `opencode` found or
    /// installed.
    pub async fn start(binary: Option<PathBuf>) -> Result<Self> {
        let binary = match binary {
            Some(binary) => binary,
            None => ensure_installed("opencode", ".opencode/bin", INSTALL).await?,
        };
        let opencode = Self::new(binary);
        warm_up(opencode.api().await?);
        Ok(opencode)
    }

    fn new(binary: PathBuf) -> Self {
        Self {
            binary,
            server: Mutex::new(None),
            busy: Default::default(),
            stale: AtomicBool::new(false),
            quiet: RwLock::new(()),
        }
    }

    fn credentials_changed(&self) {
        self.stale.store(true, Ordering::Relaxed);
    }

    /// Reloads OpenCode if credentials changed and no prompt is running;
    /// otherwise a later call does.
    async fn reload_if_stale(&self) -> Result<()> {
        if !self.stale.load(Ordering::Relaxed) {
            return Ok(());
        }
        let Ok(_quiet) = self.quiet.try_write() else {
            return Ok(());
        };
        let api = self.api().await?;
        api.reload().await?;
        self.stale.store(false, Ordering::Relaxed);
        info!("reloaded opencode for its new credentials");
        warm_up(api);
        Ok(())
    }

    /// A client for the server, which is restarted if it has died.
    async fn api(&self) -> Result<Api> {
        let mut server = self.server.lock().await;
        if let Some(running) = server.as_mut() {
            if running.child.try_wait()?.is_none() {
                return Ok(running.api.clone());
            }
            warn!("the opencode server exited; restarting it");
        }
        let started = start_server(&self.binary).await?;
        let api = started.api.clone();
        *server = Some(started);
        Ok(api)
    }
}

#[tonic::async_trait]
impl Harness for Opencode {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Opencode
    }

    async fn prompt(
        &self,
        task: AgentPrompt,
        out: &mpsc::Sender<WorkerMsg>,
        cancel: oneshot::Receiver<()>,
    ) -> Result<TaskFinished> {
        prompt::converse(self, task, out, cancel).await
    }

    async fn options(&self, mcp: Option<McpSwitch>) -> Result<AgentOptions> {
        options::options(self, mcp).await
    }

    async fn sessions(&self) -> Result<Vec<AgentSession>> {
        sessions::list(self).await
    }

    async fn history(&self, session_id: &str) -> Result<Vec<HistoryEntry>> {
        sessions::history(self, session_id).await
    }

    async fn providers(&self) -> Result<Vec<ModelProvider>> {
        auth::providers(self).await
    }

    async fn authenticate(
        &self,
        provider: &str,
        action: Option<AuthAction>,
    ) -> Result<ProviderAuthResult> {
        auth::authenticate(self, provider, action).await
    }
}

/// Gets OpenCode connecting its MCP servers now, which listing the commands
/// waits for, rather than when a client first asks.
fn warm_up(api: Api) {
    tokio::spawn(async move {
        let _ = api.commands(None).await;
    });
}

/// Runs `opencode serve` on a free loopback port, behind a random password.
async fn start_server(binary: &Path) -> Result<Server> {
    let password = commandant_common::random_hex(32);
    let mut command = Command::new(binary);
    command
        .args(["serve", "--hostname", "127.0.0.1", "--port", "0"])
        .env("OPENCODE_SERVER_PASSWORD", &password)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Its own process group, so stopping it also stops what it spawned.
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .with_context(|| format!("starting {}", binary.display()))?;

    let (line_tx, mut lines) = mpsc::channel(64);
    tokio::spawn(forward_lines(
        child.stdout.take().expect("piped"),
        line_tx.clone(),
    ));
    tokio::spawn(forward_lines(child.stderr.take().expect("piped"), line_tx));
    let url = tokio::time::timeout(START_TIMEOUT, async {
        while let Some(line) = lines.recv().await {
            debug!(target: "opencode", "{line}");
            if let Some((_, url)) = line.split_once(LISTENING) {
                return Ok(url.trim().to_string());
            }
        }
        bail!("opencode exited before its server started")
    })
    .await
    .context("timed out waiting for the opencode server")??;
    tokio::spawn(async move {
        while let Some(line) = lines.recv().await {
            debug!(target: "opencode", "{line}");
        }
    });

    let api = Api::new(url.clone(), password);
    api.health().await?;
    info!(%url, "opencode server ready");
    Ok(Server { child, api })
}

impl Drop for Server {
    fn drop(&mut self) {
        signal_group(&self.child, Signal::Term);
    }
}

async fn forward_lines(pipe: impl AsyncRead + Unpin, lines: mpsc::Sender<String>) {
    let mut reader = BufReader::new(pipe).lines();
    while let Ok(Some(line)) = reader.next_line().await {
        if lines.send(line).await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_takes_one_prompt_at_a_time() {
        let opencode = Opencode::new(PathBuf::new());
        let running = opencode.busy.claim("ses_a").unwrap();
        assert!(opencode.busy.contains("ses_a"));
        assert!(opencode.busy.claim("ses_a").is_err());
        // Other sessions run alongside.
        let other = opencode.busy.claim("ses_b").unwrap();
        drop(running);
        assert!(!opencode.busy.contains("ses_a"));
        assert!(opencode.busy.claim("ses_a").is_ok());
        drop(other);
    }
}
