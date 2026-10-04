//! One chat: a session with a node's agent, and how keys and task events
//! change it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use commandant_proto::task_event::Event as TaskEvent;
use commandant_proto::*;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::app::{Action, ChatId};
use super::picker::{Choice, Outcome, Pick, Picker};

/// What the worker prefixes its notes with.
const NOTE_PREFIX: &str = "[opencode] ";
/// Lines moved by PageUp / PageDown.
const PAGE: u16 = 10;

/// Sent with every prompt; `session_id` fills in after the first reply.
#[derive(Clone, Default)]
pub struct Settings {
    pub session_id: String,
    pub cwd: String,
    pub model: String,
    pub agent: String,
    pub effort: String,
}

/// What background tasks report to a chat.
pub enum Message {
    Task(TaskEvent),
    /// The prompt couldn't be sent, or its stream broke.
    Failed(String),
    Node(NodeInfo),
    Options(Result<AgentOptions, String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    /// The model's thinking, before or between its replies.
    Thinking,
    Agent,
    /// A tool the agent used.
    Tool,
    Error,
    /// A remark from commandant itself.
    Info,
    /// What a finished turn used: agent, model, time, tokens, cost.
    Summary,
}

pub struct Entry {
    pub role: Role,
    pub text: String,
    /// The agent that was chosen when it was written.
    pub agent: String,
}

pub enum Activity {
    Idle,
    Working {
        /// Known once the orchestrator has started the task.
        task_id: Option<String>,
        since: Instant,
        cancelling: bool,
        /// What the prompt was sent with, for the turn's summary.
        agent: String,
        effort: String,
    },
}

/// How a turn ended that nobody has looked at yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unseen {
    Done,
    Failed,
}

pub struct Chat {
    pub id: ChatId,
    /// The session's title: its first prompt, or what the node saved.
    pub title: String,
    /// Set when a turn ends; the app clears it once the chat is shown.
    pub unseen: Option<Unseen>,
    pub node: NodeInfo,
    pub settings: Settings,
    pub thread: Vec<Entry>,
    pub activity: Activity,
    pub input: Input,
    /// How many lines the thread is scrolled up from the bottom.
    pub scroll: u16,
    /// Bytes of a UTF-8 character split across output chunks, per stream.
    partial_stdout: Vec<u8>,
    partial_stderr: Vec<u8>,
    partial_reasoning: Vec<u8>,
    /// A note line still waiting for its newline.
    partial_note: String,
    /// The agents, models and efforts the node offers, once it has said.
    pub options: Option<Arc<AgentOptions>>,
    fetching_options: bool,
    /// The MCP server being switched, to report on once the node answers.
    switching_mcp: Option<String>,
    /// The model that last replied, which tells what "default" means.
    pub used_model: String,
    /// The floating window, when open.
    pub picker: Option<Picker>,
    /// What the session has cost so far, in US dollars.
    pub spent: f64,
    /// Tokens in the session's context after the last turn.
    pub context: u64,
}

impl Chat {
    /// A chat on `node`; without `options` it asks the node for them.
    pub fn new(
        id: ChatId,
        node: NodeInfo,
        settings: Settings,
        options: Option<Arc<AgentOptions>>,
    ) -> Self {
        Self {
            id,
            title: String::new(),
            unseen: None,
            node,
            settings,
            thread: Vec::new(),
            activity: Activity::Idle,
            input: Input::default(),
            scroll: 0,
            partial_stdout: Vec::new(),
            partial_stderr: Vec::new(),
            partial_reasoning: Vec::new(),
            partial_note: String::new(),
            fetching_options: options.is_none(),
            options,
            switching_mcp: None,
            used_model: String::new(),
            picker: None,
            spent: 0.0,
            context: 0,
        }
    }

    /// The agent prompts go to: the chosen one, else the harness's default.
    pub fn agent(&self) -> &str {
        match (&self.settings.agent, &self.options) {
            (agent, _) if !agent.is_empty() => agent,
            (_, Some(options)) => &options.default_agent,
            _ => "",
        }
    }

    /// The model prompts go to, if known: the chosen one, else the one that
    /// last replied, else the configured default.
    pub fn model(&self) -> &str {
        let default = self.options.as_ref().map_or("", |o| &o.default_model);
        [&self.settings.model, &self.used_model]
            .into_iter()
            .find(|m| !m.is_empty())
            .map_or(default, String::as_str)
    }

    /// What the node said about a model, if anything.
    pub fn model_choice(&self, id: &str) -> Option<&ModelChoice> {
        self.options.as_ref()?.models.iter().find(|m| m.id == id)
    }

