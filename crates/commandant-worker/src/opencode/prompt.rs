//! Runs one prompt in an OpenCode session and streams the reply back.

use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use commandant_proto::{AgentPrompt, AgentUsage, OutputStream, TaskFinished, WorkerMsg};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};

use super::Opencode;
use super::api::{Api, CommandRun, Event, Prompt};
use crate::harness::{HarnessKind, Output};

/// Runs `task` until the agent goes idle or `cancel` fires (or its sender is
/// dropped).
pub async fn converse(
    opencode: &Opencode,
    task: AgentPrompt,
    out: &mpsc::Sender<WorkerMsg>,
    mut cancel: oneshot::Receiver<()>,
) -> Result<TaskFinished> {
    opencode.reload_if_stale().await?;
    // A reload would abort this prompt, so none runs until it is done.
    let _quiet = opencode.quiet.read().await;
    let api = opencode.api().await?;
    let prompt = Prompt::new(&task.prompt, &task.model, &task.agent, &task.variant)?;
    let directory = directory(&api, &task).await?;
    if !task.command.is_empty() {
        let commands = api.commands(Some(&directory)).await?;
        if !commands.iter().any(|c| c.name == task.command) {
            bail!("the agent has no /{} command or skill", task.command);
        }
    }
    let session_id = match task.session_id.as_str() {
        "" => api.create_session(&directory).await?,
        id => id.to_string(),
    };
    let _running = opencode.busy.claim(&session_id)?;
    // Subscribed before prompting, so no event is missed.
    let mut events = api.events(&directory).await?;
    // A command only answers once the agent is done, so it runs aside while
    // its events come in; it only matters if it fails.
    let mut command = None;
    if task.command.is_empty() {
        api.prompt(&session_id, &directory, &prompt).await?;
    } else {
        let (api, session_id, directory) = (api.clone(), session_id.clone(), directory.clone());
        let task = task.clone();
        command = Some(tokio::spawn(async move {
            let run = CommandRun::new(
                &task.command,
                &task.prompt,
                &task.model,
                &task.agent,
                &task.variant,
            );
            api.command(&session_id, &directory, &run).await
        }));
    }

    let mut transcript = Transcript::new(&task.task_id, &session_id, out);
    loop {
        let event = tokio::select! {
            event = events.next() => event?,
            ran = async { command.as_mut().expect("guarded").await }, if command.is_some() => {
                command = None;
                ran??;
                continue;
            }
            _ = &mut cancel => {
                transcript.output.end_line().await;
                api.abort(&session_id, &directory).await?;
                return Ok(TaskFinished {
                    task_id: task.task_id,
                    cancelled: true,
                    session_id,
                    ..Default::default()
                });
            }
        };
        let Some(event) = event else {
            bail!("the opencode server closed its event stream");
        };
        if let Some(permission) = transcript.follow(event).await {
            // Nobody is there to answer. The admin could run any command on
            // this node anyway, so this grants nothing new.
            api.reply_permission(&permission, &directory, "once")
                .await?;
        }
        if let Some(finished) = transcript.finished() {
            transcript.output.end_line().await;
            return Ok(finished);
        }
    }
}

/// The task's working directory, else the session's, else the worker's.
async fn directory(api: &Api, task: &AgentPrompt) -> Result<String> {
    let directory = if !task.cwd.is_empty() {
        std::path::absolute(&task.cwd)?
    } else if !task.session_id.is_empty() {
        return api.session_directory(&task.session_id).await;
    } else {
        std::env::current_dir()?
    };
    Ok(directory.to_string_lossy().into_owned())
}

#[derive(Deserialize)]
struct MessageInfo {
    id: String,
    #[serde(rename = "sessionID")]
    session_id: String,
    role: String,
    /// Set on the agent's messages.
    #[serde(rename = "providerID", default)]
    provider_id: String,
    #[serde(rename = "modelID", default)]
    model_id: String,
    tokens: Option<Tokens>,
    cost: Option<f64>,
}

#[derive(Clone, Default, Deserialize)]
struct Tokens {
    #[serde(default)]
    input: u64,
    #[serde(default)]
    output: u64,
    #[serde(default)]
    reasoning: u64,
    #[serde(default)]
    cache: CacheTokens,
}

#[derive(Clone, Default, Deserialize)]
struct CacheTokens {
    #[serde(default)]
    read: u64,
    #[serde(default)]
    write: u64,
}

