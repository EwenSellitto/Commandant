//! One chat: a session with a node's agent, what the person asks of it, and
//! what its task events make of it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use commandant_proto::task_event::Event as TaskEvent;
use commandant_proto::*;

use super::{ChatId, Choice, Choose, Edit, Effect, Outcome, Pick};
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

/// What a chat's picker chooses from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Menu {
    Agent,
    Model,
    Effort,
    /// One of the agent's commands or skills, to fill in.
    Command,
    /// An MCP server, to connect or disconnect.
    Mcp,
}

impl Menu {
    fn title(self) -> &'static str {
        match self {
            Self::Agent => "Agent",
            Self::Model => "Model",
            Self::Effort => "Thinking effort",
            Self::Command => "Commands and skills",
            Self::Mcp => "MCP servers (Enter connects or disconnects)",
        }
    }
}

/// Where signing in to a provider has got to.
pub enum Auth {
    /// The next line submitted is the API key, which the front end masks.
    Key { provider: String },
    /// The next line submitted is the code the provider's page showed.
    Code { provider: String, index: u32 },
    /// The node is at it; `start` is set while it starts OAuth method `start`.
    Waiting {
        provider: String,
        start: Option<u32>,
        doing: String,
    },
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

    fn open_picker(&mut self, menu: Menu, filter: &str) -> Option<Effect> {
        let Some(options) = self.options.clone() else {
            // It opens when they come.
            self.pending = Some((menu, filter.to_string()));
            if self.fetching_options {
                self.info("the node is still saying what its agent offers; this opens when it has");
                return None;
            }
            return self.ask_for_options();
        };
        let (choices, current): (Vec<_>, _) = match menu {
            Menu::Agent => {
                let choices = options
                    .agents
                    .iter()
                    .map(|a| Choice::new(Choose::Agent(a.name.clone()), &a.name, &a.description))
                    .collect();
                (choices, Some(Choose::Agent(self.agent().to_string())))
            }
            Menu::Model => {
                let default = or(&options.default_model, "the agent picks");
                let models = options.models.iter().map(|m| {
                    let detail = format!("{} · {}", m.provider, m.id);
                    Choice::new(Choose::Model(m.id.clone()), &m.name, detail)
                });
                let choices = std::iter::once(Choice::new(
                    Choose::Model(String::new()),
                    "Default",
                    default,
                ))
                .chain(models)
                .collect();
                (choices, Some(Choose::Model(self.settings.model.clone())))
            }
            Menu::Effort => {
                let efforts = self.efforts_or_explain()?;
                let default =
                    Choice::new(Choose::Effort(String::new()), "Default", "the model's own");
                let choices = std::iter::once(default)
                    .chain(
                        efforts
                            .iter()
                            .map(|e| Choice::new(Choose::Effort(e.clone()), e, "")),
                    )
                    .collect();
                (choices, Some(Choose::Effort(self.settings.effort.clone())))
            }
            Menu::Command => {
                if options.loading {
                    self.info("the agent's commands are still loading (its MCP servers are connecting); try again in a moment");
                    return None;
                }
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
                        Choice::new(
                            Choose::Command(c.name.clone()),
                            format!("/{}", c.name),
                            detail,
                        )
                    })
                    .collect();
                (choices, None)
            }
            Menu::Mcp => {
                if options.loading {
                    self.info(
                        "the agent's MCP servers are still connecting; try again in a moment",
                    );
                    return None;
                }
                if options.mcp_servers.is_empty() {
                    self.info("the agent has no MCP servers configured");
                    return None;
                }
                let choices = options
                    .mcp_servers
                    .iter()
                    .map(|m| Choice::new(Choose::Mcp(m.name.clone()), &m.name, mcp_status(m)))
                    .collect();
                (choices, None)
            }
        };
        self.pick = Some(Pick::new(menu.title(), choices, current).with_filter(filter));
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

    /// Does what was chosen from the chat's picker, which closes.
    pub(super) fn choose(&mut self, choice: Choose) -> Outcome {
        self.pick = None;
        match choice {
            Choose::Agent(agent) => self.settings.agent = agent,
            Choose::Effort(effort) => self.settings.effort = effort,
            Choose::Model(model) => {
                self.settings.model = model;
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
            // Ready for its arguments.
            Choose::Command(name) => {
                return Outcome {
                    prompt: Some((self.id, Edit::Insert(format!("/{name} ")))),
                    ..Default::default()
                };
            }
            Choose::Provider(id) => self.open_sign_in(&id),
            Choose::SignIn {
                provider,
                oauth: None,
            } => {
                let name = self.provider_name(&provider);
                self.info(&format!(
                    "paste the API key for {name} and press Enter (Esc to cancel)"
                ));
                self.auth = Some(Auth::Key { provider });
            }
            Choose::SignIn {
                provider,
                oauth: Some(index),
            } => {
                let doing = format!("starting to sign in to {}…", self.provider_name(&provider));
                let start = auth_action::Action::OauthStart(index);
                return self
                    .authenticate(provider, Some(index), doing, start)
                    .into();
            }
            Choose::SignOut(provider) => {
                let doing = format!("signing out of {}…", self.provider_name(&provider));
                let action = auth_action::Action::SignOut(true);
                return self.authenticate(provider, None, doing, action).into();
            }
            Choose::NewCopy(repository) => return self.new_copy(repository).into(),
            Choose::Join(copy) => self.work_in(&copy.path, &copy.id),
            Choose::Mcp(name) => {
                let connect = self.options.as_ref().is_some_and(|o| {
                    o.mcp_servers
                        .iter()
                        .any(|m| m.name == name && m.status != "connected")
                });
                let doing = if connect {
                    "connecting"
                } else {
                    "disconnecting"
                };
                self.info(&format!("{doing} {name}…"));
                return Effect::SwitchMcp {
                    chat: self.id,
                    node: self.node.id.clone(),
                    name,
                    connect,
                }
                .into();
            }
            // The app's own choices.
            Choose::NewSession | Choose::Chat(_) | Choose::Saved(_) | Choose::Harness { .. } => {}
        }
        Outcome::default()
    }

    /// Closes the chat's picker, or stops waiting for a secret.
    pub(super) fn dismiss(&mut self) -> Outcome {
        if self.pick.take().is_some() || !self.awaiting_secret() {
            return Outcome::default();
        }
        self.auth = None;
        self.info("not signed in");
        self.cleared(None)
    }

    /// Sends the API key or code `text` holds, if one is asked for. It is
    /// kept out of the thread.
    fn send_secret(&mut self, text: &str) -> Option<Effect> {
        let (provider, action) = match self.auth.take()? {
            Auth::Key { provider } => (provider, auth_action::Action::ApiKey(text.into())),
            Auth::Code { provider, index } => {
                let finish = OauthFinish {
                    index,
                    code: text.into(),
                };
                (provider, auth_action::Action::OauthFinish(finish))
            }
            waiting @ Auth::Waiting { .. } => {
                self.auth = Some(waiting);
                return None;
            }
        };
        let doing = format!("signing in to {}…", self.provider_name(&provider));
        Some(self.authenticate(provider, None, doing, action))
    }

    /// Has the node do a step of signing in, and waits for it.
    fn authenticate(
        &mut self,
        provider: String,
        start: Option<u32>,
        doing: String,
        action: auth_action::Action,
    ) -> Effect {
        self.auth = Some(Auth::Waiting {
            provider: provider.clone(),
            start,
            doing,
        });
        Effect::Authenticate {
            chat: self.id,
            node: self.node.id.clone(),
            provider,
            action: AuthAction {
                action: Some(action),
            },
        }
    }

    fn provider_name(&self, id: &str) -> String {
        let provider = self.providers.iter().find(|p| p.id == id);
        provider.map_or_else(|| id.to_string(), |p| p.name.clone())
    }

    /// `/project`: browse the node's projects, or with a repository (or a
    /// project's name), clone a new copy of it to work in. A session stays in
    /// its directory, so only one that hasn't started can move.
    fn project(&mut self, repository: &str) -> Option<Effect> {
        if !self.settings.session_id.is_empty() || matches!(self.activity, Activity::Working { .. })
        {
            self.info(
                "this session already works somewhere: start a new one (Ctrl-N) for a project",
            );
            return None;
        }
        if !repository.is_empty() {
            return Some(self.new_copy(repository.to_string()));
        }
        if self.listing_projects {
            return None;
        }
        self.listing_projects = true;
        Some(Effect::FetchProjects {
            chat: self.id,
            node: self.node.id.clone(),
        })
    }

    fn new_copy(&mut self, repository: String) -> Effect {
        self.preparing = Some(repository.clone());
        Effect::PrepareProject {
            chat: self.id,
            node: self.node.id.clone(),
            repository,
        }
    }

    fn work_in(&mut self, path: &str, id: &str) {
        self.info(&format!("working in copy {id}: {path}"));
        self.settings.cwd = path.to_string();
    }

    /// The node's projects to browse: a new copy of each, then its copies
    /// with what their sessions are about.
    fn open_projects(&mut self, projects: Vec<commandant_proto::Project>) {
        if projects.is_empty() {
            self.info("the node has no projects yet: /project <repository URL> clones one");
            return;
        }
        let mut choices = Vec::new();
        for project in projects {
            let label = format!("+ new copy of {}", project.name);
            let new = Choose::NewCopy(project.name.clone());
            choices.push(Choice::new(new, label, &project.repository));
            for copy in project.copies {
                let label = format!("{} · {}", project.name, copy.id);
                let detail = copy_summary(&copy);
                choices.push(Choice::new(Choose::Join(copy), label, detail));
            }
        }
        self.pick = Some(Pick::new(
            "Projects (join a copy, or make a new one)",
            choices,
            None,
        ));
    }

    /// Asks the node for its providers, to pick one when they come.
    fn list_providers(&mut self, filter: &str) -> Option<Effect> {
        if self.listing_providers.is_some() {
            return None;
        }
        self.listing_providers = Some(filter.to_string());
        Some(Effect::FetchProviders {
            chat: self.id,
            node: self.node.id.clone(),
        })
    }

    fn open_providers(&mut self, filter: &str) {
        let choices = self
            .providers
            .iter()
            .map(|p| {
                let detail = match p.connected {
                    true => format!("signed in · {}", p.id),
                    false => p.id.clone(),
                };
                Choice::new(Choose::Provider(p.id.clone()), &p.name, detail)
            })
            .collect();
        let pick = Pick::new("Providers (Enter signs in or out)", choices, None);
        self.pick = Some(pick.with_filter(filter));
    }

    /// The ways to sign in to `provider`, and out if it is signed in.
    fn open_sign_in(&mut self, id: &str) {
        let Some(provider) = self.providers.iter().find(|p| p.id == id) else {
            return;
        };
        let mut choices: Vec<_> = provider
            .methods
            .iter()
            .map(|m| {
                let choose = Choose::SignIn {
                    provider: id.to_string(),
                    oauth: m.oauth.then_some(m.index),
                };
                let detail = if m.oauth { "OAuth" } else { "paste a key" };
                Choice::new(choose, &m.label, detail)
            })
            .collect();
        if provider.connected {
            let detail = format!("forget {}'s credentials on this node", provider.name);
            choices.push(Choice::new(
                Choose::SignOut(id.to_string()),
                "Sign out",
                detail,
            ));
        }
        self.pick = Some(Pick::new("Sign in with", choices, None));
    }

    /// Takes in how a step of signing in went, which may call for the next.
    fn signed_in(&mut self, result: Result<ProviderAuthResult, String>) -> Option<Effect> {
        // Esc'd meanwhile, or not this chat's.
        let Some(Auth::Waiting {
            provider, start, ..
        }) = self.auth.take()
        else {
            return None;
        };
        let name = self.provider_name(&provider);
        let result = match result {
            Ok(result) => result,
            Err(e) => {
                self.error(&format!("couldn't sign in to {name}: {e}"));
                return None;
            }
        };
        let Some(index) = start else {
            self.info(&format!("{name}: done; its models follow"));
            return Some(Effect::fetch_options(&self.node.id));
        };
        // OAuth has started: say where to go.
        if !result.instructions.is_empty() {
            self.info(&result.instructions);
        }
        self.info(&format!("open {}", result.url));
        if result.needs_code {
            self.info("then paste the code it shows here");
            self.auth = Some(Auth::Code { provider, index });
            return None;
        }
        // A redirect back to the node's loopback only works in a browser there.
        if result.url.contains("localhost") || result.url.contains("127.0.0.1") {
            self.info(&format!(
                "that page sends the browser back to {}: sign in from a browser there, or pick a headless method",
                self.node.name
            ));
        }
        let finish = OauthFinish {
            index,
            code: String::new(),
        };
        let doing = format!("waiting for you to sign in to {name}…");
        Some(self.authenticate(
            provider,
            None,
            doing,
            auth_action::Action::OauthFinish(finish),
        ))
    }

    /// How switching an MCP server went, which this chat asked for.
    pub fn mcp_switched(&mut self, name: &str, switched: &Result<Arc<AgentOptions>, String>) {
        match switched {
            Ok(options) => {
                if let Some(server) = options.mcp_servers.iter().find(|m| m.name == name) {
                    self.info(&format!("{name}: {}", mcp_status(server)));
                }
            }
            Err(e) => self.error(&format!("couldn't switch {name}: {e}")),
        }
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

/// `main · fix the parser; add tests +1`: a copy's branch and what its
/// sessions are about.
fn copy_summary(copy: &ProjectCopy) -> String {
    let sessions = match copy.sessions.as_slice() {
        [] => "no sessions yet".to_string(),
        [one] => one.clone(),
        [one, two] => format!("{one}; {two}"),
        [one, two, rest @ ..] => format!("{one}; {two} +{}", rest.len()),
    };
    match copy.branch.as_str() {
        "" => sessions,
        branch => format!("{branch} · {sessions}"),
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

pub use commandant_common::or;

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
        Chat::new(1, node, Settings::default(), None)
    }

    pub(crate) fn output(stream: OutputStream, data: &[u8]) -> Message {
        Message::Task(TaskEvent::Output(TaskOutput {
            stream: stream as i32,
            data: data.to_vec(),
            ..Default::default()
        }))
    }

    fn started(task_id: &str) -> Message {
        Message::Task(TaskEvent::Started(TaskStarted {
            task_id: task_id.into(),
            ..Default::default()
        }))
    }

    /// What submitting `text` comes to.
    fn command(chat: &mut Chat, text: &str) -> Outcome {
        let (outcome, app) = chat.submit(text);
        assert_eq!(app, None, "{text:?} is the chat's");
        outcome
    }

    /// The one effect an outcome has, if any.
    fn effect(outcome: Outcome) -> Option<Effect> {
        let mut effects = outcome.effects.into_iter();
        let effect = effects.next();
        assert!(effects.next().is_none());
        effect
    }

    /// Sends `text` and returns the request.
    fn send(chat: &mut Chat, text: &str) -> PromptRequest {
        let outcome = command(chat, text);
        assert_eq!(outcome.prompt, Some((chat.id, Edit::Clear)));
        match effect(outcome) {
            Some(Effect::Send(_, request)) => request,
            _ => panic!("{text:?} should be sent"),
        }
    }

    fn choose(chat: &mut Chat, choice: Choose) -> Option<Effect> {
        effect(chat.choose(choice))
    }

    #[test]
    fn a_turn_streams_into_the_thread_and_keeps_the_session() {
        let mut app = chat();
        let request = send(&mut app, "hi");
        assert_eq!(
            (request.prompt.as_str(), request.node.as_str()),
            ("hi", "n1")
        );

        // Sending while the agent works leaves the text in the prompt.
        let kept = command(&mut app, "next");
        assert!(kept.effects.is_empty() && kept.prompt.is_none());

        app.on_message(started("t1"));
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
        assert_eq!(app.unseen, Some(Unseen::Done));

        // The next prompt continues the session.
        assert_eq!(send(&mut app, "next").session_id, "ses_1");
        assert_eq!(app.sent(), ["hi", "next"], "sent once each");
    }

    #[test]
    fn cancelling_cancels_the_task_once() {
        let mut app = chat();
        send(&mut app, "hi");
        assert!(app.on_message(started("t1")).is_none());
        assert!(matches!(app.cancel(), Some(Effect::Cancel(id)) if id == "t1"));
        assert!(app.cancel().is_none());
    }

    #[test]
    fn cancelling_before_the_task_starts_cancels_it_when_it_does() {
        let mut app = chat();
        send(&mut app, "hi");
        assert!(app.cancel().is_none());
        assert!(matches!(
            app.activity,
            Activity::Working {
                cancelling: true,
                ..
            }
        ));
        assert!(matches!(
            app.on_message(started("t1")),
            Some(Effect::Cancel(id)) if id == "t1"
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
        app.on_message(Message::Options(Ok(Arc::new(AgentOptions {
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
        }))));
        app
    }

    #[test]
    fn the_agents_commands_run_with_their_arguments() {
        let mut app = with_options();
        let request = send(&mut app, "/review  the parser ");
        assert_eq!(
            (request.command.as_str(), request.prompt.as_str()),
            ("review", "the parser")
        );
        assert_eq!(app.thread.last().unwrap().text, "/review  the parser");
        app.on_message(Message::Failed("stop".into()));

        // Any other slash is just text.
        let request = send(&mut app, "/etc/hosts?");
        assert_eq!(
            (request.command.as_str(), request.prompt.as_str()),
            ("", "/etc/hosts?")
        );
        app.on_message(Message::Failed("stop".into()));

        // Picking one readies it for its arguments.
        command(&mut app, "/skills");
        let pick = app.pick.as_ref().unwrap();
        assert_eq!(pick.choices.len(), 1);
        let picked = app.choose(pick.choices[0].value.clone());
        assert_eq!(
            picked.prompt,
            Some((app.id, Edit::Insert("/review ".into())))
        );
        assert!(app.pick.is_none());
    }

    #[test]
    fn mcp_servers_are_switched_from_a_picker() {
        let mut app = with_options();
        command(&mut app, "/mcp");
        assert_eq!(app.pick.as_ref().unwrap().choices.len(), 1);
        let Some(Effect::SwitchMcp {
            chat,
            name,
            connect,
            ..
        }) = choose(&mut app, Choose::Mcp("docs".into()))
        else {
            panic!("choosing a server switches it");
        };
        assert_eq!((chat, name.as_str(), connect), (app.id, "docs", true));

        let mut options = (**app.options.as_ref().unwrap()).clone();
        options.mcp_servers[0].status = "connected".into();
        let switched = Ok(Arc::new(options));
        app.on_message(Message::Options(switched.clone()));
        app.mcp_switched("docs", &switched);
        assert_eq!(app.thread.last().unwrap().text, "docs: connected");
        app.mcp_switched("docs", &Err("timed out".into()));
        assert_eq!(
            app.thread.last().unwrap().text,
            "couldn't switch docs: timed out"
        );

        assert!(matches!(
            choose(&mut app, Choose::Mcp("docs".into())),
            Some(Effect::SwitchMcp { connect: false, .. })
        ));
    }

    #[test]
    fn agents_cycle_once_the_node_has_said_which() {
        // A new chat is still asking.
        let mut app = chat();
        assert!(app.cycle_agent(1).is_none());
        assert_eq!(app.thread.last().unwrap().role, Role::Info);

        let mut app = with_options();
        assert_eq!(app.agent(), "build");
        app.cycle_agent(1);
        assert_eq!(app.agent(), "plan");
        app.cycle_agent(-1);
        app.cycle_agent(-1);
        assert_eq!(app.agent(), "review");

        // A failed fetch can be retried.
        let mut failed = chat();
        failed.on_message(Message::Options(Err("offline".into())));
        assert!(matches!(
            failed.cycle_agent(1),
            Some(Effect::FetchOptions { .. })
        ));
    }

    #[test]
    fn model_and_effort_are_picked() {
        let mut app = with_options();
        // The default model is unknown until a reply names it.
        command(&mut app, "/effort");
        assert!(app.pick.is_none());

        let outcome = command(&mut app, "/model smart");
        assert_eq!(outcome.prompt, Some((app.id, Edit::Clear)));
        let pick = app.pick.as_ref().expect("/model opens a picker");
        assert_eq!((pick.title, pick.filter.as_str()), ("Model", "smart"));
        assert_eq!(pick.current, Some(Choose::Model(String::new())));
        choose(&mut app, Choose::Model("a/smart".into()));
        assert!(app.pick.is_none());
        assert_eq!(app.settings.model, "a/smart");

        command(&mut app, "/effort");
        let efforts: Vec<_> = app
            .pick
            .as_ref()
            .unwrap()
            .choices
            .iter()
            .map(|c| c.label.as_str())
            .collect();
        assert_eq!(efforts, ["Default", "low", "high"]);
        choose(&mut app, Choose::Effort("high".into()));
        assert_eq!(app.settings.effort, "high");
        // Cycling wraps round to the model's default.
        app.cycle_effort();
        assert_eq!(app.settings.effort, "");
        app.cycle_effort();
        assert_eq!(app.settings.effort, "low");

        // A model without that effort drops it.
        choose(&mut app, Choose::Model("a/fast".into()));
        assert_eq!(app.settings.model, "a/fast");
        assert_eq!(app.settings.effort, "");

        let request = send(&mut app, "hi");
        assert_eq!(request.model, "a/fast");
        assert_eq!(request.agent, "");
    }

    #[test]
    fn odd_input_is_handled() {
        let mut app = chat();
        // Blank input, and a lone slash, send nothing useful.
        let blank = command(&mut app, "   ");
        assert!(blank.effects.is_empty() && blank.prompt.is_none());
        assert!(app.sent().is_empty());
        let request = send(&mut app, "/");
        assert_eq!(
            (request.prompt.as_str(), request.command.as_str()),
            ("/", "")
        );
        app.on_message(Message::Failed("stop".into()));

        // Before the node has listed its commands, a command is just text.
        let request = send(&mut app, "/review x");
        assert_eq!(
            (request.prompt.as_str(), request.command.as_str()),
            ("/review x", "")
        );
    }

    #[test]
    fn the_apps_commands_go_to_the_app() {
        let mut app = chat();
        for (text, expected) in [
            ("/quit", AppCommand::Quit),
            ("/exit", AppCommand::Quit),
            ("/new", AppCommand::New),
            ("/sessions", AppCommand::Sessions),
            ("/nodes", AppCommand::Nodes),
            ("/close", AppCommand::Close),
        ] {
            let (outcome, command) = app.submit(text);
            assert_eq!(command, Some(expected), "{text}");
            // Quitting leaves the prompt as it is.
            let cleared = expected != AppCommand::Quit;
            assert_eq!(outcome.prompt.is_some(), cleared, "{text}");
        }
    }

    #[test]
    fn the_title_is_the_first_prompt_sent() {
        let mut app = with_options();
        command(&mut app, "/model");
        app.dismiss();
        assert!(app.pick.is_none());
        assert_eq!(app.title, "", "commands aren't prompts");
        send(&mut app, "first");
        // Sent while it works: kept, not sent, not the title.
        assert!(command(&mut app, "second").effects.is_empty());
        app.on_message(Message::Failed("stop".into()));
        send(&mut app, "third");
        assert_eq!(app.title, "first");
    }

    #[test]
    fn a_failed_stream_ends_the_turn_and_marks_it() {
        let mut app = chat();
        send(&mut app, "hi");
        app.on_message(started("t1"));
        app.on_message(output(OutputStream::Stdout, b"partial \xc3"));
        app.on_message(Message::Failed("the stream ended".into()));
        assert!(matches!(app.activity, Activity::Idle));
        assert_eq!(app.unseen, Some(Unseen::Failed));
        assert!(app.running_task().is_none());
        // Cancelling after the turn ended does nothing.
        assert!(app.cancel().is_none());
        // The half character is dropped, not glued to the next reply.
        send(&mut app, "again");
        app.on_message(output(OutputStream::Stdout, b"ok"));
        assert_eq!(app.thread.last().unwrap().text, "ok");
    }

    #[test]
    fn a_picker_asked_for_too_early_opens_when_the_options_come() {
        let mut app = chat();
        // The node hasn't said yet: the request is remembered, not dropped.
        assert!(effect(command(&mut app, "/model smart")).is_none());
        assert!(app.pick.is_none());
        assert!(
            effect(command(&mut app, "/model smart")).is_none(),
            "still the one request"
        );
        let options = AgentOptions {
            models: vec![ModelChoice {
                id: "a/smart".into(),
                name: "Smart".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        app.on_message(Message::Options(Ok(Arc::new(options))));
        let pick = app.pick.as_ref().expect("it opens by itself");
        assert_eq!((pick.title, pick.filter.as_str()), ("Model", "smart"));

        // If the node can't say, the wait ends with why.
        let mut failed = chat();
        command(&mut failed, "/agent");
        failed.on_message(Message::Options(Err("timed out".into())));
        assert!(failed.pick.is_none());
        let said = &failed.thread.last().unwrap().text;
        assert!(
            said.contains("couldn't list the agent's options: timed out"),
            "{said}"
        );
    }

    fn listed(app: &mut Chat) {
        assert!(matches!(
            effect(command(app, "/providers")),
            Some(Effect::FetchProviders { .. })
        ));
        let method = |label: &str, oauth: bool, index: u32| AuthMethod {
            label: label.into(),
            oauth,
            index,
        };
        let providers = vec![ModelProvider {
            id: "acme".into(),
            name: "Acme".into(),
            connected: true,
            methods: vec![
                method("Browser", true, 0),
                method("Code", true, 1),
                method("API key", false, 2),
            ],
        }];
        assert!(app.on_message(Message::Providers(Ok(providers))).is_none());
        assert!(app.pick.is_some(), "the providers come in a picker");
        choose(app, Choose::Provider("acme".into()));
        // Browser, Code, API key, Sign out.
        assert_eq!(app.pick.as_ref().unwrap().choices.len(), 4);
    }

    fn sent(effect: Option<Effect>) -> (String, auth_action::Action) {
        match effect {
            Some(Effect::Authenticate {
                provider, action, ..
            }) => (provider, action.action.unwrap()),
            _ => panic!("a sign-in step is sent"),
        }
    }

    fn sign_in(provider: &str, oauth: Option<u32>) -> Choose {
        Choose::SignIn {
            provider: provider.into(),
            oauth,
        }
    }

    #[test]
    fn an_api_key_is_sent_but_never_shown() {
        let mut app = with_options();
        listed(&mut app);
        assert!(choose(&mut app, sign_in("acme", None)).is_none());
        assert!(app.awaiting_secret());
        assert!(app.completions("/").is_empty(), "a key isn't a command");
        let typed = command(&mut app, "sk-secret");
        assert_eq!(typed.prompt, Some((app.id, Edit::Clear)));
        let (provider, action) = sent(effect(typed));
        assert_eq!(provider, "acme");
        assert_eq!(action, auth_action::Action::ApiKey("sk-secret".into()));
        assert!(app.thread.iter().all(|e| !e.text.contains("sk-secret")));
        assert!(app.sent().iter().all(|s| !s.contains("sk-secret")));

        // Done: the node's models are asked for again.
        let done = app.on_message(Message::Auth(Ok(ProviderAuthResult::default())));
        assert!(matches!(done, Some(Effect::FetchOptions { .. })));
        assert!(app.auth.is_none());

        // Dismissing backs out of typing a key.
        listed(&mut app);
        choose(&mut app, sign_in("acme", None));
        let dismissed = app.dismiss();
        assert_eq!(dismissed.prompt, Some((app.id, Edit::Clear)));
        assert!(app.auth.is_none());
        assert_eq!(app.thread.last().unwrap().text, "not signed in");
    }

    #[test]
    fn oauth_waits_in_the_browser_or_takes_the_code() {
        let mut app = with_options();
        listed(&mut app);
        let (_, start) = sent(choose(&mut app, sign_in("acme", Some(0))));
        assert_eq!(start, auth_action::Action::OauthStart(0));
        let started = ProviderAuthResult {
            url: "https://acme.test/authorize?redirect_uri=http://localhost:1455".into(),
            ..Default::default()
        };
        // It finishes by itself, once the user is done in the browser.
        let (_, finish) = sent(app.on_message(Message::Auth(Ok(started))));
        assert!(matches!(finish, auth_action::Action::OauthFinish(f) if f.code.is_empty()));
        assert!(app.thread.iter().any(|e| e.text.contains("acme.test")));
        assert!(app.thread.iter().any(|e| e.text.contains("headless")));
        app.on_message(Message::Auth(Err("timed out".into())));
        assert!(
            app.thread
                .last()
                .unwrap()
                .text
                .contains("couldn't sign in to Acme: timed out")
        );

        listed(&mut app);
        sent(choose(&mut app, sign_in("acme", Some(1))));
        let started = ProviderAuthResult {
            url: "https://acme.test/code".into(),
            needs_code: true,
            ..Default::default()
        };
        assert!(app.on_message(Message::Auth(Ok(started))).is_none());
        let (_, finish) = sent(effect(command(&mut app, "abc")));
        assert!(
            matches!(finish, auth_action::Action::OauthFinish(f) if f.index == 1 && f.code == "abc")
        );
    }

    #[test]
    fn slash_commands_complete_by_name() {
        let app = with_options();
        let names = |text: &str| -> Vec<String> {
            app.completions(text).into_iter().map(|(n, _)| n).collect()
        };
        assert_eq!(names("/re"), ["review"], "the agent's own commands too");
        assert_eq!(names("/s"), ["skills", "sessions"]);
        assert!(
            names("/review ").is_empty(),
            "nothing to complete past the name"
        );
        assert!(names("/zzz").is_empty());
        assert!(names("re").is_empty());

        assert_eq!(app.command_len("/model smart"), 6);
        assert_eq!(app.command_len("/review it"), 7);
        assert_eq!(app.command_len("/nope x"), 0);
    }

    #[test]
    fn projects_are_browsed_joined_or_copied() {
        let mut app = with_options();
        assert!(matches!(
            effect(command(&mut app, "/project")),
            Some(Effect::FetchProjects { .. })
        ));
        assert!(
            effect(command(&mut app, "/project")).is_none(),
            "one listing at a time"
        );
        let copy = |id: &str, sessions: &[&str]| ProjectCopy {
            id: id.into(),
            path: format!("/state/projects/app/{id}"),
            branch: "main".into(),
            sessions: sessions.iter().map(|s| s.to_string()).collect(),
        };
        let projects = vec![commandant_proto::Project {
            name: "app".into(),
            repository: "https://example.com/me/app.git".into(),
            copies: vec![
                copy("3fa9c1d2", &["fix the parser", "add tests", "docs"]),
                copy("77aa00bb", &[]),
            ],
        }];
        app.on_message(Message::Projects(Ok(projects)));
        let pick = app.pick.as_ref().expect("the projects to browse");
        let shown: Vec<_> = pick
            .choices
            .iter()
            .map(|c| (c.label.clone(), c.detail.clone()))
            .collect();
        assert_eq!(shown[0].0, "+ new copy of app");
        assert_eq!(
            shown[1],
            (
                "app · 3fa9c1d2".into(),
                "main · fix the parser; add tests +1".into()
            )
        );
        assert_eq!(shown[2].1, "main · no sessions yet");

        // Joining a copy works in it at once.
        let join = pick.choices[1].value.clone();
        assert!(choose(&mut app, join).is_none());
        assert_eq!(app.settings.cwd, "/state/projects/app/3fa9c1d2");

        // A new copy is cloned first, by name or by URL.
        let Some(Effect::PrepareProject { repository, .. }) =
            effect(command(&mut app, "/project app"))
        else {
            panic!("a new copy is asked for");
        };
        assert_eq!(repository, "app");
        assert!(app.preparing.is_some());
        let ready = ProjectReady {
            id: "c0ffee00".into(),
            path: "/state/projects/app/c0ffee00".into(),
            ..Default::default()
        };
        app.on_message(Message::Project(Ok(ready)));
        assert!(app.preparing.is_none());
        assert_eq!(send(&mut app, "hi").cwd, "/state/projects/app/c0ffee00");

        // Once the session has started, a new one is needed to move.
        app.on_message(Message::Task(TaskEvent::Finished(TaskFinished {
            exit_code: Some(0),
            session_id: "ses_1".into(),
            ..Default::default()
        })));
        assert!(effect(command(&mut app, "/project")).is_none());
        assert!(app.thread.last().unwrap().text.contains("Ctrl-N"));
        let mut empty = with_options();
        command(&mut empty, "/project");
        empty.on_message(Message::Projects(Ok(Vec::new())));
        assert!(empty.pick.is_none());
        assert!(
            empty
                .thread
                .last()
                .unwrap()
                .text
                .contains("no projects yet")
        );
    }

    #[test]
    fn help_lists_every_command_and_key() {
        let mut app = with_options();
        let outcome = command(&mut app, "/help");
        assert!(outcome.effects.is_empty());
        assert_eq!(outcome.prompt, Some((app.id, Edit::Clear)));
        let help = &app.thread.last().unwrap().text;
        for needle in ["/providers", "/review", "Ctrl-T", "/quit"] {
            assert!(help.contains(needle), "{needle} missing from {help}");
        }
    }
}