    /// A model's name, else its id.
    pub fn model_name<'a>(&'a self, id: &'a str) -> &'a str {
        self.model_choice(id).map_or(id, |m| &m.name)
    }

    /// The thinking efforts of the current model; `None` if it's unknown.
    fn efforts(&self) -> Option<&[String]> {
        self.model_choice(self.model())
            .map(|m| m.variants.as_slice())
    }

    /// What the agent is up to, judging by the latest output.
    pub fn doing(&self) -> &'static str {
        match self.thread.last().map(|e| e.role) {
            Some(Role::Thinking) => "thinking",
            Some(Role::Agent) => "writing",
            Some(Role::Tool) => "using tools",
            _ => "waiting",
        }
    }

    pub fn harness(&self) -> Option<&str> {
        self.node.harnesses.first().map(String::as_str)
    }

    pub fn running_task(&self) -> Option<String> {
        match &self.activity {
            Activity::Working { task_id, .. } => task_id.clone(),
            Activity::Idle => None,
        }
    }

    pub fn on_input(&mut self, event: Event) -> Option<Action> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.on_key(key),
            Event::Paste(text) => {
                // The prompt is a single line.
                self.input.insert_str(&text.replace(['\r', '\n'], " "));
                None
            }
            _ => None,
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && matches!(key.code, KeyCode::Char('c' | 'd')) {
            return Some(Action::Quit);
        }
        if let Some(picker) = &mut self.picker {
            match picker.on_key(key) {
                Outcome::Open => {}
                Outcome::Closed => self.picker = None,
                Outcome::Chosen(value) => {
                    let pick = picker.pick;
                    self.picker = None;
                    return self.choose(pick, value);
                }
            }
            return None;
        }
        match key.code {
            KeyCode::Char('u') if ctrl => self.input.clear(),
            KeyCode::Char('t') if ctrl => return self.cycle_effort(),
            KeyCode::Tab => return self.cycle_agent(1),
            KeyCode::BackTab => return self.cycle_agent(-1),
            KeyCode::Char(c) => self.input.insert(c),
            KeyCode::Enter => return self.submit(),
            KeyCode::Esc => return self.cancel(),
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete(),
            KeyCode::Left => self.input.left(),
            KeyCode::Right => self.input.right(),
            KeyCode::Home => self.input.home(),
            KeyCode::End => self.input.end(),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_add(PAGE),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(PAGE),
            _ => {}
        }
        None
    }

    /// Sends the prompt, or runs a `/command`.
    fn submit(&mut self) -> Option<Action> {
        let text = self.input.text.trim().to_string();
        if text.is_empty() {
            return None;
        }
        if let Some(command) = text.strip_prefix('/') {
            let (command, filter) = command.split_once(' ').unwrap_or((command, ""));
            let pick = match command {
                "quit" | "exit" => return Some(Action::Quit),
                "new" | "sessions" | "nodes" | "close" => {
                    let action = match command {
                        "new" => Action::NewChat,
                        "sessions" => Action::ShowSessions,
                        "nodes" => Action::ShowNodes,
                        _ => Action::CloseChat,
                    };
                    self.input.clear();
                    return Some(action);
                }
                "agent" => Pick::Agent,
                "model" => Pick::Model,
                "effort" => Pick::Effort,
                "commands" | "skills" => Pick::Command,
                "mcp" => Pick::Mcp,
                // One of the agent's own commands or skills, its arguments after it.
                name if self.is_agent_command(name) => {
                    let (name, arguments) = (name.to_string(), filter.trim().to_string());
                    return self.send(text, name, arguments);
                }
                // Not a command: a prompt that starts with a slash.
                _ => return self.send(text.clone(), String::new(), text),
            };
            self.input.clear();
            return self.open_picker(pick, filter.trim());
        }
        self.send(text.clone(), String::new(), text)
    }

    fn is_agent_command(&self, name: &str) -> bool {
        self.options
            .as_ref()
            .is_some_and(|o| o.commands.iter().any(|c| c.name == name))
    }

    /// Sends `prompt` (or runs `command` with it), showing `text` in the thread.
    fn send(&mut self, text: String, command: String, prompt: String) -> Option<Action> {
        if matches!(self.activity, Activity::Working { .. }) {
            // Keep the text; it can be sent once the agent is done.
            return None;
        }
        self.input.clear();
        self.scroll = 0;
        if self.title.is_empty() {
            self.title = text.clone();
        }
        self.push(Role::User, &text);
        self.activity = Activity::Working {
            task_id: None,
            since: Instant::now(),
            cancelling: false,
            agent: self.agent().to_string(),
            effort: self.settings.effort.clone(),
        };
        Some(Action::Send(
            self.id,
            PromptRequest {
                node: self.node.id.clone(),
                prompt,
                command,
                session_id: self.settings.session_id.clone(),
                cwd: self.settings.cwd.clone(),
                model: self.settings.model.clone(),
                agent: self.settings.agent.clone(),
                variant: self.settings.effort.clone(),
            },
        ))
    }

    /// Asks the node what its agent offers, unless that's under way.
    fn ask_for_options(&mut self) -> Option<Action> {
        if self.fetching_options {
            self.info("still asking the node what its agent offers…");
            return None;
        }
        self.fetching_options = true;
        self.info("asking the node what its agent offers…");
        Some(Action::FetchOptions(self.node.id.clone()))
    }

    fn open_picker(&mut self, pick: Pick, filter: &str) -> Option<Action> {
        let Some(options) = self.options.clone() else {
            return self.ask_for_options();
        };
        let (choices, current) = match pick {
            Pick::Agent => {
                let choices = options
                    .agents
                    .iter()
                    .map(|a| Choice::new(&a.name, &a.name, &a.description))
                    .collect();
                (choices, self.agent().to_string())
            }
            Pick::Model => {
                let default = match options.default_model.as_str() {
                    "" => "OpenCode picks".to_string(),
                    model => model.to_string(),
                };
                let models = options
                    .models
                    .iter()
                    .map(|m| Choice::new(&m.id, &m.name, format!("{} · {}", m.provider, m.id)));
                let choices = std::iter::once(Choice::new("", "Default", default))
                    .chain(models)
                    .collect();
                (choices, self.settings.model.clone())
            }
            Pick::Effort => {
                let efforts = self.efforts_or_explain()?;
                let choices = std::iter::once(Choice::new("", "Default", "the model's own"))
                    .chain(efforts.iter().map(|e| Choice::new(e, e, "")))
                    .collect();
                (choices, self.settings.effort.clone())
            }
            Pick::Command => {
                if options.commands.is_empty() {
                    self.info("the agent has no commands or skills");
                    return None;
                }
                let choices = options
                    .commands
                    .iter()
                    .map(|c| {
                        let detail = match c.description.as_str() {
                            "" => c.source.clone(),
                            text => format!("{} · {text}", c.source),
                        };
                        Choice::new(&c.name, format!("/{}", c.name), detail)
                    })
                    .collect();
                (choices, String::new())
            }
            Pick::Mcp => {
                if options.mcp_servers.is_empty() {
                    self.info("the agent has no MCP servers configured");
                    return None;
                }
                let choices = options
                    .mcp_servers
                    .iter()
                    .map(|m| Choice::new(&m.name, &m.name, mcp_status(m)))
                    .collect();
                (choices, String::new())
            }
            // The app's own.
            Pick::Session | Pick::Harness => return None,
        };
        let mut picker = Picker::new(pick, choices, &current);
        for c in filter.chars() {
            picker.on_key(KeyEvent::from(KeyCode::Char(c)));
        }
        self.picker = Some(picker);
        None
    }

    /// The current model's efforts, or `None` after saying why there are none.
    fn efforts_or_explain(&mut self) -> Option<Vec<String>> {
        match self.efforts() {
            Some([]) => {
                let model = self.model().to_string();
                self.info(&format!("{model} has no thinking efforts"));
                None
            }
            Some(efforts) => Some(efforts.to_vec()),
            None => {
                self.info(
                    "pick a model with /model first (the default one is known after a reply)",
                );
                None
            }
        }
    }

    fn choose(&mut self, pick: Pick, value: String) -> Option<Action> {
        match pick {
            Pick::Agent => self.settings.agent = value,
            Pick::Effort => self.settings.effort = value,
            Pick::Model => {
                self.settings.model = value;
                // An effort the new model doesn't know would be refused.
                let effort = &self.settings.effort;
                if !effort.is_empty() && self.efforts().is_some_and(|e| !e.contains(effort)) {
                    let model = self.model().to_string();
                    let effort = std::mem::take(&mut self.settings.effort);
                    self.info(&format!(
                        "{model} has no {effort:?} effort; back to its default"
                    ));
                }
            }
            Pick::Session | Pick::Harness => {}
            // Ready for its arguments.
            Pick::Command => self.input.insert_str(&format!("/{value} ")),
            Pick::Mcp => {
                let connect = self.options.as_ref().is_some_and(|o| {
                    o.mcp_servers
                        .iter()
                        .any(|m| m.name == value && m.status != "connected")
                });
                let doing = if connect {
                    "connecting"
                } else {
                    "disconnecting"
                };
                self.info(&format!("{doing} {value}…"));
                self.switching_mcp = Some(value.clone());
                return Some(Action::SwitchMcp {
                    node: self.node.id.clone(),
                    name: value,
                    connect,
                });
            }
        }
        None
    }

    /// Switches to the next (or previous) agent.
    fn cycle_agent(&mut self, step: isize) -> Option<Action> {
        let Some(options) = self.options.clone() else {
            return self.ask_for_options();
        };
        let names: Vec<&str> = options.agents.iter().map(|a| a.name.as_str()).collect();
        if let Some(next) = cycle(&names, self.agent(), step) {
            self.settings.agent = next.to_string();
        }
        None
    }

    /// Switches to the next effort, wrapping round to the model's default.
    fn cycle_effort(&mut self) -> Option<Action> {
        if self.options.is_none() {
            return self.ask_for_options();
        }
        let efforts = self.efforts_or_explain()?;
        let choices: Vec<&str> = std::iter::once("")
            .chain(efforts.iter().map(String::as_str))
            .collect();
        if let Some(next) = cycle(&choices, &self.settings.effort, 1) {
            self.settings.effort = next.to_string();
        }
        None
    }

    /// Cancels the task; one not started yet is cancelled as soon as it is.
    fn cancel(&mut self) -> Option<Action> {
        let Activity::Working {
            task_id,
            cancelling,
            ..
        } = &mut self.activity
        else {
            return None;
        };
        if *cancelling {
            return None;
        }
        *cancelling = true;
        task_id.clone().map(Action::Cancel)
    }

    /// Takes in what a background task reports, which may call for an action.
    pub fn on_message(&mut self, message: Message) -> Option<Action> {
        match message {
            Message::Node(node) => self.node = node,
            Message::Options(options) => {
                // Every chat on the node hears; the one that asked explains.
                let asked = std::mem::take(&mut self.fetching_options);
                let switched = self.switching_mcp.take();
                match options {
                    Ok(options) => {
                        let server = switched
                            .and_then(|name| options.mcp_servers.iter().find(|m| m.name == name));
                        if let Some(server) = server {
                            let status = mcp_status(server);
                            self.info(&format!("{}: {status}", server.name));
                        }
                        self.options = Some(Arc::new(options));
                    }
                    Err(e) if asked || switched.is_some() => {
                        let what = match switched {
                            Some(name) => format!("couldn't switch {name}"),
                            None => "couldn't list the agent's options".to_string(),
                        };
                        self.push(Role::Error, &format!("{what}: {e}"));
                    }
                    Err(_) => {}
                }
            }
            Message::Failed(error) => {
                self.flush_output();
                self.push(Role::Error, &error);
                self.activity = Activity::Idle;
                self.unseen = Some(Unseen::Failed);
            }
            Message::Task(TaskEvent::Started(started)) => {
                if let Activity::Working {
                    task_id,
                    cancelling,
                    ..
                } = &mut self.activity
                {
                    *task_id = Some(started.task_id.clone());
                    // Esc was pressed before the task had an id.
                    if *cancelling {
                        return Some(Action::Cancel(started.task_id));
                    }
                }
            }
            Message::Task(TaskEvent::Output(output)) => match output.stream() {
                OutputStream::Stderr => {
                    let text = decode(&mut self.partial_stderr, &output.data);
                    self.note(&text);
                }
                OutputStream::Reasoning => {
                    let text = decode(&mut self.partial_reasoning, &output.data);
                    self.append(Role::Thinking, &text);
                }
                _ => {
                    let text = decode(&mut self.partial_stdout, &output.data);
                    self.append(Role::Agent, &text);
                }
            },
            Message::Task(TaskEvent::Finished(finished)) => self.finish(finished),
        }
        None
    }

    fn finish(&mut self, finished: TaskFinished) {
        self.flush_output();
        if !finished.session_id.is_empty() {
            self.settings.session_id = finished.session_id.clone();
        }
        if !finished.model.is_empty() {
            self.used_model = finished.model.clone();
        }
        if let Some(usage) = &finished.usage {
            self.spent += usage.cost;
            if usage.context > 0 {
                self.context = usage.context;
            }
        }
        let summary = match &self.activity {
            Activity::Working {
                since,
                agent,
                effort,
                ..
            } => Some(self.summary(&finished, agent, effort, since.elapsed())),
            Activity::Idle => None,
        };
        let mut unseen = Unseen::Failed;
        if finished.cancelled {
            self.info("cancelled");
        } else if !finished.error.is_empty() {
            self.push(Role::Error, &finished.error);
        } else if finished.exit_code != Some(0) {
            self.push(Role::Error, "the agent failed");
        } else {
            unseen = Unseen::Done;
        }
        self.unseen = Some(unseen);
        if let Some(summary) = summary {
            self.push(Role::Summary, &summary);
        }
        self.activity = Activity::Idle;
    }

    /// The worker reports tools and errors as `[opencode] …` lines.
    fn note(&mut self, text: &str) {
        self.partial_note.push_str(text);
        while let Some(end) = self.partial_note.find('\n') {
            let line: String = self.partial_note.drain(..=end).collect();
            let line = line.trim_end();
            match line.strip_prefix(NOTE_PREFIX) {
                Some(tool) => self.push(Role::Tool, tool),
                None if !line.is_empty() => self.push(Role::Error, line),
                None => {}
            }
        }
    }

    /// `build · Claude Sonnet 5 · high · 12.3s · 12.6k in · 184 out · $0.0123`
    fn summary(
        &self,
        finished: &TaskFinished,
        agent: &str,
        effort: &str,
        took: Duration,
    ) -> String {
        // A cancelled turn doesn't say which model it had.
        let model = match finished.model.as_str() {
            "" => self.model(),
            model => model,
        };
        let mut parts = vec![agent.to_string(), self.model_name(model).to_string()];
        if !effort.is_empty() {
            parts.push(effort.to_string());
        }
        parts.push(elapsed(took));
        if let Some(usage) = finished.usage.as_ref().filter(|u| u.input + u.output > 0) {
            let input = usage.input + usage.cache_read + usage.cache_write;
            parts.push(format!("{} in", count(input)));
            parts.push(format!("{} out", count(usage.output + usage.reasoning)));
            if usage.cost > 0.0 {
                parts.push(dollars(usage.cost));
            }
        }
        parts.retain(|p| !p.is_empty());
        parts.join(" · ")
    }

    fn flush_output(&mut self) {
        self.partial_stdout.clear();
        self.partial_stderr.clear();
        self.partial_reasoning.clear();
        let rest = std::mem::take(&mut self.partial_note);
        self.note(&format!("{rest}\n"));
    }

    pub fn info(&mut self, text: &str) {
        self.push(Role::Info, text);
    }

    fn push(&mut self, role: Role, text: &str) {
        self.thread.push(Entry {
            role,
            text: text.to_string(),
            agent: self.agent().to_string(),
        });
    }

    /// Extends the last entry if it has the same role, so a streamed reply
    /// stays one entry.
    fn append(&mut self, role: Role, text: &str) {
        match self.thread.last_mut() {
            Some(last) if last.role == role => last.text.push_str(text),
            _ => self.push(role, text),
        }
    }
}

