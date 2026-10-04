//! A client for the OpenCode server's HTTP API.
//!
//! Every call names the directory it works in: one server serves any number
//! of projects.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The HTTP basic auth user OpenCode expects.
const USER: &str = "opencode";
const HEALTH_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct Api {
    http: reqwest::Client,
    base: String,
    password: String,
    /// Set once the server turns out to predate `/experimental/session`.
    legacy_sessions: Arc<AtomicBool>,
}

/// A message to send to an agent.
#[derive(Serialize)]
pub struct Prompt<'a> {
    parts: [TextPart<'a>; 1],
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<ModelRef<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<&'a str>,
    /// The model's thinking effort, e.g. `high`.
    #[serde(skip_serializing_if = "Option::is_none")]
    variant: Option<&'a str>,
}

#[derive(Serialize)]
struct TextPart<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    text: &'a str,
}

#[derive(Serialize)]
struct ModelRef<'a> {
    #[serde(rename = "providerID")]
    provider_id: &'a str,
    #[serde(rename = "modelID")]
    model_id: &'a str,
}

impl<'a> Prompt<'a> {
    /// `model` is `provider/model`; empty strings mean the defaults.
    pub fn new(text: &'a str, model: &'a str, agent: &'a str, variant: &'a str) -> Result<Self> {
        let model = match model {
            "" => None,
            _ => {
                let Some((provider_id, model_id)) = model.split_once('/') else {
                    bail!("model must look like provider/model, got {model:?}");
                };
                Some(ModelRef {
                    provider_id,
                    model_id,
                })
            }
        };
        Ok(Self {
            parts: [TextPart { kind: "text", text }],
            model,
            agent: Some(agent).filter(|a| !a.is_empty()),
            variant: Some(variant).filter(|v| !v.is_empty()),
        })
    }
}

/// A command to run in a session: one of OpenCode's, a skill, or an MCP
/// prompt, with the prompt as its arguments.
#[derive(Serialize)]
pub struct CommandRun<'a> {
    command: &'a str,
    arguments: &'a str,
    /// `provider/model`, as it is here.
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    variant: Option<&'a str>,
}

impl<'a> CommandRun<'a> {
    pub fn new(
        command: &'a str,
        arguments: &'a str,
        model: &'a str,
        agent: &'a str,
        variant: &'a str,
    ) -> Self {
        let set = |s: &'a str| Some(s).filter(|s| !s.is_empty());
        Self {
            command,
            arguments,
            model: set(model),
            agent: set(agent),
            variant: set(variant),
        }
    }
}

// OpenCode sends `null` for unset fields, hence the options.

#[derive(Deserialize)]
pub struct Command {
    pub name: String,
    pub description: Option<String>,
    /// `command`, `skill` or `mcp`.
    pub source: Option<String>,
}

#[derive(Deserialize)]
pub struct McpStatus {
    /// `connected`, `disabled`, `failed`, `needs_auth`...
    pub status: String,
    pub error: Option<String>,
}

#[derive(Deserialize)]
pub struct Agent {
    pub name: String,
    pub description: Option<String>,
    /// `primary`, `subagent` or `all`.
    pub mode: String,
    pub hidden: Option<bool>,
}

#[derive(Deserialize)]
pub struct Providers {
    pub providers: Vec<Provider>,
}

#[derive(Deserialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub models: HashMap<String, Model>,
}

#[derive(Deserialize)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub status: Option<String>,
    /// Keyed by effort name; the values are provider settings.
    pub variants: Option<HashMap<String, serde_json::Value>>,
    pub limit: Option<Limit>,
}

#[derive(Deserialize)]
pub struct Limit {
    /// The context window, in tokens.
    pub context: Option<u64>,
}

#[derive(Deserialize)]
pub struct AllProviders {
    pub all: Vec<ProviderName>,
    pub connected: Vec<String>,
}

#[derive(Deserialize)]
pub struct ProviderName {
    pub id: String,
    pub name: String,
}

#[derive(Deserialize)]
pub struct AuthMethod {
    /// `oauth` or `api`.
    #[serde(rename = "type")]
    pub kind: String,
    pub label: String,
    /// What it asks before starting.
    #[serde(default)]
    pub prompts: Vec<AuthPrompt>,
}

#[derive(Deserialize)]
pub struct AuthPrompt {
    pub key: String,
    /// A select's choices; a text prompt has none.
    #[serde(default)]
    pub options: Vec<AuthOption>,
}

#[derive(Deserialize)]
pub struct AuthOption {
    pub value: String,
}

/// Where to sign in, and whether the page then shows a code to paste back
/// (`code`) or OpenCode notices by itself (`auto`).
#[derive(Deserialize)]
pub struct Authorization {
    pub url: String,
    pub method: String,
    #[serde(default)]
    pub instructions: String,
}

/// The parts of OpenCode's configuration that pick defaults.
#[derive(Deserialize)]
pub struct Config {
    pub model: Option<String>,
    pub default_agent: Option<String>,
}

