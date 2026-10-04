//! The Claude Code harness: runs the `claude` CLI itself, signed in with a
//! Claude subscription (or a token from `claude setup-token`), one process
//! per prompt, and relays what it streams.
//!
//! Everything goes through Claude Code, so its own login, settings, skills,
//! hooks and MCP servers apply as they would in a terminal.

mod sessions;
mod stream;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use commandant_proto::{
    AgentChoice, AgentCommand, AgentOptions, AgentPrompt, AgentSession, AuthAction, AuthMethod,
    HistoryEntry, McpServer, McpSwitch, ModelChoice, ModelProvider, ProviderAuthResult,
    TaskFinished, WorkerMsg, auth_action,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};

use crate::harness::{Busy, Harness, HarnessKind, ensure_installed};

/// The native installer, into `~/.local/bin`.
const INSTALL: &str = "curl -fsSL https://claude.ai/install.sh | bash";
/// Where the token pasted from `claude setup-token` is kept, in the worker's
/// state directory.
const TOKEN_FILE: &str = "claude-code.token";
/// How long Claude Code gets to say what it offers; it starts its MCP
/// servers and hooks first.
const OPTIONS_TIMEOUT: Duration = Duration::from_secs(12);

pub struct ClaudeCode {
    binary: PathBuf,
    token_file: PathBuf,
    /// Sessions answering a prompt: a second `claude --resume` on one would
    /// fork its transcript.
    busy: Busy,
}

impl ClaudeCode {
    pub async fn start(binary: Option<PathBuf>, state_dir: &Path) -> Result<Self> {
        let binary = match binary {
            Some(binary) => binary,
            None => ensure_installed("claude", ".local/bin", INSTALL).await?,
        };
        let version = Command::new(&binary)
            .arg("--version")
            .output()
            .await
            .with_context(|| format!("running {}", binary.display()))?;
        ensure!(
            version.status.success(),
            "{} --version failed",
            binary.display()
        );
        Ok(Self {
            binary,
            token_file: state_dir.join(TOKEN_FILE),
            busy: Busy::default(),
        })
    }

    /// `claude` in `directory`, signed in with the saved token if there is one
    /// (else with whatever login Claude Code has on this machine), so the
    /// subscription pays, never an API key.
    fn command(&self, directory: &Path) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .current_dir(directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // These would take over from the subscription and bill by the token.
        for key in ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"] {
            command.env_remove(key);
        }
        if let Some(token) = self.token() {
            command.env("CLAUDE_CODE_OAUTH_TOKEN", token);
        }
        // Its own process group, so cancelling also stops what it started.
        #[cfg(unix)]
        command.process_group(0);
        command
    }

    fn token(&self) -> Option<String> {
        commandant_common::fs::read_setting(&self.token_file)
            .ok()
            .flatten()
    }

    /// Whether Claude Code is signed in, by the saved token or its own login.
    async fn signed_in(&self) -> Result<bool> {
        if self.token().is_some() {
            return Ok(true);
        }
        let output = self
            .command(&std::env::current_dir()?)
            .args(["auth", "status", "--json"])
            .stdin(Stdio::null())
            .output()
            .await?;
        let status: Value = serde_json::from_slice(&output.stdout).unwrap_or_default();
        // A Console login bills by the token, so it doesn't count.
        Ok(status["loggedIn"] == true && status["authMethod"] == "claude.ai")
    }

    /// Asks a `claude` that is given no prompt what it offers: its answer to
    /// the SDK's `initialize` and `mcp_status` requests, which spend nothing.
    async fn describe(&self) -> Result<(Value, Value)> {
        let mut child = self
            .command(&std::env::current_dir()?)
            .args(["-p", "--input-format", "stream-json", "--output-format"])
            .args(["stream-json", "--verbose", "--no-session-persistence"])
            .stderr(Stdio::null())
            .spawn()
            .context("starting claude")?;
        let mut stdin = child.stdin.take().expect("piped");
        for (id, subtype) in [("init", "initialize"), ("mcp", "mcp_status")] {
            let request = json!({ "type": "control_request", "request_id": id,
                                  "request": { "subtype": subtype } });
            stdin.write_all(format!("{request}\n").as_bytes()).await?;
        }
        let mut lines = BufReader::new(child.stdout.take().expect("piped")).lines();
        let (mut init, mut mcp) = (None, None);
        let read = async {
            while let Some(line) = lines.next_line().await? {
                let Ok(event) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let response = &event["response"];
                if event["type"] != "control_response" {
                    continue;
                }
                if response["subtype"] == "error" {
                    bail!("claude: {}", response["error"]);
                }
                match response["request_id"].as_str() {
                    Some("init") => init = Some(response["response"].clone()),
                    Some("mcp") => mcp = Some(response["response"].clone()),
                    _ => {}
                }
                if init.is_some() && mcp.is_some() {
                    return Ok(());
                }
            }
            bail!("claude exited before saying what it offers; is it signed in?")
        };
        tokio::time::timeout(OPTIONS_TIMEOUT, read)
            .await
            .context("claude took too long to say what it offers")??;
        Ok((init.unwrap_or_default(), mcp.unwrap_or_default()))
    }
}

#[tonic::async_trait]
impl Harness for ClaudeCode {
    fn kind(&self) -> HarnessKind {
        HarnessKind::ClaudeCode
    }

    async fn prompt(
        &self,
        task: AgentPrompt,
        out: &mpsc::Sender<WorkerMsg>,
        cancel: oneshot::Receiver<()>,
    ) -> Result<TaskFinished> {
        stream::converse(self, task, out, cancel).await
    }

    async fn options(&self, mcp: Option<McpSwitch>) -> Result<AgentOptions> {
        if mcp.is_some() {
            bail!("Claude Code's MCP servers are switched with `claude mcp` on the node");
        }
        let (init, mcp) = self.describe().await?;
        Ok(options(&init, &mcp))
    }

