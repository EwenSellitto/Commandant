//! One chat: a session with a node's agent, what the person asks of it, and
//! what its task events make of it.

use std::sync::Arc;
use std::time::Instant;

use commandant_common::or;
use commandant_proto::task_event::Event as TaskEvent;
use commandant_proto::*;

use super::{Ask, ChatId, Effect, Outcome, cycle};

mod commands;
mod events;
mod format;
mod menus;
mod projects;
mod sign_in;
#[cfg(test)]
mod tests;

pub(in crate::state) use self::commands::AppCommand;
pub use self::commands::{COMMANDS, KEYS};
pub use self::format::{count, dollars, elapsed};
use self::menus::Menu;
use self::sign_in::SigningIn;

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

impl Activity {
    pub fn working(&self) -> bool {
        matches!(self, Self::Working { .. })
    }
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
    /// The node's model providers, as last listed; the filter to choose
    /// from them with while they're listed.
    providers: Vec<ModelProvider>,
    pub listing_providers: Option<String>,
    /// The node at a step of signing in.
    signing_in: Option<SigningIn>,
    /// The repository the node is cloning for this chat.
    pub preparing: Option<String>,
    /// Waiting for the node's projects, to browse them.
    pub listing_projects: bool,
    /// A menu asked for before the options came, to open once they do.
    pending: Option<(Menu, String)>,
    /// The model that last replied, which tells what "default" means.
    pub used_model: String,
    /// What the chat asks the person, if anything.
    pub(in crate::state) ask: Option<Ask>,
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

    /// What it is called in its tab and among the sessions.
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

    /// What the chat asks the person, if anything.
    pub fn ask(&self) -> Option<&Ask> {
        self.ask.as_ref()
    }

    /// Whether the chat asks for a line, which its prompt is then for.
    pub fn entering(&self) -> bool {
        self.ask.as_ref().is_some_and(Ask::wants_line)
    }

    /// Asks `ask` instead of what was asked; but a line being typed, a key
    /// say, is never pushed aside, so it stays hidden and goes where it was
    /// asked for.
    fn put(&mut self, ask: impl Into<Ask>) {
        if self.entering() {
            self.info("finish signing in first (Esc to cancel), then ask again");
            return;
        }
        self.ask = Some(ask.into());
    }

    /// What the node is doing to sign in, while it is at it.
    pub fn signing_in(&self) -> Option<&str> {
        self.signing_in.as_ref().map(|s| s.doing.as_str())
    }

    /// Whether the commands and MCP servers are still to come.
    pub fn loading(&self) -> bool {
        self.options.as_ref().is_some_and(|o| o.loading)
    }

    /// Sends `prompt` (or runs `command` with it), showing `text` in the
    /// thread. While the agent works the text stays in the prompt, to send
    /// once it is done.
    fn send(&mut self, text: String, command: String, prompt: String) -> Outcome {
        if self.activity.working() {
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
        task_id
            .clone()
            .map(|task_id| Effect::Cancel(self.id, task_id))
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
}
