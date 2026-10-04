//! Runs one prompt with `claude -p` and streams the reply back.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use commandant_proto::{AgentPrompt, AgentUsage, OutputStream, TaskFinished, WorkerMsg};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use super::{ClaudeCode, new_session_id, sessions};
use crate::harness::{HarnessKind, Output};

/// Runs `task` until Claude Code answers, or `cancel` fires (or its sender is
/// dropped).
pub async fn converse(
    claude: &ClaudeCode,
    task: AgentPrompt,
    out: &mpsc::Sender<WorkerMsg>,
    mut cancel: oneshot::Receiver<()>,
) -> Result<TaskFinished> {
    // A new session gets its id up front, so it can be claimed at once.
    let (session_id, resume) = match task.session_id.as_str() {
        "" => (new_session_id(), false),
        id => (id.to_string(), true),
    };
    let directory = directory(&task, resume)?;
    let _running = claude.busy.claim(&session_id)?;

    let mut command = claude.command(&directory);
    // Nobody is there to answer a permission request. The admin could run
    // any command on this node anyway, so this grants nothing new.
    command.args(["-p", "--output-format", "stream-json", "--verbose"]);
    command.args([
        "--include-partial-messages",
        "--dangerously-skip-permissions",
    ]);
    command.args([
        if resume { "--resume" } else { "--session-id" },
        &session_id,
    ]);
    for (flag, value) in [
        ("--model", &task.model),
        ("--effort", &task.variant),
        ("--agent", &task.agent),
    ] {
        if !value.is_empty() {
            command.args([flag, value]);
        }
    }
    let mut child = command.spawn().context("starting claude")?;
    let text = match task.command.as_str() {
        "" => task.prompt.clone(),
        name => format!("/{name} {}", task.prompt).trim_end().to_string(),
    };
    let mut stdin = child.stdin.take().expect("piped");
    stdin.write_all(text.as_bytes()).await?;
    drop(stdin);
    let mut stderr = child.stderr.take().expect("piped");
    let errors = tokio::spawn(async move {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text).await;
        text
    });

    let mut lines = BufReader::new(child.stdout.take().expect("piped")).lines();
    let mut turn = Turn::new(&task.task_id, &session_id, out);
    loop {
        let line = tokio::select! {
            line = lines.next_line() => line?,
            _ = &mut cancel => {
                stop(&mut child);
                turn.output.end_line().await;
                return Ok(TaskFinished {
                    task_id: task.task_id,
                    cancelled: true,
                    session_id,
                    ..Default::default()
                });
            }
        };
        let Some(line) = line else { break };
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Err(e) = turn.follow(&event).await {
            stop(&mut child);
            return Err(e);
        }
        if let Some(finished) = turn.finished.take() {
            turn.output.end_line().await;
            let _ = child.wait().await;
            return Ok(finished);
        }
    }
    let status = child.wait().await?;
    let errors = errors.await.unwrap_or_default();
    let last = errors.lines().rev().find(|l| !l.trim().is_empty());
    bail!(
        "claude stopped without answering ({status}){}",
        last.map(|l| format!(": {l}")).unwrap_or_default()
    )
}

/// The task's directory, else the resumed session's (Claude Code only finds a
/// session from where it was started), else the worker's.
fn directory(task: &AgentPrompt, resume: bool) -> Result<PathBuf> {
    Ok(if !task.cwd.is_empty() {
        std::path::absolute(&task.cwd)?
    } else if resume {
        sessions::directory(&sessions::projects()?, &task.session_id)?
    } else {
        std::env::current_dir()?
    })
}

/// Stops `claude` and what it started.
fn stop(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: plain syscall; the group id equals the child's pid.
        unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGTERM) };
    }
    let _ = child.start_kill();
}

/// Follows what `claude` streams: text to stdout, thinking to the reasoning
/// stream, tool calls to stderr. Subagents' own events are left out.
struct Turn<'a> {
    task_id: &'a str,
    session_id: &'a str,
    output: Output<'a>,
    model: String,
    /// Tokens in the context after the latest model call.
    context: u64,
    /// Tool names by call id, to name a call that fails.
    tools: HashMap<String, String>,
    finished: Option<TaskFinished>,
}