/// `184`, `12.6k`, `1.2M`.
pub fn count(n: u64) -> String {
    let (value, unit) = match n {
        0..1_000 => return n.to_string(),
        1_000..1_000_000 => (n as f64 / 1e3, "k"),
        _ => (n as f64 / 1e6, "M"),
    };
    let value = format!("{value:.1}");
    format!("{}{unit}", value.trim_end_matches(".0"))
}

/// `$0.0042`, `$1.23`: more digits for small amounts.
pub fn dollars(cost: f64) -> String {
    if cost < 0.01 {
        format!("${cost:.4}")
    } else {
        format!("${cost:.2}")
    }
}

/// `4.2s`, `2m 03s`.
pub fn elapsed(took: Duration) -> String {
    let secs = took.as_secs();
    if secs < 60 {
        format!("{:.1}s", took.as_secs_f64())
    } else {
        format!("{}m {:02}s", secs / 60, secs % 60)
    }
}

/// `connected`, or `failed: why`.
fn mcp_status(server: &McpServer) -> String {
    match server.error.as_str() {
        "" => server.status.clone(),
        error => format!("{}: {error}", server.status),
    }
}

/// The item `step` places after `current`, wrapping round; the first item if
/// `current` isn't there.
pub fn cycle<'a>(items: &[&'a str], current: &str, step: isize) -> Option<&'a str> {
    let len = items.len() as isize;
    if len == 0 {
        return None;
    }
    let next = match items.iter().position(|&i| i == current) {
        Some(at) => (at as isize + step).rem_euclid(len),
        None => 0,
    };
    Some(items[next as usize])
}

