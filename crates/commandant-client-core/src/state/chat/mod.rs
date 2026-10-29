//! One chat: a session with a node's agent, what the person asks of it, and
//! what its task events make of it.

use std::sync::Arc;
use std::time::Instant;

use commandant_common::or;
use commandant_proto::task_event::Event as TaskEvent;
use commandant_proto::{
    AgentOptions, HistoryEntry, ModelChoice, ModelProvider, NodeInfo, ProjectReady, PromptRequest,
    ProviderAuthResult,
};

use super::{Ask, ChatId, Effect, Outcome, cycle};

mod commands;
mod events;
mod format;
mod menus;
mod projects;
mod run;
mod sign_in;
#[cfg(test)]
mod tests;

pub(in crate::state) use self::commands::AppCommand;
pub use self::commands::{ALIASES, COMMANDS};
pub use self::format::{count, dollars, elapsed};
pub use self::menus::Menu;
pub use self::run::{Ended, Printed, Run, RunId};
use self::sign_in::SigningIn;

/// Sent with every prompt; `session_id` fills in after the first reply.
#[derive(Clone, Default)]
pub struct Settings {
    /// The agent's session to carry on; empty starts a new one.
    pub session_id: String,
    /// Where the agent works on the node; empty for its worker's directory.
    pub cwd: String,
    /// The model chosen; empty for the agent's default.
    pub model: String,
    /// The agent chosen; empty for the harness's default.
    pub agent: String,
    /// The thinking effort chosen (the model's variant); empty for its default.
    pub effort: String,
}

/// What background tasks report to a chat.
pub enum Message {
    /// What the turn's task reports.
    Task(TaskEvent),
    /// The prompt couldn't be sent, or its stream broke.
    Failed(String),
    /// What a command run from the chat reports, or why its stream broke.
    Run(RunId, Result<TaskEvent, String>),
    /// The task of the turn or of a command couldn't be cancelled.
    CancelFailed {
        /// The task, as the server names it.
        task_id: String,
        /// Why, as the server says.
        error: String,
    },
    /// The chat's node, as the server last listed it.
    Node(NodeInfo),
    /// What the node's agent offers, or why it couldn't say.
    Options(Result<Arc<AgentOptions>, String>),
    /// A resumed session's earlier turns.
    History(Result<Vec<HistoryEntry>, String>),
    /// The model providers the node's agent knows.
    Providers(Result<Vec<ModelProvider>, String>),
    /// How a step of signing in went.
    Auth(Result<ProviderAuthResult, String>),
    /// Where the project asked for is ready to work in.
    Project(Result<ProjectReady, String>),
    /// The projects the node has cloned, to browse.
    Projects(Result<Vec<commandant_proto::Project>, String>),
}

/// Who wrote an entry of the thread, which tells how it is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The person, prompting.
    User,
    /// The model's thinking, before or between its replies.
    Thinking,
    /// The agent's reply.
    Agent,
    /// A tool the agent used.
    Tool,
    /// Something that went wrong.
    Error,
    /// A remark from commandant itself.
    Info,
    /// What a finished turn used: agent, model, time, tokens, cost.
    Summary,
    /// A command run with `!`: its text is the command, its block the rest.
    Command,
}

/// One entry of a chat's thread.
pub struct Entry {
    /// Who wrote it.
    pub role: Role,
    /// What it says, as markdown for the agent's replies.
    pub text: String,
    /// The agent that was chosen when it was written.
    pub agent: String,
    /// A command's block: what it printed and how it ended.
    pub run: Option<Run>,
}

/// Whether the chat's agent is at work on a turn.
#[derive(Default)]
pub enum Activity {
    /// Nothing sent waits for an answer.
    #[default]
    Idle,
    /// A turn is under way.
    Working {
        /// Known once the orchestrator has started the task.
        task_id: Option<String>,
        /// When the prompt was sent.
        since: Instant,
        /// Asked to stop, not stopped yet.
        cancelling: bool,
        /// What the prompt was sent with, for the turn's summary.
        agent: String,
        /// The thinking effort the prompt was sent with.
        effort: String,
        /// Started elsewhere and watched here, so left running on quitting.
        watched: bool,
    },
}

impl Activity {
    /// Whether a turn is under way.
    pub fn working(&self) -> bool {
        matches!(self, Self::Working { .. })
    }
}

/// How a turn ended that nobody has looked at yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unseen {
    /// It finished.
    Done,
    /// It failed, or was cancelled.
    Failed,
}

