//! One chat: a session with a node's agent, what the person asks of it, and
//! what its task events make of it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use commandant_common::or;
use commandant_proto::task_event::Event as TaskEvent;
use commandant_proto::*;

use super::{ChatId, Choose, Edit, Effect, Outcome, Pick};

mod format;
mod menus;
mod projects;
mod sign_in;
#[cfg(test)]
pub(crate) mod tests;

pub use self::format::{count, dollars, elapsed};
use self::menus::Menu;
pub use self::sign_in::Auth;
/// What a submitted line asks of the app rather than the chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AppCommand {
    Quit,
    New,
    Sessions,
    Nodes,
    Close,
}

/// The chat's own commands, for completing and `/help`.
pub const COMMANDS: [(&str, &str); 18] = [
    ("help", "what every command and key does"),
    ("agent", "choose an agent"),
    ("model", "choose a model"),
    ("effort", "choose a thinking effort"),
    ("skills", "the agent's commands and skills"),
    ("mcp", "connect or disconnect MCP servers"),
    ("providers", "sign in to or out of a model provider"),
    (
        "project",
        "browse the node's projects, or clone one: /project <repository>",
    ),
    ("new", "another session, working alongside"),
    ("sessions", "switch, or resume a saved one"),
    ("close", "close this session"),
    ("nodes", "the other nodes"),
    ("commands", "same as /skills"),
    ("login", "same as /providers"),
    ("quit", "leave"),
    ("exit", "same as /quit"),
    ("logout", "same as /providers"),
    ("?", "same as /help"),
];
/// The terminal's keys, for `/help` and an empty chat.
pub const KEYS: [(&str, &str); 11] = [
    ("Tab", "switch agent, or complete a /command"),
    ("↑ ↓", "earlier prompts, or move in the /command list"),
    ("Ctrl-T", "next thinking effort"),
    ("Esc", "cancel a turn"),
    ("PgUp PgDn", "scroll"),
    ("Ctrl-N", "new session"),
    ("Ctrl-O", "sessions"),
    ("Alt-← Alt-→", "previous / next session"),
    ("Ctrl-W", "close this session"),
    ("Ctrl-G", "the other nodes"),
    ("Ctrl-U", "clear the prompt"),
];

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
    Options(Result<Arc<AgentOptions>, String>),
    /// A resumed session's earlier turns.
    History(Result<Vec<HistoryEntry>, String>),
    Providers(Result<Vec<ModelProvider>, String>),
    /// How a step of signing in went.
    Auth(Result<ProviderAuthResult, String>),
    /// Where the project asked for is ready to work in.
    Project(Result<ProjectReady, String>),
    Projects(Result<Vec<commandant_proto::Project>, String>),
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