#[derive(Deserialize)]
struct Session {
    id: String,
    directory: String,
}

/// A session as listed: what it is about and what it last used.
#[derive(Deserialize)]
pub struct SavedSession {
    pub id: String,
    #[serde(default)]
    pub title: String,
    pub directory: String,
    /// Set on a subagent's session.
    #[serde(rename = "parentID")]
    pub parent_id: Option<String>,
    pub agent: Option<String>,
    pub model: Option<SessionModel>,
    #[serde(default)]
    pub cost: f64,
    pub time: SessionTime,
}

#[derive(Deserialize)]
pub struct SessionModel {
    pub id: String,
    #[serde(rename = "providerID")]
    pub provider_id: String,
    pub variant: Option<String>,
}

#[derive(Deserialize)]
pub struct SessionTime {
    /// Unix milliseconds.
    pub updated: i64,
}

/// A message as saved: who wrote it, and what it is made of.
#[derive(Deserialize)]
pub struct SavedMessage {
    pub info: MessageRole,
    #[serde(default)]
    pub parts: Vec<SavedPart>,
}

#[derive(Deserialize)]
pub struct MessageRole {
    /// `user` or `assistant`.
    pub role: String,
}

#[derive(Deserialize)]
pub struct SavedPart {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub text: String,
    /// Text OpenCode added itself, like a file's contents.
    #[serde(default)]
    pub synthetic: Option<bool>,
    #[serde(default)]
    pub tool: String,
    pub state: Option<SavedToolState>,
}

#[derive(Deserialize)]
pub struct SavedToolState {
    pub status: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

impl Api {
    pub fn new(base: String, password: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            base,
            password,
            legacy_sessions: Arc::default(),
        }
    }

    fn request(&self, method: Method, path: &str, directory: Option<&str>) -> RequestBuilder {
        let request = self
            .http
            .request(method, format!("{}{path}", self.base))
            .basic_auth(USER, Some(&self.password));
        match directory {
            Some(directory) => request.query(&[("directory", directory)]),
            None => request,
        }
    }