impl Tokens {
    fn total(&self) -> u64 {
        self.input + self.output + self.reasoning + self.cache.read + self.cache.write
    }
}

#[derive(Deserialize)]
struct PartDelta {
    #[serde(rename = "sessionID")]
    session_id: String,
    #[serde(rename = "messageID")]
    message_id: String,
    #[serde(rename = "partID")]
    part_id: String,
    field: String,
    delta: String,
}

#[derive(Deserialize)]
struct Part {
    id: String,
    #[serde(rename = "sessionID")]
    session_id: String,
    #[serde(rename = "messageID")]
    message_id: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    tool: String,
    #[serde(default)]
    state: Option<ToolState>,
}

#[derive(Deserialize)]
struct ToolState {
    status: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    error: String,
}

#[derive(Deserialize)]
struct SessionStatus {
    #[serde(rename = "sessionID")]
    session_id: String,
    status: StatusKind,
}

#[derive(Deserialize)]
struct StatusKind {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct SessionError {
    #[serde(rename = "sessionID")]
    session_id: Option<String>,
    error: Option<ErrorInfo>,
}

#[derive(Deserialize)]
struct ErrorInfo {
    name: String,
    #[serde(default)]
    data: ErrorData,
}

#[derive(Default, Deserialize)]
struct ErrorData {
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct PermissionAsked {
    id: String,
    #[serde(rename = "sessionID")]
    session_id: String,
    permission: String,
    #[serde(default)]
    patterns: Vec<String>,
}

/// Follows one session's events: the agent's text goes to stdout, its
/// thinking to the reasoning stream, its tool activity to stderr.
struct Transcript<'a> {
    task_id: &'a str,
    session_id: &'a str,
    output: Output<'a>,
    /// The agent's messages. The prompt itself comes back as a user message.
    replies: HashSet<String>,
    /// Tokens and cost of each of the agent's messages, in order.
    usage: Vec<(String, Tokens, f64)>,
    /// Parts holding the model's thinking rather than its reply.
    reasoning: HashSet<String>,
    /// How many bytes of each text part were sent.
    sent: HashMap<String, usize>,
    /// The part being written.
    current_part: Option<String>,
    /// Tool calls already reported.
    reported_tools: HashSet<String>,
    /// Set once the agent starts on the prompt, so going idle means it's done.
    busy: bool,
    idle: bool,
    error: Option<String>,
    /// The `provider/model` that replied.
    model: String,
}

impl<'a> Transcript<'a> {
    fn new(task_id: &'a str, session_id: &'a str, out: &'a mpsc::Sender<WorkerMsg>) -> Self {
        Self {
            task_id,
            session_id,
            output: Output::new(task_id, HarnessKind::Opencode, out),
            replies: HashSet::new(),
            usage: Vec::new(),
            reasoning: HashSet::new(),
            sent: HashMap::new(),
            current_part: None,
            reported_tools: HashSet::new(),
            busy: false,
            idle: false,
            error: None,
            model: String::new(),
        }
    }

    /// Handles an event. Returns the id of a permission request to grant.
    async fn follow(&mut self, event: Event) -> Option<String> {
        let properties = event.properties;
        match event.kind.as_str() {
            "message.updated" => {
                let info = parse::<MessageInfo>(&properties["info"])?;
                if info.session_id == self.session_id && info.role == "assistant" {
                    if !info.provider_id.is_empty() {
                        self.model = format!("{}/{}", info.provider_id, info.model_id);
                    }
                    if let Some(tokens) = info.tokens {
                        self.count(&info.id, tokens, info.cost.unwrap_or_default());
                    }
                    self.replies.insert(info.id);
                }
            }
            "message.part.delta" => {
                let delta = parse::<PartDelta>(&properties)?;
                if delta.session_id == self.session_id
                    && delta.field == "text"
                    && self.replies.contains(&delta.message_id)
                {
                    self.say(delta.part_id, delta.delta).await;
                }
            }
            "message.part.updated" => {
                let part = parse::<Part>(&properties["part"])?;
                if part.session_id == self.session_id && self.replies.contains(&part.message_id) {
                    self.follow_part(part).await;
                }
            }
            "session.status" => {
                let status = parse::<SessionStatus>(&properties)?;
                if status.session_id == self.session_id {
                    match status.status.kind.as_str() {
                        "idle" => self.idle = self.busy,
                        _ => self.busy = true,
                    }
                }
            }
            "session.error" => {
                let SessionError {
                    session_id: Some(session_id),
                    error: Some(error),
                } = parse(&properties)?
                else {
                    return None;
                };
                if session_id == self.session_id && error.name != "MessageAbortedError" {
                    self.error = Some(match error.data.message.as_str() {
                        "" => error.name,
                        message => format!("{}: {message}", error.name),
                    });
                }
            }
            "permission.asked" => {
                let asked = parse::<PermissionAsked>(&properties)?;
                if asked.session_id == self.session_id {
                    let what = format!("{} {}", asked.permission, asked.patterns.join(" "));
                    self.output.note(&format!("allowed {}", what.trim())).await;
                    return Some(asked.id);
                }
            }
            _ => {}
        }
        None
    }

