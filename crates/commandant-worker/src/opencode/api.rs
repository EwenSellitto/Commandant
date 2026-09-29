//! A client for the OpenCode server's HTTP API.
//!
//! Every call names the directory it works in: one server serves any number
//! of projects.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::{Method, RequestBuilder, Response};
use serde::{Deserialize, Serialize};

/// The HTTP basic auth user OpenCode expects.
const USER: &str = "opencode";
const HEALTH_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct Api {
    http: reqwest::Client,
    base: String,
    password: String,
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

// OpenCode sends `null` for unset fields, hence the options.

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

impl Api {
    pub fn new(base: String, password: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            base,
            password,
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

    pub async fn health(&self) -> Result<()> {
        let request = self.request(Method::GET, "/global/health", None);
        send(request.timeout(HEALTH_TIMEOUT)).await?;
        Ok(())
    }

    /// Starts a session in `directory` and returns its id.
    pub async fn create_session(&self, directory: &str) -> Result<String> {
        let request = self.request(Method::POST, "/session", Some(directory));
        let session: Session = send(request.json(&serde_json::json!({})))
            .await?
            .json()
            .await?;
        Ok(session.id)
    }

    /// The directory an existing session works in.
    pub async fn session_directory(&self, session_id: &str) -> Result<String> {
        let request = self.request(Method::GET, &format!("/session/{session_id}"), None);
        let session: Session = send(request).await?.json().await?;
        Ok(session.directory)
    }

    pub async fn agents(&self) -> Result<Vec<Agent>> {
        Ok(send(self.request(Method::GET, "/agent", None))
            .await?
            .json()
            .await?)
    }

    /// The providers that are set up, with their models.
    pub async fn providers(&self) -> Result<Providers> {
        let request = self.request(Method::GET, "/config/providers", None);
        Ok(send(request).await?.json().await?)
    }

    pub async fn config(&self) -> Result<Config> {
        Ok(send(self.request(Method::GET, "/config", None))
            .await?
            .json()
            .await?)
    }

    /// Queues a prompt; the reply arrives as events.
    pub async fn prompt(
        &self,
        session_id: &str,
        directory: &str,
        prompt: &Prompt<'_>,
    ) -> Result<()> {
        let path = format!("/session/{session_id}/prompt_async");
        send(
            self.request(Method::POST, &path, Some(directory))
                .json(prompt),
        )
        .await?;
        Ok(())
    }

    pub async fn abort(&self, session_id: &str, directory: &str) -> Result<()> {
        let path = format!("/session/{session_id}/abort");
        send(self.request(Method::POST, &path, Some(directory))).await?;
        Ok(())
    }

    /// Answers a permission request: `once`, `always` or `reject`.
    pub async fn reply_permission(&self, id: &str, directory: &str, reply: &str) -> Result<()> {
        let path = format!("/permission/{id}/reply");
        let body = serde_json::json!({ "reply": reply });
        send(
            self.request(Method::POST, &path, Some(directory))
                .json(&body),
        )
        .await?;
        Ok(())
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

/// Sends a request and turns an error status into an error carrying the body.
async fn send(request: RequestBuilder) -> Result<Response> {
    let response = request
        .send()
        .await
        .context("reaching the opencode server")?;
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