#[derive(Default)]
pub enum Activity {
    #[default]
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

#[derive(Default)]
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
    /// Bytes of a UTF-8 character split across output chunks, per stream.
    partial_stdout: Vec<u8>,
    partial_stderr: Vec<u8>,
    partial_reasoning: Vec<u8>,
    /// A note line still waiting for its newline.
    partial_note: String,
    /// The agents, models and efforts the node offers, once it has said.
    pub options: Option<Arc<AgentOptions>>,
    pub fetching_options: bool,
    /// Waiting for a resumed session's earlier turns.
    pub loading_history: bool,
    /// The node's model providers, as last listed; the filter to open their
    /// picker with while they're listed.
    providers: Vec<ModelProvider>,
    pub listing_providers: Option<String>,
    pub auth: Option<Auth>,
    /// The repository the node is cloning for this chat.
    pub preparing: Option<String>,
    /// Waiting for the node's projects, to browse them.
    pub listing_projects: bool,
    /// A picker asked for before the options came, to open once they do.
    pending: Option<(Menu, String)>,
    /// The model that last replied, which tells what "default" means.
    pub used_model: String,
    /// What to choose from, when asked.
    pub pick: Option<Pick>,
    /// What was sent, oldest first, to bring back.
    sent: Vec<String>,
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
            node,
            settings,
            fetching_options: options.is_none(),
            options,
            ..Default::default()
        }
    }

    /// What it is called in its tab and the session picker.
    pub fn title(&self) -> String {
        or(&self.title, "new session").to_string()
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

    pub fn running_task(&self) -> Option<String> {
        match &self.activity {
            Activity::Working { task_id, .. } => task_id.clone(),
            Activity::Idle => None,
        }
    }

    /// What was sent, oldest first.
    pub fn sent(&self) -> &[String] {
        &self.sent
    }

    /// Whether the next line submitted is an API key or code, kept out of
    /// the thread.
    pub fn awaiting_secret(&self) -> bool {
        matches!(self.auth, Some(Auth::Key { .. } | Auth::Code { .. }))
    }

    /// Takes a line: a prompt, a `/command`, or the secret asked for. A line
    /// that is the app's to carry out comes back as its command.
    pub(super) fn submit(&mut self, text: &str) -> (Outcome, Option<AppCommand>) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Default::default();
        }
        if let Some(effect) = self.send_secret(&text) {
            return (self.cleared(Some(effect)), None);
        }
        if self.sent.last() != Some(&text) {
            self.sent.push(text.clone());
        }
        if let Some(command) = text.strip_prefix('/') {
            let (command, filter) = command.split_once(' ').unwrap_or((command, ""));
            let app = match command {
                "quit" | "exit" => return (Outcome::default(), Some(AppCommand::Quit)),
                "new" => Some(AppCommand::New),
                "sessions" => Some(AppCommand::Sessions),
                "nodes" => Some(AppCommand::Nodes),
                "close" => Some(AppCommand::Close),
                _ => None,
            };
            if app.is_some() {
                return (self.cleared(None), app);
            }
            let menu = match command {
                "help" | "?" => {
                    self.help();
                    return (self.cleared(None), None);
                }
                "agent" => Menu::Agent,
                "model" => Menu::Model,
                "effort" => Menu::Effort,
                "commands" | "skills" => Menu::Command,
                "mcp" => Menu::Mcp,
                "providers" | "login" | "logout" => {
                    let effect = self.list_providers(filter.trim());
                    return (self.cleared(effect), None);
                }
                "project" => {
                    let effect = self.project(filter.trim());
                    return (self.cleared(effect), None);
                }
                // One of the agent's own commands or skills, its arguments after it.
                name if self.is_agent_command(name) => {
                    let (name, arguments) = (name.to_string(), filter.trim().to_string());
                    return (self.send(text, name, arguments), None);
                }
                // It may be one of those, once they've loaded.
                _ if self.loading() => {
                    self.info("the agent's commands are still loading; send it again in a moment");
                    return Default::default();
                }
                // Not a command: a prompt that starts with a slash.
                _ => return (self.send(text.clone(), String::new(), text), None),
            };
            let effect = self.open_picker(menu, filter.trim());
            return (self.cleared(effect), None);
        }
        (self.send(text.clone(), String::new(), text), None)
    }

    /// What `effect` asks, the line it came from taken out of the prompt.
    fn cleared(&self, effect: Option<Effect>) -> Outcome {
        Outcome {
            prompt: Some((self.id, Edit::Clear)),
            ..effect.into()
        }
    }

    /// The commands, the chat's own then the agent's, that complete the
    /// `/name` being typed in `text` (before its arguments), with what they do.
    pub fn completions(&self, text: &str) -> Vec<(String, String)> {
        let Some(typed) = text.strip_prefix('/') else {
            return Vec::new();
        };
        if typed.contains(' ') || self.auth.is_some() {
            return Vec::new();
        }
        let own = COMMANDS.iter().map(|(n, d)| (n.to_string(), d.to_string()));
        let agent = self
            .options
            .iter()
            .flat_map(|o| &o.commands)
            .map(|c| (c.name.clone(), or(&c.description, &c.source).to_string()));
        own.chain(agent)
            .filter(|(n, _)| n.starts_with(typed))
            .collect()
    }

    /// Whether `name` is one of the chat's commands or the agent's.
    pub fn is_command(&self, name: &str) -> bool {
        COMMANDS.iter().any(|(n, _)| *n == name) || self.is_agent_command(name)
    }

    /// How many characters of `text` name a known `/command`, before its
    /// arguments; 0 when it names none.
    pub fn command_len(&self, text: &str) -> usize {
        let Some(typed) = text.strip_prefix('/') else {
            return 0;
        };
        let name = typed.split(' ').next().unwrap_or_default();
        match self.is_command(name) {
            true => 1 + name.chars().count(),
            false => 0,
        }
    }

    /// Every command and key, in the thread.
    fn help(&mut self) {
        let mut lines = vec!["commands".to_string()];
        lines.extend(COMMANDS.iter().map(|(n, d)| format!("  /{n:<12}{d}")));
        if let Some(options) = self.options.clone().filter(|o| !o.commands.is_empty()) {
            lines.push("the agent's commands and skills".into());
            let agent = options.commands.iter();
            lines.extend(
                agent.map(|c| format!("  /{:<12}{}", c.name, or(&c.description, &c.source))),
            );
        }
        lines.push("keys".into());
        lines.extend(KEYS.iter().map(|(k, d)| format!("  {k:<13}{d}")));
        self.info(&lines.join("\n"));
    }

    /// Whether the commands and MCP servers are still to come.
    pub fn loading(&self) -> bool {
        self.options.as_ref().is_some_and(|o| o.loading)
    }

    fn is_agent_command(&self, name: &str) -> bool {
        self.options
            .as_ref()
            .is_some_and(|o| o.commands.iter().any(|c| c.name == name))
    }

    /// Sends `prompt` (or runs `command` with it), showing `text` in the
    /// thread. While the agent works the text stays in the prompt, to send
    /// once it is done.
    fn send(&mut self, text: String, command: String, prompt: String) -> Outcome {
        if matches!(self.activity, Activity::Working { .. }) {
            return Outcome::default();
        }
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
        let request = PromptRequest {
            node: self.node.id.clone(),
            prompt,
            command,
            session_id: self.settings.session_id.clone(),
            cwd: self.settings.cwd.clone(),
            model: self.settings.model.clone(),
            agent: self.settings.agent.clone(),
            variant: self.settings.effort.clone(),
        };
        self.cleared(Some(Effect::Send(self.id, request)))
    }

    /// Asks the node what its agent offers, unless that's under way.
    fn ask_for_options(&mut self) -> Option<Effect> {
        if self.fetching_options {
            self.info("still asking the node what its agent offers…");
            return None;
        }
        // The state drops this if the node is answering already; the answer
        // reaches every chat on it.
        self.fetching_options = true;
        self.info("asking the node what its agent offers…");
        Some(Effect::fetch_options(&self.node.id))
    }

    /// Switches to the next (or previous) agent.
    pub(super) fn cycle_agent(&mut self, step: isize) -> Option<Effect> {
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
    pub(super) fn cycle_effort(&mut self) -> Option<Effect> {
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
    pub(super) fn cancel(&mut self) -> Option<Effect> {
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
        task_id.clone().map(Effect::Cancel)
    }

    /// Takes in what a background task reports, which may call for an effect.
    pub fn on_message(&mut self, message: Message) -> Option<Effect> {
        match message {
            Message::Node(node) => self.node = node,
            Message::Options(options) => {
                // Every chat on the node hears; one that was waiting explains.
                let waiting = std::mem::take(&mut self.fetching_options);
                let pending = self.pending.take();
                match options {
                    Ok(options) => {
                        self.options = Some(options);
                        if let Some((menu, filter)) = pending {
                            return self.open_picker(menu, &filter);
                        }
                    }
                    Err(e) if waiting || pending.is_some() => {
                        self.error(&format!("couldn't list the agent's options: {e}"))
                    }
                    Err(_) => {}
                }
            }
            Message::Providers(providers) => {
                let filter = self.listing_providers.take().unwrap_or_default();
                match providers {
                    Ok(providers) => {
                        self.providers = providers;
                        self.open_providers(&filter);
                    }
                    Err(e) => self.error(&format!("couldn't list the providers: {e}")),
                }
            }
            Message::Auth(result) => return self.signed_in(result),
            Message::Project(ready) => {
                let repository = self.preparing.take().unwrap_or_default();
                match ready {
                    Ok(ready) => self.work_in(&ready.path, &ready.id),
                    Err(e) => self.error(&format!("couldn't get {repository}: {e}")),
                }
            }
            Message::Projects(projects) => {
                self.listing_projects = false;
                match projects {
                    Ok(projects) => self.open_projects(projects),
                    Err(e) => self.error(&format!("couldn't list the projects: {e}")),
                }
            }
            Message::History(history) => {
                self.loading_history = false;
                match history {
                    Ok(entries) => {
                        let agent = self.agent().to_string();
                        let earlier = entries.into_iter().map(|e| Entry {
                            role: match e.role.as_str() {
                                "user" => Role::User,
                                "thinking" => Role::Thinking,
                                "tool" => Role::Tool,
                                _ => Role::Agent,
                            },
                            text: e.text,
                            agent: agent.clone(),
                        });
                        // Before whatever was said since resuming.
                        self.thread.splice(0..0, earlier);
                    }
                    Err(e) => self.error(&format!(
                        "couldn't load the session's earlier messages: {e}"
                    )),
                }
            }
            Message::Failed(error) => {
                self.flush_output();
                self.error(&error);
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
                        return Some(Effect::Cancel(started.task_id));
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
            self.error(&finished.error);
        } else if finished.exit_code != Some(0) {
            self.error("the agent failed");
        } else {
            unseen = Unseen::Done;
        }
        self.unseen = Some(unseen);
        if let Some(summary) = summary {
            self.push(Role::Summary, &summary);
        }
        self.activity = Activity::Idle;
    }

    /// The worker reports tools as `[<harness>] …` lines, and errors as
    /// any other.
    fn note(&mut self, text: &str) {
        let prefix = format!(
            "[{}] ",
            self.node.harnesses.first().map_or("", String::as_str)
        );
        self.partial_note.push_str(text);
        while let Some(end) = self.partial_note.find('\n') {
            let line: String = self.partial_note.drain(..=end).collect();
            let line = line.trim_end();
            match line.strip_prefix(&prefix) {
                Some(tool) => self.push(Role::Tool, tool),
                None if !line.is_empty() => self.error(line),
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

    fn error(&mut self, text: &str) {
        self.push(Role::Error, text);
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

/// The item `step` places after `current`, wrapping round; the first item if
/// `current` isn't there.
pub fn cycle<T: PartialEq + Copy>(items: &[T], current: T, step: isize) -> Option<T> {
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