/// Decodes output, holding back a character split across chunks in `partial`.
fn decode(partial: &mut Vec<u8>, data: &[u8]) -> String {
    partial.extend_from_slice(data);
    let complete = match std::str::from_utf8(partial) {
        Ok(_) => partial.len(),
        // Invalid bytes, not a cut: let the lossy conversion show them.
        Err(e) if e.error_len().is_some() => partial.len(),
        Err(e) => e.valid_up_to(),
    };
    let rest = partial.split_off(complete);
    let text = String::from_utf8_lossy(partial).into_owned();
    *partial = rest;
    text
}

/// A single-line text field. `cursor` counts characters, not bytes.
#[derive(Default)]
pub struct Input {
    pub text: String,
    pub cursor: usize,
}

impl Input {
    fn byte_index(&self) -> usize {
        self.text
            .char_indices()
            .nth(self.cursor)
            .map_or(self.text.len(), |(i, _)| i)
    }

    fn insert(&mut self, c: char) {
        let at = self.byte_index();
        self.text.insert(at, c);
        self.cursor += 1;
    }

    fn insert_str(&mut self, s: &str) {
        let at = self.byte_index();
        self.text.insert_str(at, s);
        self.cursor += s.chars().count();
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.delete();
        }
    }

    fn delete(&mut self) {
        let at = self.byte_index();
        if at < self.text.len() {
            self.text.remove(at);
        }
    }

    fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.text.chars().count());
    }

    fn home(&mut self) {
        self.cursor = 0;
    }

    fn end(&mut self) {
        self.cursor = self.text.chars().count();
    }

    fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn chat() -> Chat {
        let node = NodeInfo {
            id: "n1".into(),
            name: "w1".into(),
            harnesses: vec!["opencode".into()],
            ..Default::default()
        };
        let settings = Settings {
            session_id: String::new(),
            cwd: String::new(),
            model: String::new(),
            agent: String::new(),
            effort: String::new(),
        };
        Chat::new(1, node, settings, None)
    }

    pub(crate) fn output(stream: OutputStream, data: &[u8]) -> Message {
        Message::Task(TaskEvent::Output(TaskOutput {
            stream: stream as i32,
            data: data.to_vec(),
            ..Default::default()
        }))
    }

    fn type_text(app: &mut Chat, text: &str) {
        for c in text.chars() {
            app.on_key(KeyEvent::from(KeyCode::Char(c)));
        }
    }

    #[test]
    fn a_turn_streams_into_the_thread_and_keeps_the_session() {
        let mut app = chat();
        type_text(&mut app, "hi");
        let Some(Action::Send(_, request)) = app.on_key(KeyEvent::from(KeyCode::Enter)) else {
            panic!("Enter should send");
        };
        assert_eq!(
            (request.prompt.as_str(), request.node.as_str()),
            ("hi", "n1")
        );
        assert!(app.input.text.is_empty());

        // Typing on while the agent works is fine, sending isn't.
        type_text(&mut app, "next");
        assert!(app.on_key(KeyEvent::from(KeyCode::Enter)).is_none());
        assert_eq!(app.input.text, "next");

        app.on_message(Message::Task(TaskEvent::Started(TaskStarted {
            task_id: "t1".into(),
            ..Default::default()
        })));
        // "é" split across two chunks.
        app.on_message(output(OutputStream::Reasoning, b"let me think"));
        app.on_message(output(OutputStream::Stdout, b"Hello caf\xc3"));
        app.on_message(output(OutputStream::Stderr, b"[opencode] write a.txt\n"));
        app.on_message(output(OutputStream::Stdout, b"\xa9 done"));
        app.on_message(Message::Task(TaskEvent::Finished(TaskFinished {
            task_id: "t1".into(),
            exit_code: Some(0),
            session_id: "ses_1".into(),
            model: "a/smart".into(),
            usage: Some(AgentUsage {
                input: 1200,
                output: 34,
                cost: 0.5,
                context: 1234,
                ..Default::default()
            }),
            ..Default::default()
        })));

        let summary = app.thread.last().unwrap().text.clone();
        assert!(
            summary.starts_with("a/smart · ") && summary.ends_with(" · 1.2k in · 34 out · $0.50"),
            "{summary}"
        );
        let thread: Vec<_> = app
            .thread
            .iter()
            .map(|e| (e.role, e.text.as_str()))
            .collect();
        assert_eq!(
            thread,
            [
                (Role::User, "hi"),
                (Role::Thinking, "let me think"),
                (Role::Agent, "Hello caf"),
                (Role::Tool, "write a.txt"),
                (Role::Agent, "é done"),
                (Role::Summary, summary.as_str()),
            ]
        );
        assert_eq!((app.spent, app.context), (0.5, 1234));
        assert!(matches!(app.activity, Activity::Idle));

        // The next prompt continues the session.
        let Some(Action::Send(_, request)) = app.on_key(KeyEvent::from(KeyCode::Enter)) else {
            panic!("Enter should send once idle");
        };
        assert_eq!(request.session_id, "ses_1");
    }

    fn started(task_id: &str) -> Message {
        Message::Task(TaskEvent::Started(TaskStarted {
            task_id: task_id.into(),
            ..Default::default()
        }))
    }

    #[test]
    fn esc_cancels_the_task_once() {
        let mut app = chat();
        type_text(&mut app, "hi");
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(app.on_message(started("t1")).is_none());
        assert!(matches!(
            app.on_key(KeyEvent::from(KeyCode::Esc)),
            Some(Action::Cancel(id)) if id == "t1"
        ));
        assert!(app.on_key(KeyEvent::from(KeyCode::Esc)).is_none());
    }

    #[test]
    fn esc_before_the_task_starts_cancels_it_when_it_does() {
        let mut app = chat();
        type_text(&mut app, "hi");
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(app.on_key(KeyEvent::from(KeyCode::Esc)).is_none());
        assert!(matches!(
            app.activity,
            Activity::Working {
                cancelling: true,
                ..
            }
        ));
        assert!(matches!(
            app.on_message(started("t1")),
            Some(Action::Cancel(id)) if id == "t1"
        ));
    }

    fn with_options() -> Chat {
        let mut app = chat();
        let model = |id: &str, variants: &[&str]| ModelChoice {
            id: id.into(),
            name: id.into(),
            variants: variants.iter().map(|v| v.to_string()).collect(),
            ..Default::default()
        };
        let agent = |name: &str| AgentChoice {
            name: name.into(),
            ..Default::default()
        };
        app.on_message(Message::Options(Ok(AgentOptions {
            agents: vec![agent("build"), agent("plan"), agent("review")],
            models: vec![model("a/fast", &[]), model("a/smart", &["low", "high"])],
            default_agent: "build".into(),
            commands: vec![AgentCommand {
                name: "review".into(),
                description: "Review the changes".into(),
                source: "skill".into(),
            }],
            mcp_servers: vec![McpServer {
                name: "docs".into(),
                status: "disabled".into(),
                ..Default::default()
            }],
            ..Default::default()
        })));
        app
    }

    #[test]
    fn the_agents_commands_run_with_their_arguments() {
        let mut app = with_options();
        let Some(Action::Send(_, request)) = command(&mut app, "/review  the parser ") else {
            panic!("a known command is sent");
        };
        assert_eq!(
            (request.command.as_str(), request.prompt.as_str()),
            ("review", "the parser")
        );
        assert_eq!(app.thread.last().unwrap().text, "/review  the parser");
        app.on_message(Message::Failed("stop".into()));

        // Any other slash is just text.
        let Some(Action::Send(_, request)) = command(&mut app, "/etc/hosts?") else {
            panic!("an unknown command is a prompt");
        };
        assert_eq!(
            (request.command.as_str(), request.prompt.as_str()),
            ("", "/etc/hosts?")
        );
        app.on_message(Message::Failed("stop".into()));

        // Picking one readies it for its arguments.
        command(&mut app, "/skills");
        assert_eq!(app.picker.as_ref().unwrap().shown_len(), 1);
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.input.text, "/review ");
    }

    #[test]
    fn mcp_servers_are_switched_from_the_floating_window() {
        let mut app = with_options();
        command(&mut app, "/mcp");
        let Some(Action::SwitchMcp { name, connect, .. }) =
            app.on_key(KeyEvent::from(KeyCode::Enter))
        else {
            panic!("choosing a server switches it");
        };
        assert_eq!((name.as_str(), connect), ("docs", true));

        let mut options = (**app.options.as_ref().unwrap()).clone();
        options.mcp_servers[0].status = "connected".into();
        app.on_message(Message::Options(Ok(options)));
        assert_eq!(app.thread.last().unwrap().text, "docs: connected");

        command(&mut app, "/mcp");
        assert!(matches!(
            app.on_key(KeyEvent::from(KeyCode::Enter)),
            Some(Action::SwitchMcp { connect: false, .. })
        ));
    }

    fn command(app: &mut Chat, text: &str) -> Option<Action> {
        type_text(app, text);
        app.on_key(KeyEvent::from(KeyCode::Enter))
    }

    #[test]
    fn tab_cycles_agents_once_the_node_has_said_which() {
        let mut app = chat();
        assert!(app.on_key(KeyEvent::from(KeyCode::Tab)).is_none());
        assert_eq!(app.thread.last().unwrap().role, Role::Info);

        let mut app = with_options();
        assert_eq!(app.agent(), "build");
        app.on_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.agent(), "plan");
        app.on_key(KeyEvent::from(KeyCode::BackTab));
        app.on_key(KeyEvent::from(KeyCode::BackTab));
        assert_eq!(app.agent(), "review");

        // A failed fetch can be retried.
        let mut failed = chat();
        failed.on_message(Message::Options(Err("offline".into())));
        assert!(matches!(
            failed.on_key(KeyEvent::from(KeyCode::Tab)),
            Some(Action::FetchOptions(_))
        ));
    }

    #[test]
    fn model_and_effort_are_picked_in_the_floating_window() {
        let mut app = with_options();
        // The default model is unknown until a reply names it.
        command(&mut app, "/effort");
        assert!(app.picker.is_none());

        command(&mut app, "/model smart");
        let picker = app.picker.as_ref().expect("/model opens the picker");
        assert_eq!(picker.shown_len(), 1);
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(app.picker.is_none());
        assert_eq!(app.settings.model, "a/smart");
        assert!(app.input.text.is_empty());

        command(&mut app, "/effort");
        app.on_key(KeyEvent::from(KeyCode::Down));
        app.on_key(KeyEvent::from(KeyCode::Down));
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.settings.effort, "high");
        // Ctrl-T wraps round to the model's default.
        app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert_eq!(app.settings.effort, "");
        app.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert_eq!(app.settings.effort, "low");

        // A model without that effort drops it.
        command(&mut app, "/model fast");
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.settings.model, "a/fast");
        assert_eq!(app.settings.effort, "");

        let Some(Action::Send(_, request)) = command(&mut app, "hi") else {
            panic!("a prompt is sent");
        };
        assert_eq!(request.model, "a/fast");
        assert_eq!(request.agent, "");
    }

    #[test]
    fn odd_input_is_handled() {
        let mut app = chat();
        // Blank input, and a lone slash, send nothing useful.
        assert!(command(&mut app, "   ").is_none());
        let Some(Action::Send(_, request)) = command(&mut app, "/") else {
            panic!("a lone slash is just text");
        };
        assert_eq!(
            (request.prompt.as_str(), request.command.as_str()),
            ("/", "")
        );
        app.on_message(Message::Failed("stop".into()));

        // Before the node has listed its commands, a command is just text.
        let Some(Action::Send(_, request)) = command(&mut app, "/review x") else {
            panic!("sent as a prompt");
        };
        assert_eq!(
            (request.prompt.as_str(), request.command.as_str()),
            ("/review x", "")
        );
    }

    #[test]
    fn the_title_is_the_first_prompt_sent() {
        let mut app = with_options();
        command(&mut app, "/model");
        app.on_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(app.title, "", "commands aren't prompts");
        command(&mut app, "first");
        // Typed while it works: kept, not sent, not the title.
        assert!(command(&mut app, "second").is_none());
        app.on_message(Message::Failed("stop".into()));
        app.input.clear();
        command(&mut app, "third");
        assert_eq!(app.title, "first");
    }

    #[test]
    fn a_failed_stream_ends_the_turn_and_marks_it() {
        let mut app = chat();
        command(&mut app, "hi");
        app.on_message(started("t1"));
        app.on_message(output(OutputStream::Stdout, b"partial \xc3"));
        app.on_message(Message::Failed("the stream ended".into()));
        assert!(matches!(app.activity, Activity::Idle));
        assert_eq!(app.unseen, Some(Unseen::Failed));
        assert!(app.running_task().is_none());
        // Esc after the turn ended does nothing.
        assert!(app.on_key(KeyEvent::from(KeyCode::Esc)).is_none());
        // The half character is dropped, not glued to the next reply.
        command(&mut app, "again");
        app.on_message(output(OutputStream::Stdout, b"ok"));
        assert_eq!(app.thread.last().unwrap().text, "ok");
    }

    #[test]
    fn input_edits_by_character() {
        let mut input = Input::default();
        for c in "héllo".chars() {
            input.insert(c);
        }
        input.home();
        input.right();
        input.delete();
        input.end();
        input.backspace();
        assert_eq!(input.text, "hll");
        assert_eq!(input.cursor, 3);
    }
}
