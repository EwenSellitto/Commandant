//! The OpenCode harness: installs `opencode` if needed, keeps its HTTP server
//! running on loopback, and relays prompts to it.

mod api;
mod options;
mod prompt;
mod sessions;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, info, warn};

use self::api::Api;
pub use self::options::list as list_options;
pub use self::prompt::run;
pub use self::sessions::list as list_sessions;

const INSTALL_SCRIPT: &str = "https://opencode.ai/install";
/// The first start can be slow: OpenCode fetches its plugins.
const START_TIMEOUT: Duration = Duration::from_secs(120);
/// What `opencode serve` prints once it accepts requests.
const LISTENING: &str = "listening on ";

pub struct Opencode {
    binary: PathBuf,
    server: Mutex<Option<Server>>,
    /// Sessions answering a prompt, which can't take another meanwhile.
    busy: std::sync::Mutex<HashSet<String>>,
}

/// Holds a session busy until dropped.
struct Running<'a> {
    opencode: &'a Opencode,
    session_id: String,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.opencode.busy.lock().unwrap().remove(&self.session_id);
    }
}

struct Server {
    child: Child,
    api: Api,
}

impl Opencode {
    /// Installs opencode if it is missing and starts its server.
    pub async fn start() -> Result<Self> {
        let binary = ensure_installed().await?;
        let opencode = Self {
            binary,
            server: Mutex::new(None),
            busy: Default::default(),
        };
        opencode.api().await?;
        Ok(opencode)
    }

    /// Marks a session busy, unless it already is: OpenCode would queue a
    /// second prompt, and the two replies would mix.
    fn claim(&self, session_id: &str) -> Result<Running<'_>> {
        let fresh = self.busy.lock().unwrap().insert(session_id.to_string());
        ensure!(
            fresh,
            "session {session_id} is already answering a prompt; wait for it, or cancel it"
        );
        Ok(Running {
            opencode: self,
            session_id: session_id.to_string(),
        })
    }

    fn is_busy(&self, session_id: &str) -> bool {
        self.busy.lock().unwrap().contains(session_id)
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

/// Runs `opencode serve` on a free loopback port, behind a random password.
async fn start_server(binary: &Path) -> Result<Server> {
    let password = random_password();
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
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            // SAFETY: plain syscall; the group id equals the child's pid.
            unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGTERM) };
        }
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

fn random_password() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("OS random number generator unavailable");
    hex::encode(bytes)
}

/// Finds opencode, installing it with the official script if needed.
async fn ensure_installed() -> Result<PathBuf> {
    if let Some(binary) = find_binary() {
        return Ok(binary);
    }
    info!("opencode not found; installing it from {INSTALL_SCRIPT}");
    install().await?;
    find_binary().context("the opencode installer finished, but no opencode binary was found")
}

/// `opencode` on the PATH, else where the installer puts it.
fn find_binary() -> Option<PathBuf> {
    let binary = format!("opencode{}", std::env::consts::EXE_SUFFIX);
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(std::env::home_dir().map(|home| home.join(".opencode").join("bin")))
        .map(|dir| dir.join(&binary))
        .find(|candidate| candidate.is_file())
}

/// Installs into `~/.opencode/bin`, leaving shell profiles alone.
#[cfg(unix)]
async fn install() -> Result<()> {
    let script =
        format!("set -o pipefail; curl -fsSL {INSTALL_SCRIPT} | bash -s -- --no-modify-path");
    let status = Command::new("bash")
        .args(["-c", &script])
        .stdin(Stdio::null())
        .status()
        .await
        .context("running the opencode installer (it needs bash and curl)")?;
    ensure!(status.success(), "the opencode installer failed ({status})");
    Ok(())
}

#[cfg(not(unix))]
async fn install() -> Result<()> {
    bail!("install opencode first (npm install -g opencode-ai), then restart the worker")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_takes_one_prompt_at_a_time() {
        let opencode = Opencode {
            binary: PathBuf::new(),
            server: Mutex::new(None),
            busy: Default::default(),
        };
        let running = opencode.claim("ses_a").unwrap();
        assert!(opencode.is_busy("ses_a"));
        assert!(opencode.claim("ses_a").is_err());
        // Other sessions run alongside.
        let other = opencode.claim("ses_b").unwrap();
        drop(running);
        assert!(!opencode.is_busy("ses_a"));
        assert!(opencode.claim("ses_a").is_ok());
        drop(other);
    }
}