    async fn follow_part(&mut self, part: Part) {
        match part.kind.as_str() {
            "text" | "reasoning" => {
                if part.kind == "reasoning" {
                    self.reasoning.insert(part.id.clone());
                }
                // Catches up on text that came without deltas.
                let sent = self.sent.get(&part.id).copied().unwrap_or_default();
                if let Some(unsent) = part.text.get(sent..).filter(|t| !t.is_empty()) {
                    self.say(part.id, unsent.to_string()).await;
                }
            }
            "tool" => {
                let Some(state) = part.state else { return };
                let line = match state.status.as_str() {
                    "completed" => format!("{} {}", part.tool, state.title),
                    "error" => format!("{} failed: {}", part.tool, state.error),
                    _ => return,
                };
                if self.reported_tools.insert(part.id) {
                    self.output.note(line.trim()).await;
                }
            }
            _ => {}
        }
    }

    fn finished(&self) -> Option<TaskFinished> {
        self.idle.then(|| TaskFinished {
            task_id: self.task_id.to_string(),
            exit_code: Some(if self.error.is_some() { 1 } else { 0 }),
            error: self.error.clone().unwrap_or_default(),
            cancelled: false,
            session_id: self.session_id.to_string(),
            model: self.model.clone(),
            usage: Some(self.usage()),
        })
    }

    /// Keeps a message's latest counts; OpenCode resends them as they grow.
    fn count(&mut self, message_id: &str, tokens: Tokens, cost: f64) {
        match self.usage.iter_mut().find(|(id, ..)| id == message_id) {
            Some(usage) => *usage = (message_id.to_string(), tokens, cost),
            None => self.usage.push((message_id.to_string(), tokens, cost)),
        }
    }

    fn usage(&self) -> AgentUsage {
        let mut usage = AgentUsage::default();
        for (_, tokens, cost) in &self.usage {
            usage.input += tokens.input;
            usage.output += tokens.output;
            usage.reasoning += tokens.reasoning;
            usage.cache_read += tokens.cache.read;
            usage.cache_write += tokens.cache.write;
            usage.cost += cost;
        }
        // The last call saw the whole conversation so far.
        usage.context = self
            .usage
            .iter()
            .rev()
            .map(|(_, tokens, _)| tokens.total())
            .find(|&total| total > 0)
            .unwrap_or_default();
        usage
    }

    /// Writes the agent's text or thinking, starting each new part on a new
    /// line.
    async fn say(&mut self, part_id: String, text: String) {
        *self.sent.entry(part_id.clone()).or_default() += text.len();
        let stream = match self.reasoning.contains(&part_id) {
            true => OutputStream::Reasoning,
            false => OutputStream::Stdout,
        };
        if self.current_part.as_ref() != Some(&part_id) {
            self.output.end_line().await;
            self.current_part = Some(part_id);
        }
        self.output.say(stream, text).await;
    }
}

/// Reads an event's properties, skipping the event if they don't fit.
fn parse<T: for<'de> Deserialize<'de>>(value: &serde_json::Value) -> Option<T> {
    T::deserialize(value).ok()
}

#[cfg(test)]
mod tests {
    use commandant_proto::worker_msg::Msg;
    use serde_json::json;

    use super::*;

    fn event(kind: &str, properties: serde_json::Value) -> Event {
        Event {
            kind: kind.into(),
            properties,
        }
    }

    fn part(message: &str, id: &str, extra: serde_json::Value) -> Event {
        let mut part = json!({ "id": id, "sessionID": "s", "messageID": message });
        part.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        event("message.part.updated", json!({ "part": part }))
    }