/// A session with a node's agent, as one front end tab shows it.
#[derive(Default)]
pub struct Chat {
    /// Unique among the app's chats, never reused.
    pub id: ChatId,
    /// The session's title: its first prompt, or what the node saved.
    pub title: String,
    /// Set when a turn ends; the app clears it once the chat is shown.
    pub unseen: Option<Unseen>,
    /// The node the agent runs on, as last listed.
    pub node: NodeInfo,
    /// What the next prompt is sent with.
    pub settings: Settings,
    /// What was said, oldest first.
    pub thread: Vec<Entry>,
    /// Whether a turn is under way.
    pub activity: Activity,
    /// The agents, models and efforts the node offers, once it has said.
    pub options: Option<Arc<AgentOptions>>,
    /// Waiting for the node to say what it offers.
    pub fetching_options: bool,
    /// Waiting for a resumed session's earlier turns.
    pub loading_history: bool,
    /// The filter to choose a model provider with, while they're listed.
    pub listing_providers: Option<String>,
    /// The repository the node is cloning for this chat.
    pub preparing: Option<String>,
    /// Waiting for the node's projects, to browse them.
    pub listing_projects: bool,
    /// The model that last replied, which tells what "default" means.
    pub used_model: String,
    /// What the session has cost so far, in US dollars.
    pub spent: f64,
    /// Tokens in the session's context after the last turn.
    pub context: u64,
    /// What the chat asks the person, if anything.
    pub(in crate::state) ask: Option<Ask>,
    /// Bytes of a UTF-8 character split across output chunks, per stream.
    partial_stdout: Vec<u8>,
    partial_stderr: Vec<u8>,
    partial_reasoning: Vec<u8>,
    /// A note line still waiting for its newline.
    partial_note: String,
    /// The prompt of the turn attached to, which its history, still to
    /// come, ends with.
    attached: Option<String>,
    /// The node's model providers, as last listed.
    providers: Vec<ModelProvider>,
    /// The node at a step of signing in.
    signing_in: Option<SigningIn>,
    /// A menu asked for before the options came, to open once they do.
    pending: Option<(Menu, String)>,
    /// What was sent, oldest first, to bring back.
    sent: Vec<String>,
    next_run: RunId,
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

    /// The model the next prompt goes to, by name: `default model` until
    /// one is chosen or has replied.
    pub fn model_label(&self) -> String {
        match (self.settings.model.as_str(), self.model()) {
            ("", "") => "default model".to_string(),
            (_, id) => self.model_name(id).to_string(),
        }
    }

    /// `high effort`, or `default effort`.
    pub fn effort_label(&self) -> String {
        format!("{} effort", or(&self.settings.effort, "default"))
    }

    /// What the session has used: its context, against the model's window
    /// when known, and its cost; nothing before its first turn.
    pub fn usage(&self) -> Vec<String> {
        let mut usage = Vec::new();
        if self.context > 0 {
            let window = self.model_choice(self.model()).map_or(0, |m| m.context);
            usage.push(match window {
                0 => format!("{} tokens", count(self.context)),
                window => format!(
                    "{}/{} {}%",
                    count(self.context),
                    count(window),
                    self.context * 100 / window
                ),
            });
        }
        if self.spent > 0.0 {
            usage.push(dollars(self.spent));
        }
        usage
    }

    /// The thinking efforts of the current model; `None` if it's unknown.
    fn efforts(&self) -> Option<&[String]> {
        self.model_choice(self.model())
            .map(|m| m.variants.as_slice())
    }

