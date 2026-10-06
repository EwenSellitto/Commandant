//! A stand-in for `opencode serve`: just enough of its HTTP API for a
//! worker to host it. Its "binary" is a script that prints the address of a
//! server this process runs, so tests can hold turns open, look at what the
//! worker asked for, and make a turn fail.
//!
//! What a prompt does depends on its text: one with `hold` waits for
//! [`FakeOpencode::release`] (or an abort), `fail` ends in a session error,
//! `refuse` is turned down before it starts, and `permission` asks for a
//! permission first. The reply is `echo: <text>`.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, oneshot};

/// What frees a held turn.
enum Release {
    Go,
    Abort,
}

#[derive(Clone)]
struct Session {
    id: String,
    directory: String,
    title: String,
    updated: u64,
}

#[derive(Default)]
struct State {
    sessions: Mutex<Vec<Session>>,
    /// Providers with credentials.
    connected: Mutex<Vec<String>>,
    /// What each session's turns said, as `GET /session/:id/message` lists it.
    messages: Mutex<HashMap<String, Vec<Value>>>,
    mcp: Mutex<BTreeMap<String, (String, Option<String>)>>,
    /// Held turns, by session.
    held: Mutex<HashMap<String, oneshot::Sender<Release>>>,
    /// Permission requests waiting for an answer, by id.
    asked: Mutex<HashMap<String, oneshot::Sender<String>>>,
    /// `METHOD path` of every request, and the bodies of prompts and commands.
    log: Mutex<Vec<String>>,
    next: AtomicU64,
    events: Option<broadcast::Sender<String>>,
    /// Like OpenCode before `/experimental/session`: `/session` lists them.
    legacy: bool,
    /// How long listing the commands and MCP servers takes, as while they
    /// connect.
    mcp_delay: Mutex<std::time::Duration>,
}

pub struct FakeOpencode {
    state: Arc<State>,
    /// The script a worker runs as `opencode`.
    pub binary: PathBuf,
    _dir: tempfile::TempDir,
}

impl FakeOpencode {
    pub async fn start() -> Self {
        Self::launch(false).await
    }

    /// One without the cross-project session listing.
    pub async fn start_legacy() -> Self {
        Self::launch(true).await
    }

    async fn launch(legacy: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let mcp = BTreeMap::from([
            ("docs".to_string(), ("disabled".to_string(), None)),
            (
                "broken".to_string(),
                ("failed".to_string(), Some("connection refused".to_string())),
            ),
        ]);
        let state = Arc::new(State {
            mcp: Mutex::new(mcp),
            events: Some(broadcast::channel(1024).0),
            legacy,
            ..Default::default()
        });
        tokio::spawn(serve(listener, state.clone()));

        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("opencode");
        // `serve` is all a worker runs; sleeping stands for the server.
        let script =
            format!("#!/bin/sh\necho \"opencode server listening on {url}\"\nexec sleep 300\n");
        write_script(&binary, &script);
        Self {
            state,
            binary,
            _dir: dir,
        }
    }

    /// Makes listing the commands and MCP servers take `delay`, like OpenCode
    /// waiting for its MCP servers to connect.
    pub fn delay_mcp(&self, delay: std::time::Duration) {
        *self.state.mcp_delay.lock().unwrap() = delay;
    }

    /// Every request so far, and the prompts' and commands' bodies.
    pub fn log(&self) -> Vec<String> {
        self.state.log.lock().unwrap().clone()
    }

    /// Whether a turn in `session_id` is being held.
    pub fn holding(&self, session_id: &str) -> bool {
        self.state.held.lock().unwrap().contains_key(session_id)
    }

    /// Lets a held turn finish; false if none was held.
    pub fn release(&self, session_id: &str) -> bool {
        let held = self.state.held.lock().unwrap().remove(session_id);
        held.is_some_and(|h| h.send(Release::Go).is_ok())
    }

    /// Waits until `count` turns are held, and returns their sessions.
    pub async fn wait_held(&self, count: usize) -> Vec<String> {
        crate::eventually(&format!("{count} turns were never held"), || async {
            let held: Vec<String> = self.state.held.lock().unwrap().keys().cloned().collect();
            (held.len() >= count).then_some(held)
        })
        .await
    }
}