    fn delta(part: &str, text: &str) -> Event {
        event(
            "message.part.delta",
            json!({ "sessionID": "s", "messageID": "reply", "partID": part, "field": "text", "delta": text }),
        )
    }

    fn status(session: &str, kind: &str) -> Event {
        event(
            "session.status",
            json!({ "sessionID": session, "status": { "type": kind } }),
        )
    }

    fn message(id: &str, role: &str) -> Event {
        event(
            "message.updated",
            json!({ "info": { "id": id, "sessionID": "s", "role": role, "providerID": "p", "modelID": "m" } }),
        )
    }

    fn usage(id: &str, input: u64, output: u64, cost: f64) -> Event {
        event(
            "message.updated",
            json!({ "info": { "id": id, "sessionID": "s", "role": "assistant", "cost": cost,
                "tokens": { "input": input, "output": output, "reasoning": 0, "cache": { "read": 0, "write": 0 } } } }),
        )
    }

    #[tokio::test]
    async fn streams_the_reply_and_ends_when_idle() {
        let (out, mut rx) = mpsc::channel(64);
        let mut transcript = Transcript::new("t", "s", &out);
        let events = [
            status("s", "idle"),
            message("prompt", "user"),
            part(
                "prompt",
                "p0",
                json!({ "type": "text", "text": "the prompt" }),
            ),
            message("reply", "assistant"),
            status("s", "busy"),
            status("other", "idle"),
            part("reply", "r1", json!({ "type": "reasoning", "text": "" })),
            delta("r1", "hmm"),
            delta("p1", "po"),
            delta("p1", "ng"),
            part("reply", "p1", json!({ "type": "text", "text": "pong" })),
            part(
                "reply",
                "p2",
                json!({ "type": "tool", "tool": "bash", "state": { "status": "running" } }),
            ),
            part(
                "reply",
                "p2",
                json!({ "type": "tool", "tool": "bash", "state": { "status": "completed", "title": "ls" } }),
            ),
            part(
                "reply",
                "p2",
                json!({ "type": "tool", "tool": "bash", "state": { "status": "completed", "title": "ls" } }),
            ),
            part("reply", "p3", json!({ "type": "text", "text": "bye" })),
            usage("reply", 10, 3, 0.5),
            usage("reply2", 0, 0, 0.0),
            usage("reply2", 20, 4, 0.25),
        ];
        for event in events {
            assert_eq!(transcript.follow(event).await, None);
            assert!(transcript.finished().is_none());
        }
        transcript.follow(status("s", "idle")).await;
        let finished = transcript.finished().expect("idle after busy");
        transcript.output.end_line().await;
        assert_eq!(finished.exit_code, Some(0));
        assert_eq!(finished.session_id, "s");
        assert_eq!(finished.model, "p/m");
        let usage = finished.usage.unwrap();
        assert_eq!((usage.input, usage.output, usage.cost), (30, 7, 0.75));
        assert_eq!(usage.context, 24);

        drop(transcript);
        drop(out);
        let (mut stdout, mut stderr, mut thinking) = (String::new(), String::new(), String::new());
        while let Some(WorkerMsg {
            msg: Some(Msg::Output(output)),
        }) = rx.recv().await
        {
            let stream = output.stream();
            let text = String::from_utf8(output.data).unwrap();
            match stream {
                OutputStream::Stderr => stderr += &text,
                OutputStream::Reasoning => thinking += &text,
                _ => stdout += &text,
            }
        }
        assert_eq!(thinking, "hmm\n");
        assert_eq!(stdout, "pong\nbye\n");
        assert_eq!(stderr, "[opencode] bash ls\n");
    }

    #[tokio::test]
    async fn reports_errors_and_grants_permissions() {
        let (out, _rx) = mpsc::channel(64);
        let mut transcript = Transcript::new("t", "s", &out);
        let asked = event(
            "permission.asked",
            json!({ "id": "per1", "sessionID": "s", "permission": "edit", "patterns": ["/etc/x"] }),
        );
        assert_eq!(transcript.follow(asked).await, Some("per1".into()));
        transcript.follow(status("s", "busy")).await;
        let error = json!({ "sessionID": "s", "error": { "name": "UnknownError", "data": { "message": "boom" } } });
        transcript.follow(event("session.error", error)).await;
        transcript.follow(status("s", "idle")).await;
        let finished = transcript.finished().unwrap();
        assert_eq!(finished.exit_code, Some(1));
        assert_eq!(finished.error, "UnknownError: boom");
    }
}