impl<'a> Turn<'a> {
    fn new(task_id: &'a str, session_id: &'a str, out: &'a mpsc::Sender<WorkerMsg>) -> Self {
        Self {
            task_id,
            session_id,
            output: Output::new(task_id, HarnessKind::ClaudeCode, out),
            model: String::new(),
            context: 0,
            tools: HashMap::new(),
            finished: None,
        }
    }

    /// Takes in one event; fails if Claude Code isn't on the subscription.
    async fn follow(&mut self, event: &Value) -> Result<()> {
        // A subagent's work shows as the tool call that started it.
        if !event["parent_tool_use_id"].is_null() {
            return Ok(());
        }
        match event["type"].as_str().unwrap_or_default() {
            "system" if event["subtype"] == "init" => {
                let source = event["apiKeySource"].as_str().unwrap_or("none");
                if source != "none" {
                    bail!(
                        "Claude Code would bill an API key (from {source}) instead of the subscription; remove it on the node"
                    );
                }
                self.model = text(&event["model"]);
            }
            "stream_event" => {
                let event = &event["event"];
                match event["type"].as_str().unwrap_or_default() {
                    "content_block_start" => self.output.end_line().await,
                    "content_block_delta" => {
                        let delta = &event["delta"];
                        match delta["type"].as_str().unwrap_or_default() {
                            "text_delta" => self.say(OutputStream::Stdout, &delta["text"]).await,
                            "thinking_delta" => {
                                self.say(OutputStream::Reasoning, &delta["thinking"]).await
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            "assistant" => {
                let message = &event["message"];
                if let Some(model) = message["model"].as_str() {
                    self.model = model.to_string();
                }
                let usage = &message["usage"];
                let context = [
                    "input_tokens",
                    "cache_read_input_tokens",
                    "cache_creation_input_tokens",
                    "output_tokens",
                ]
                .iter()
                .map(|k| usage[k].as_u64().unwrap_or_default())
                .sum::<u64>();
                if context > 0 {
                    self.context = context;
                }
                for block in blocks(message) {
                    if block["type"] == "tool_use" {
                        let name = text(&block["name"]);
                        self.tools.insert(text(&block["id"]), name.clone());
                        self.output.note(&tool_line(&name, &block["input"])).await;
                    }
                }
            }
            "user" => {
                for block in blocks(&event["message"]) {
                    if block["type"] == "tool_result" && block["is_error"] == true {
                        let name = self.tools.get(&text(&block["tool_use_id"])).cloned();
                        let why = first_line(&result_text(&block["content"]));
                        let name = name.unwrap_or_else(|| "a tool".into());
                        self.output.note(&format!("{name} failed: {why}")).await;
                    }
                }
            }
            "result" => self.finished = Some(self.result(event)),
            _ => {}
        }
        Ok(())
    }

    fn result(&self, event: &Value) -> TaskFinished {
        let usage = &event["usage"];
        let count = |key: &str| usage[key].as_u64().unwrap_or_default();
        let error = match event["is_error"] == true {
            true => {
                let said = text(&event["result"]);
                commandant_common::or(&said, &text(&event["subtype"])).to_string()
            }
            false => String::new(),
        };
        TaskFinished {
            task_id: self.task_id.to_string(),
            exit_code: Some(if error.is_empty() { 0 } else { 1 }),
            error,
            cancelled: false,
            session_id: self.session_id.to_string(),
            model: self.model.clone(),
            usage: Some(AgentUsage {
                input: count("input_tokens"),
                output: count("output_tokens"),
                reasoning: 0,
                cache_read: count("cache_read_input_tokens"),
                cache_write: count("cache_creation_input_tokens"),
                // The subscription pays: `total_cost_usd` is only what the
                // API would have charged.
                cost: 0.0,
                context: self.context,
            }),
        }
    }

    async fn say(&mut self, stream: OutputStream, value: &Value) {
        self.output.say(stream, text(value)).await;
    }
}

/// A message's content blocks.
pub fn blocks(message: &Value) -> impl Iterator<Item = &Value> {
    message["content"].as_array().into_iter().flatten()
}

/// `Bash git status`: a tool call, by what best says what it does.
pub fn tool_line(name: &str, input: &Value) -> String {
    let what = [
        "description",
        "command",
        "file_path",
        "pattern",
        "url",
        "query",
        "prompt",
    ]
    .iter()
    .find_map(|k| input[k].as_str())
    .map(first_line)
    .unwrap_or_default();
    format!("{name} {what}").trim().to_string()
}

/// A tool result's text, given as a string or as text blocks.
fn result_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        content => blocks(&serde_json::json!({ "content": content }))
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// The first line, cut to a readable length.
fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default();
    match line.char_indices().nth(100) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None => line.trim().to_string(),
    }
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use commandant_proto::worker_msg::Msg;
    use serde_json::json;

    use super::*;

    fn delta(kind: &str, field: &str, text: &str) -> Value {
        json!({ "type": "stream_event", "parent_tool_use_id": null,
                "event": { "type": "content_block_delta", "delta": { "type": kind, field: text } } })
    }

    fn start() -> Value {
        json!({ "type": "stream_event", "event": { "type": "content_block_start" } })
    }

    #[tokio::test]
    async fn streams_the_reply_and_ends_with_the_result() {
        let (out, mut rx) = mpsc::channel(64);
        let mut turn = Turn::new("t", "s", &out);
        let events = [
            json!({ "type": "system", "subtype": "init", "apiKeySource": "none", "model": "claude-x" }),
            start(),
            delta("thinking_delta", "thinking", "hmm"),
            start(),
            delta("text_delta", "text", "po"),
            delta("text_delta", "text", "ng"),
            json!({ "type": "assistant", "parent_tool_use_id": null, "message": { "model": "claude-y",
                "usage": { "input_tokens": 10, "cache_read_input_tokens": 90, "output_tokens": 5 },
                "content": [{ "type": "tool_use", "id": "tu1", "name": "Bash",
                              "input": { "command": "ls -la\nmore", "description": null } }] } }),
            json!({ "type": "user", "message": { "content": [
                { "type": "tool_result", "tool_use_id": "tu1", "is_error": true,
                  "content": [{ "type": "text", "text": "\nno such dir" }] }] } }),
            // A subagent's: left out.
            json!({ "type": "stream_event", "parent_tool_use_id": "tu9",
                    "event": { "type": "content_block_delta", "delta": { "type": "text_delta", "text": "inner" } } }),
            start(),
            delta("text_delta", "text", "bye"),
            json!({ "type": "result", "is_error": false, "total_cost_usd": 0.5,
                    "usage": { "input_tokens": 10, "output_tokens": 7, "cache_read_input_tokens": 90 } }),
        ];
        for event in &events {
            turn.follow(event).await.unwrap();
        }
        let finished = turn.finished.take().expect("the result ends it");
        turn.output.end_line().await;
        assert_eq!(finished.exit_code, Some(0));
        assert_eq!(finished.model, "claude-y");
        let usage = finished.usage.unwrap();
        assert_eq!((usage.input, usage.output, usage.cache_read), (10, 7, 90));
        assert_eq!(
            (usage.cost, usage.context),
            (0.0, 105),
            "the subscription pays"
        );

        drop(turn);
        drop(out);
        let (mut stdout, mut stderr, mut thinking) = (String::new(), String::new(), String::new());
        while let Some(WorkerMsg {
            msg: Some(Msg::Output(output)),
        }) = rx.recv().await
        {
            let text = String::from_utf8(output.data.clone()).unwrap();
            match output.stream() {
                OutputStream::Stderr => stderr += &text,
                OutputStream::Reasoning => thinking += &text,
                _ => stdout += &text,
            }
        }
        assert_eq!(thinking, "hmm\n");
        assert_eq!(stdout, "pong\nbye\n");
        assert_eq!(
            stderr,
            "[claude-code] Bash ls -la\n[claude-code] Bash failed: no such dir\n"
        );
    }

    #[tokio::test]
    async fn refuses_anything_but_the_subscription_and_reports_errors() {
        let (out, _rx) = mpsc::channel(64);
        let mut turn = Turn::new("t", "s", &out);
        let keyed =
            json!({ "type": "system", "subtype": "init", "apiKeySource": "ANTHROPIC_API_KEY" });
        let refused = turn.follow(&keyed).await.unwrap_err().to_string();
        assert!(refused.contains("instead of the subscription"), "{refused}");

        let failed = json!({ "type": "result", "is_error": true, "subtype": "error_during_execution",
                             "result": "Credit balance is too low" });
        turn.follow(&failed).await.unwrap();
        let finished = turn.finished.unwrap();
        assert_eq!(finished.exit_code, Some(1));
        assert_eq!(finished.error, "Credit balance is too low");
    }
}