    async fn sessions(&self) -> Result<Vec<AgentSession>> {
        let projects = sessions::projects()?;
        let mut listed = tokio::task::spawn_blocking(move || sessions::list(&projects)).await??;
        for session in &mut listed {
            session.busy = self.busy.contains(&session.id);
        }
        Ok(listed)
    }

    async fn history(&self, session_id: &str) -> Result<Vec<HistoryEntry>> {
        let (projects, id) = (sessions::projects()?, session_id.to_string());
        tokio::task::spawn_blocking(move || sessions::history(&projects, &id)).await?
    }

    async fn providers(&self) -> Result<Vec<ModelProvider>> {
        Ok(vec![ModelProvider {
            id: "claude".into(),
            name: "Claude subscription".into(),
            connected: self.signed_in().await?,
            methods: vec![AuthMethod {
                label: "Paste a token from `claude setup-token`".into(),
                ..Default::default()
            }],
        }])
    }

    async fn authenticate(
        &self,
        _provider: &str,
        action: Option<AuthAction>,
    ) -> Result<ProviderAuthResult> {
        match action.and_then(|a| a.action) {
            Some(auth_action::Action::ApiKey(token)) => {
                let token = token.trim();
                ensure!(!token.is_empty(), "the token is empty");
                commandant_common::fs::write_private(&self.token_file, &format!("{token}\n"))?;
            }
            Some(auth_action::Action::SignOut(_)) if self.token().is_some() => {
                std::fs::remove_file(&self.token_file)?;
            }
            Some(auth_action::Action::SignOut(_)) => {
                let status = self
                    .command(&std::env::current_dir()?)
                    .args(["auth", "logout"])
                    .stdin(Stdio::null())
                    .status()
                    .await?;
                ensure!(status.success(), "claude auth logout failed ({status})");
            }
            _ => bail!(
                "Claude Code signs in with a token: run `claude setup-token` where a browser is, and paste what it prints"
            ),
        }
        Ok(ProviderAuthResult::default())
    }
}

#[derive(Deserialize)]
struct Init {
    #[serde(default)]
    agents: Vec<Named>,
    #[serde(default)]
    commands: Vec<Named>,
    #[serde(default)]
    models: Vec<Model>,
}

#[derive(Deserialize)]
struct Named {
    name: String,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
struct Model {
    value: String,
    #[serde(rename = "displayName", default)]
    display_name: String,
    #[serde(rename = "supportedEffortLevels", default)]
    efforts: Option<Vec<String>>,
}

/// What a prompt can choose, from Claude Code's answers. Its `default` model
/// is left out: no model at all means the same.
fn options(init: &Value, mcp: &Value) -> AgentOptions {
    let init = Init::deserialize(init).unwrap_or(Init {
        agents: Vec::new(),
        commands: Vec::new(),
        models: Vec::new(),
    });
    let mcp_servers = mcp["mcpServers"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|server| McpServer {
            name: server["name"].as_str().unwrap_or_default().into(),
            status: server["status"]
                .as_str()
                .unwrap_or_default()
                .replace('-', "_"),
            error: server["error"].as_str().unwrap_or_default().into(),
        })
        .collect();
    AgentOptions {
        agents: init
            .agents
            .into_iter()
            .map(|a| AgentChoice {
                name: a.name,
                description: a.description,
            })
            .collect(),
        models: init
            .models
            .into_iter()
            .filter(|m| m.value != "default")
            .map(|m| ModelChoice {
                name: commandant_common::or(&m.display_name, &m.value).to_string(),
                id: m.value,
                provider: "Anthropic".into(),
                variants: m.efforts.unwrap_or_default(),
                context: 0,
            })
            .collect(),
        commands: init
            .commands
            .into_iter()
            .map(|c| AgentCommand {
                name: c.name,
                description: c.description,
                source: "command".into(),
            })
            .collect(),
        mcp_servers,
        ..Default::default()
    }
}

/// A random version-4 UUID, as Claude Code wants its session ids.
fn new_session_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("OS random number generator unavailable");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = hex::encode(bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_come_from_claudes_own_answers() {
        let init = json!({
            "agents": [{ "name": "Explore", "description": "Searches" }],
            "commands": [{ "name": "review", "description": "Review a PR", "argumentHint": "" }],
            "models": [
                { "value": "default", "displayName": "Default (recommended)",
                  "supportedEffortLevels": ["low", "high"] },
                { "value": "sonnet", "displayName": "Sonnet", "supportedEffortLevels": ["low", "high"] },
                { "value": "haiku", "displayName": "Haiku" },
            ],
            "account": { "email": "someone@example.com" },
        });
        let mcp = json!({ "mcpServers": [
            { "name": "docs", "status": "needs-auth", "config": { "headers": { "Authorization": "secret" } } },
        ]});
        let options = options(&init, &mcp);
        assert_eq!(options.agents[0].name, "Explore");
        let models: Vec<_> = options.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(models, ["sonnet", "haiku"]);
        assert_eq!(options.models[0].variants, ["low", "high"]);
        assert!(options.models[1].variants.is_empty());
        assert_eq!(options.commands[0].name, "review");
        assert_eq!(options.mcp_servers[0].status, "needs_auth");
        assert!(!format!("{options:?}").contains("secret"));
        assert!(options.default_model.is_empty());
    }

    #[test]
    fn session_ids_are_v4_uuids() {
        let id = new_session_id();
        let parts: Vec<_> = id.split('-').map(str::len).collect();
        assert_eq!(parts, [8, 4, 4, 4, 12]);
        assert_eq!(&id[14..15], "4");
        assert_ne!(id, new_session_id());
    }
}