    /// What the agent is up to, judging by its latest output.
    pub fn doing(&self) -> &'static str {
        let said = self.thread.iter().rev().find(|e| e.role != Role::Command);
        match said.map(|e| e.role) {
            Some(Role::Thinking) => "thinking",
            Some(Role::Agent) => "writing",
            Some(Role::Tool) => "using tools",
            _ => "waiting",
        }
    }

    /// The turn's task, once it has started.
    pub fn running_task(&self) -> Option<String> {
        match &self.activity {
            Activity::Working { task_id, .. } => task_id.clone(),
            Activity::Idle => None,
        }
    }

    /// Every task the chat started still running: the turn's, then the
    /// commands'. A turn only watched isn't among them.
    pub fn tasks(&self) -> impl Iterator<Item = String> {
        let watched = matches!(self.activity, Activity::Working { watched: true, .. });
        let turn = self.running_task().filter(|_| !watched);
        turn.into_iter().chain(self.run_tasks().cloned())
    }

    /// Follows a turn `prompt` started elsewhere on the chat's session, as
    /// if it had been sent here.
    pub(in crate::state) fn attach(&mut self, prompt: &str, task_id: String) -> Effect {
        self.push(Role::User, prompt);
        self.activity = Activity::Working {
            task_id: Some(task_id.clone()),
            since: Instant::now(),
            cancelling: false,
            agent: self.agent().to_string(),
            effort: self.settings.effort.clone(),
            watched: true,
        };
        // The history on its way has the turn's start, which the watch replays.
        if self.loading_history {
            self.attached = Some(prompt.to_string());
        }
        Effect::WatchTurn {
            chat: self.id,
            task_id,
        }
    }

    /// Whether nothing has been said or run in it yet and nothing is under
    /// way: a chat to start afresh in, or to drop once left.
    pub fn is_blank(&self) -> bool {
        let said = self
            .thread
            .iter()
            .any(|e| matches!(e.role, Role::User | Role::Command));
        self.settings.session_id.is_empty()
            && !said
            && self.ask.is_none()
            && !self.at_work()
            && self.signing_in.is_none()
            && self.preparing.is_none()
    }

    /// Whether the agent or a command is at work.
    pub fn at_work(&self) -> bool {
        self.activity.working() || self.runs().any(Run::running)
    }

    /// Whether it has been asked to stop, its turn or its commands, and
    /// hasn't yet.
    pub fn stopping(&self) -> bool {
        let turn = matches!(
            self.activity,
            Activity::Working {
                cancelling: true,
                ..
            }
        );
        turn || self.runs().any(Run::stopping)
    }

    /// Whether the entry at `at` is part of the turn the agent is still at.
    pub fn in_running_turn(&self, at: usize) -> bool {
        let asked = self.thread.iter().rposition(|e| e.role == Role::User);
        self.activity.working() && asked.is_none_or(|asked| at > asked)
    }

    /// Where the node lists the agent named `name`, which tells agents
    /// apart; 0 for one it doesn't list.
    pub fn agent_index(&self, name: &str) -> usize {
        self.options
            .as_ref()
            .and_then(|o| o.agents.iter().position(|a| a.name == name))
            .unwrap_or(0)
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

    /// What the node is getting ready for the chat, while it is: a copy of
    /// a project, a list, a resumed session's turns, what its agent offers.
    pub fn getting_ready(&self) -> Option<String> {
        let node = &self.node.name;
        Some(match &self.preparing {
            Some(repository) => format!("getting {repository} ready on {node}…"),
            None if self.listing_projects => format!("listing {node}'s projects…"),
            None if self.listing_providers.is_some() => {
                format!("listing {node}'s model providers…")
            }
            None if self.loading_history => "loading the session's earlier messages…".into(),
            None if self.options.is_none() && self.fetching_options => {
                format!("asking {node} what its agent offers…")
            }
            None if self.loading() => "connecting MCP servers; commands follow…".into(),
            None => return None,
        })
    }

    /// Sends `prompt` (or runs `command` with it), showing `text` in the
    /// thread. While the agent works the text stays in the prompt, to send
    /// once it is done.
    fn send(&mut self, text: &str, command: String, prompt: String) -> Outcome {
        if self.activity.working() {
            return Outcome::default();
        }
        if self.title.is_empty() {
            self.title = text.to_string();
        }
        self.push(Role::User, text);
        self.activity = Activity::Working {
            task_id: None,
            since: Instant::now(),
            cancelling: false,
            agent: self.agent().to_string(),
            effort: self.settings.effort.clone(),
            watched: false,
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

    /// Stops the commands still running, else cancels the turn; a task not
    /// started yet is cancelled as soon as it is.
    pub(super) fn cancel(&mut self) -> Vec<Effect> {
        match self.cancel_runs() {
            Some(stops) => stops,
            None => self.cancel_turn().into_iter().collect(),
        }
    }

    /// Stops the commands still running and cancels the turn, as for
    /// closing.
    pub(super) fn stop_all(&mut self) -> Vec<Effect> {
        let mut stops = self.cancel_runs().unwrap_or_default();
        stops.extend(self.cancel_turn());
        stops
    }

    fn cancel_turn(&mut self) -> Option<Effect> {
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

    /// Adds a remark of commandant's own to the thread.
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
            run: None,
        });
    }
}