/// Writes an executable shell script, a stand-in for an agent's binary.
pub fn write_script(path: &std::path::Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

async fn serve(listener: TcpListener, state: Arc<State>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        tokio::spawn(handle(stream, state.clone()));
    }
}

/// Reads one request and answers it; every connection serves one request.
async fn handle(mut stream: TcpStream, state: Arc<State>) {
    let mut buf = Vec::new();
    let head_end = loop {
        let mut chunk = [0u8; 4096];
        let Ok(n) = stream.read(&mut chunk).await else {
            return;
        };
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let length = head
        .lines()
        .find_map(|l| {
            let (name, value) = l.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    while buf.len() < head_end + length {
        let mut chunk = [0u8; 4096];
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let body: Value =
        serde_json::from_slice(&buf[head_end..head_end + length]).unwrap_or(Value::Null);
    let mut request_line = head.lines().next().unwrap_or_default().split(' ');
    let method = request_line.next().unwrap_or_default().to_string();
    let target = request_line.next().unwrap_or_default().to_string();
    let path = target.split('?').next().unwrap_or_default().to_string();
    let query = target
        .split_once('?')
        .map(|(_, q)| q.to_string())
        .unwrap_or_default();
    state.log.lock().unwrap().push(format!("{method} {path}"));

    if method == "GET" && path == "/event" {
        return stream_events(stream, &state).await;
    }
    let (status, reply) = route(&state, &method, &path, &query, body).await;
    let reply = reply.to_string();
    let response = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
        reply.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

/// Server-sent events, one JSON object per `data:` line, until the worker
/// goes away.
async fn stream_events(mut stream: TcpStream, state: &State) {
    let mut events = state.events.as_ref().unwrap().subscribe();
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
    if stream.write_all(head.as_bytes()).await.is_err() {
        return;
    }
    while let Ok(event) = events.recv().await {
        let line = format!("data: {event}\n\n");
        if stream.write_all(line.as_bytes()).await.is_err() {
            return;
        }
    }
}

async fn route(
    state: &Arc<State>,
    method: &str,
    path: &str,
    query: &str,
    body: Value,
) -> (u16, Value) {
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match (method, parts.as_slice()) {
        ("GET", ["global", "health"]) => (200, json!({ "healthy": true })),
        ("GET", ["provider"]) => {
            let all = json!([{ "id": "fake", "name": "Fake" }, { "id": "other", "name": "Other" }]);
            let connected = state.connected.lock().unwrap().clone();
            (
                200,
                json!({ "all": all, "connected": connected, "default": {} }),
            )
        }
        ("GET", ["provider", "auth"]) => (
            200,
            json!({ "other": [
                { "type": "oauth", "label": "Browser" },
                { "type": "oauth", "label": "Paste a code" },
                { "type": "api", "label": "API key" },
            ]}),
        ),
        ("POST", ["provider", id, "oauth", "authorize"]) => {
            let code = body["method"] == 1;
            let method = if code { "code" } else { "auto" };
            let url = format!("https://example.com/{id}/authorize");
            (
                200,
                json!({ "url": url, "method": method, "instructions": "Sign in there" }),
            )
        }
        ("POST", ["provider", id, "oauth", "callback"]) => {
            state.log.lock().unwrap().push(format!("callback {body}"));
            if body["method"] == 1 && body["code"] != "good" {
                return (400, json!({ "name": "ProviderAuthOauthCallbackFailed" }));
            }
            state.connected.lock().unwrap().push(id.to_string());
            (200, json!(true))
        }
        ("PUT", ["auth", id]) => {
            state.log.lock().unwrap().push(format!("auth {id} {body}"));
            state.connected.lock().unwrap().push(id.to_string());
            (200, json!(true))
        }
        ("DELETE", ["auth", id]) => {
            state.connected.lock().unwrap().retain(|c| c != id);
            (200, json!(true))
        }
        ("POST", ["global", "dispose"]) => (200, json!(true)),
        ("GET", ["agent"]) => (
            200,
            json!([
                { "name": "build", "mode": "primary", "description": "Builds things" },
                { "name": "plan", "mode": "primary" },
                { "name": "explore", "mode": "subagent" },
            ]),
        ),
        ("GET", ["config", "providers"]) => (
            200,
            json!({ "providers": [{ "id": "fake", "name": "Fake", "models": {
                "echo": { "id": "echo", "name": "Echo", "variants": { "high": {}, "low": {} },
                          "limit": { "context": 1000 } },
                "old": { "id": "old", "name": "Old", "status": "deprecated" },
            }}]}),
        ),
        ("GET", ["config"]) => (200, json!({ "model": "fake/echo" })),
        ("GET", ["command"]) => {
            let delay = *state.mcp_delay.lock().unwrap();
            tokio::time::sleep(delay).await;
            (200, commands())
        }
        ("GET", ["mcp"]) => {
            let delay = *state.mcp_delay.lock().unwrap();
            tokio::time::sleep(delay).await;
            let mcp = state.mcp.lock().unwrap();
            let servers: serde_json::Map<String, Value> = mcp
                .iter()
                .map(|(name, (status, error))| {
                    (name.clone(), json!({ "status": status, "error": error }))
                })
                .collect();
            (200, Value::Object(servers))
        }
        ("POST", ["mcp", name, action]) => {
            let mut mcp = state.mcp.lock().unwrap();
            match mcp.get_mut(*name) {
                Some(server) => {
                    *server = match *action {
                        "connect" => ("connected".into(), None),
                        _ => ("disabled".into(), None),
                    };
                    (200, json!(true))
                }
                None => (
                    400,
                    json!({ "error": format!("no MCP server named {name}") }),
                ),
            }
        }
        ("POST", ["session"]) => {
            let n = state.next.fetch_add(1, Ordering::SeqCst);
            let directory = param(query, "directory").unwrap_or_default();
            let session = Session {
                id: format!("ses_{n:04}"),
                directory: directory.clone(),
                title: format!("session {n}"),
                updated: n,
            };
            state.sessions.lock().unwrap().push(session.clone());
            (200, json!({ "id": session.id, "directory": directory }))
        }
        ("GET", ["experimental", "session"]) if state.legacy => {
            (404, json!({ "error": "not found" }))
        }
        ("GET", ["experimental", "session"]) | ("GET", ["session"]) => {
            let mut sessions = state.sessions.lock().unwrap().clone();
            sessions.sort_by_key(|s| std::cmp::Reverse(s.updated));
            let listed: Vec<Value> = sessions
                .iter()
                .map(|s| json!({ "id": s.id, "title": s.title, "directory": s.directory,
                                 "agent": "build", "model": { "id": "echo", "providerID": "fake" },
                                 "time": { "created": 1, "updated": 1_000_000 + s.updated * 1000 } }))
                .collect();
            (200, Value::Array(listed))
        }
        ("GET", ["session", id, "message"]) => {
            let messages = state.messages.lock().unwrap();
            (200, json!(messages.get(*id).cloned().unwrap_or_default()))
        }
        ("GET", ["session", id]) => match find_session(state, id) {
            Some(s) => (200, json!({ "id": s.id, "directory": s.directory })),
            None => (404, json!({ "error": "no such session" })),
        },
        ("POST", ["session", id, "prompt_async"]) => {
            state.log.lock().unwrap().push(format!("prompt {body}"));
            let text = body["parts"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if text.contains("refuse") {
                return (500, json!({ "name": "UnknownError" }));
            }
            tokio::spawn(turn(state.clone(), id.to_string(), text));
            (204, json!(null))
        }
        ("POST", ["session", id, "command"]) => {
            state.log.lock().unwrap().push(format!("command {body}"));
            let name = body["command"].as_str().unwrap_or_default();
            if !commands()
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["name"] == name)
            {
                return (500, json!({ "name": "UnknownError" }));
            }
            let arguments = body["arguments"].as_str().unwrap_or_default();
            // Like OpenCode, it answers once the turn is over.
            turn(
                state.clone(),
                id.to_string(),
                format!("/{name} {arguments}"),
            )
            .await;
            (200, json!({}))
        }
        ("POST", ["session", id, "abort"]) => {
            let held = state.held.lock().unwrap().remove(*id);
            if let Some(held) = held {
                let _ = held.send(Release::Abort);
            }
            (200, json!(true))
        }
        ("POST", ["permission", id, "reply"]) => {
            let asked = state.asked.lock().unwrap().remove(*id);
            let reply = body["reply"].as_str().unwrap_or_default().to_string();
            match asked {
                Some(asked) => {
                    let _ = asked.send(reply);
                    (200, json!(true))
                }
                None => (404, json!({ "error": "no such permission" })),
            }
        }
        _ => (
            404,
            json!({ "error": format!("{method} {path} isn't faked") }),
        ),
    }
}

fn commands() -> Value {
    json!([
        { "name": "review", "description": "Review the changes", "source": "command" },
        { "name": "pdf", "description": "Work with PDFs", "source": "skill" },
    ])
}

fn find_session(state: &State, id: &str) -> Option<Session> {
    state
        .sessions
        .lock()
        .unwrap()
        .iter()
        .find(|s| s.id == id)
        .cloned()
}

fn param(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| percent_decode(value))
    })
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap();
                out.push(u8::from_str_radix(hex, 16).unwrap());
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap()
}

/// Plays one turn of the agent in `session_id`, as events.
async fn turn(state: Arc<State>, session_id: String, text: String) {
    let emit = |kind: &str, properties: Value| {
        let event = json!({ "type": kind, "properties": properties });
        let _ = state.events.as_ref().unwrap().send(event.to_string());
    };
    let n = state.next.fetch_add(1, Ordering::SeqCst);
    if let Some(session) = state
        .sessions
        .lock()
        .unwrap()
        .iter_mut()
        .find(|s| s.id == session_id)
    {
        session.updated = n;
    }
    let (message, part) = (format!("msg_{n}"), format!("prt_{n}"));
    state
        .messages
        .lock()
        .unwrap()
        .entry(session_id.clone())
        .or_default()
        .extend([
            json!({ "info": { "role": "user" }, "parts": [{ "type": "text", "text": text }] }),
            json!({ "info": { "role": "assistant" }, "parts": [
                { "type": "text", "text": format!("echo: {text}") }] }),
        ]);
    let status = |kind: &str| json!({ "sessionID": session_id, "status": { "type": kind } });
    emit("session.status", status("busy"));
    emit(
        "message.updated",
        json!({ "info": { "id": message, "sessionID": session_id, "role": "assistant",
            "providerID": "fake", "modelID": "echo", "cost": 0.01,
            "tokens": { "input": 10, "output": 3, "reasoning": 0, "cache": { "read": 0, "write": 0 } } } }),
    );
    if text.contains("permission") {
        let id = format!("per_{n}");
        let (tx, rx) = oneshot::channel();
        state.asked.lock().unwrap().insert(id.clone(), tx);
        emit(
            "permission.asked",
            json!({ "id": id, "sessionID": session_id, "permission": "bash", "patterns": ["make"] }),
        );
        let reply = rx.await.unwrap_or_default();
        state
            .log
            .lock()
            .unwrap()
            .push(format!("permission {reply}"));
    }
    emit(
        "message.part.updated",
        json!({ "part": { "id": part, "sessionID": session_id, "messageID": message,
            "type": "text", "text": format!("echo: {text}") } }),
    );
    if text.contains("hold") {
        let (tx, rx) = oneshot::channel();
        state.held.lock().unwrap().insert(session_id.clone(), tx);
        if let Ok(Release::Abort) = rx.await {
            emit(
                "session.error",
                json!({ "sessionID": session_id, "error": { "name": "MessageAbortedError" } }),
            );
        }
    }
    if text.contains("fail") {
        emit(
            "session.error",
            json!({ "sessionID": session_id, "error": { "name": "APIError", "data": { "message": "boom" } } }),
        );
    }
    emit("session.status", status("idle"));
}