    /// GETs `path`, outside any directory, and reads the JSON answer.
    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        json(self.request(Method::GET, path, None)).await
    }

    pub async fn health(&self) -> Result<()> {
        call(
            self.request(Method::GET, "/global/health", None)
                .timeout(HEALTH_TIMEOUT),
        )
        .await
    }

    /// Starts a session in `directory` and returns its id.
    pub async fn create_session(&self, directory: &str) -> Result<String> {
        let request = self.request(Method::POST, "/session", Some(directory));
        let session: Session = json(request.json(&json!({}))).await?;
        Ok(session.id)
    }

    /// The directory an existing session works in.
    pub async fn session_directory(&self, session_id: &str) -> Result<String> {
        let session: Session = self.get(&format!("/session/{session_id}")).await?;
        Ok(session.directory)
    }

    /// The latest top-level sessions, in every directory.
    pub async fn sessions(&self, limit: usize) -> Result<Vec<SavedSession>> {
        let limit = limit.to_string();
        let query = [("roots", "true"), ("limit", limit.as_str())];
        let list = |path| self.request(Method::GET, path, None).query(&query);
        // Across projects; servers without it list the current project's.
        if !self.legacy_sessions.load(Ordering::Relaxed) {
            let response = list("/experimental/session")
                .send()
                .await
                .context("reaching the opencode server")?;
            if response.status() != StatusCode::NOT_FOUND {
                return Ok(check(response).await?.json().await?);
            }
            self.legacy_sessions.store(true, Ordering::Relaxed);
        }
        json(list("/session")).await
    }

    /// A session's messages with their parts, oldest first.
    pub async fn messages(&self, session_id: &str) -> Result<Vec<SavedMessage>> {
        self.get(&format!("/session/{session_id}/message")).await
    }

    /// Every provider OpenCode knows, and the ids of those with credentials.
    pub async fn all_providers(&self) -> Result<AllProviders> {
        self.get("/provider").await
    }

    /// How each provider that has its own sign-in can be signed in to; any
    /// other takes an API key.
    pub async fn auth_methods(&self) -> Result<HashMap<String, Vec<AuthMethod>>> {
        self.get("/provider/auth").await
    }

    /// Starts the provider's OAuth method `method`.
    pub async fn oauth_authorize(
        &self,
        provider: &str,
        method: u32,
        inputs: &HashMap<String, String>,
    ) -> Result<Authorization> {
        let path = format!("/provider/{provider}/oauth/authorize");
        let body = json!({ "method": method, "inputs": inputs });
        json(self.request(Method::POST, &path, None).json(&body)).await
    }

    /// Finishes it, with the code the provider showed, if it asked for one;
    /// without, this waits for the user to be done.
    pub async fn oauth_callback(&self, provider: &str, method: u32, code: &str) -> Result<()> {
        let path = format!("/provider/{provider}/oauth/callback");
        let mut body = json!({ "method": method });
        if !code.is_empty() {
            body["code"] = code.into();
        }
        call(self.request(Method::POST, &path, None).json(&body)).await
    }

    pub async fn set_api_key(&self, provider: &str, key: &str) -> Result<()> {
        let body = json!({ "type": "api", "key": key });
        call(
            self.request(Method::PUT, &format!("/auth/{provider}"), None)
                .json(&body),
        )
        .await
    }

    pub async fn remove_auth(&self, provider: &str) -> Result<()> {
        call(self.request(Method::DELETE, &format!("/auth/{provider}"), None)).await
    }

    /// Drops every project's loaded state, so credentials are read again.
    /// It aborts the prompts running meanwhile.
    pub async fn reload(&self) -> Result<()> {
        call(self.request(Method::POST, "/global/dispose", None)).await
    }

    pub async fn agents(&self) -> Result<Vec<Agent>> {
        self.get("/agent").await
    }

    /// The providers that are set up, with their models.
    pub async fn providers(&self) -> Result<Providers> {
        self.get("/config/providers").await
    }

    pub async fn config(&self) -> Result<Config> {
        self.get("/config").await
    }

    /// The commands a prompt can run in `directory`, skills included.
    pub async fn commands(&self, directory: Option<&str>) -> Result<Vec<Command>> {
        json(self.request(Method::GET, "/command", directory)).await
    }

    /// Every MCP server that is set up, by name.
    pub async fn mcp_servers(&self) -> Result<HashMap<String, McpStatus>> {
        self.get("/mcp").await
    }

    pub async fn switch_mcp_server(&self, name: &str, connect: bool) -> Result<()> {
        let action = if connect { "connect" } else { "disconnect" };
        call(self.request(Method::POST, &format!("/mcp/{name}/{action}"), None)).await
    }

    /// Runs a command; it returns once the agent is done, while the reply
    /// arrives as events.
    pub async fn command(
        &self,
        session_id: &str,
        directory: &str,
        command: &CommandRun<'_>,
    ) -> Result<()> {
        let path = format!("/session/{session_id}/command");
        call(
            self.request(Method::POST, &path, Some(directory))
                .json(command),
        )
        .await
    }

    /// Queues a prompt; the reply arrives as events.
    pub async fn prompt(
        &self,
        session_id: &str,
        directory: &str,
        prompt: &Prompt<'_>,
    ) -> Result<()> {
        let path = format!("/session/{session_id}/prompt_async");
        call(
            self.request(Method::POST, &path, Some(directory))
                .json(prompt),
        )
        .await
    }

    pub async fn abort(&self, session_id: &str, directory: &str) -> Result<()> {
        let path = format!("/session/{session_id}/abort");
        call(self.request(Method::POST, &path, Some(directory))).await
    }

    /// Answers a permission request: `once`, `always` or `reject`.
    pub async fn reply_permission(&self, id: &str, directory: &str, reply: &str) -> Result<()> {
        let path = format!("/permission/{id}/reply");
        let body = json!({ "reply": reply });
        call(
            self.request(Method::POST, &path, Some(directory))
                .json(&body),
        )
        .await
    }

    /// Subscribes to everything happening in `directory`.
    pub async fn events(&self, directory: &str) -> Result<Events> {
        let response = send(self.request(Method::GET, "/event", Some(directory))).await?;
        Ok(Events {
            response,
            buffer: Vec::new(),
        })
    }
}

/// Sends a request and reads its JSON answer.
async fn json<T: DeserializeOwned>(request: RequestBuilder) -> Result<T> {
    Ok(send(request).await?.json().await?)
}

/// Sends a request whose answer doesn't matter.
async fn call(request: RequestBuilder) -> Result<()> {
    send(request).await?;
    Ok(())
}

/// Sends a request and turns an error status into an error carrying the body.
async fn send(request: RequestBuilder) -> Result<Response> {
    let response = request
        .send()
        .await
        .context("reaching the opencode server")?;
    check(response).await
}

/// The response, if it succeeded.
async fn check(response: Response) -> Result<Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.text().await.unwrap_or_default();
    bail!("opencode server answered {status}: {body}")
}

/// One server-sent event: a `type` and its `properties`.
#[derive(Debug, Deserialize)]
pub struct Event {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub properties: serde_json::Value,
}

/// The server-sent event stream. OpenCode puts each event's JSON on a single
/// `data:` line.
pub struct Events {
    response: Response,
    buffer: Vec<u8>,
}

impl Events {
    /// The next event, or `None` once the server closes the stream.
    pub async fn next(&mut self) -> Result<Option<Event>> {
        loop {
            while let Some(end) = self.buffer.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.buffer.drain(..=end).collect();
                let Some(data) = line.strip_prefix(b"data:") else {
                    continue;
                };
                match serde_json::from_slice(data) {
                    Ok(event) => return Ok(Some(event)),
                    Err(e) => tracing::debug!("skipping unreadable opencode event: {e}"),
                }
            }
            match self.response.chunk().await? {
                Some(chunk) => self.buffer.extend_from_slice(&chunk),
                None => return Ok(None),
            }
        }
    }
}
