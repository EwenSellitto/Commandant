//! What a client knows and does, apart from how it is shown: the nodes, and
//! every chat open on them. Chats on any node run side by side.
//!
//! A front end sends [`Intent`]s and the background results it gets back as
//! [`Update`]s; both answer with an [`Outcome`]: the calls to make, maybe
//! where to go next, and what to do to a chat's prompt. Which screen is
//! shown, typing and scrolling stay with the front end.

pub mod chat;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use commandant_common::time::ago;
use commandant_proto::*;

use self::chat::{Activity, AppCommand, Chat, Message, Settings};

/// Tells chats apart for as long as the client runs.
pub type ChatId = u64;

/// How long to wait before asking again for commands that were loading.
const OPTIONS_RETRY: Duration = Duration::from_secs(3);
const SESSIONS: &str = "Sessions on this node";
/// How often a node that may answer in a moment is asked again for its
/// options before the failure is shown.
const OPTIONS_RETRIES: u32 = 2;

/// What the person asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    /// Show one of a node's chats: `chat`, the one the front end last
    /// showed, if it is still open, else its newest, else a new one.
    Open {
        node: String,
        chat: Option<ChatId>,
    },
    /// Another chat on the same node, with the same settings.
    NewChat(ChatId),
    /// Pick one of the chat's node's chats or saved sessions.
    Sessions(ChatId),
    /// Close a chat, unless its agent is still at work.
    Close(ChatId),
    /// A line typed in a chat: a prompt, a `/command`, or a secret asked for.
    Submit(ChatId, String),
    Choose(Scope, Choose),
    /// Close the picker, or stop waiting for a secret.
    Dismiss(Scope),
    /// Cancel the chat's turn.
    Cancel(ChatId),
    /// The next (or previous) agent.
    CycleAgent(ChatId, isize),
    /// The next thinking effort.
    CycleEffort(ChatId),
    /// The chat has been looked at, so how its last turn ended is no news.
    Seen(ChatId),
}

/// Where a picker, or an intent about one, belongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    App,
    Chat(ChatId),
}

/// Where the front end may go next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Go {
    Chat(ChatId),
    Nodes,
    Quit,
    /// The node hosts an agent now: worth opening, if it is the one in view.
    Ready(String),
}

/// What a chat's prompt becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// The line was taken.
    Clear,
    /// Typed at the cursor.
    Insert(String),
}

/// What an intent or an update comes to, beyond what the state keeps.
#[derive(Default)]
pub struct Outcome {
    pub effects: Vec<Effect>,
    pub go: Option<Go>,
    pub prompt: Option<(ChatId, Edit)>,
}

impl Outcome {
    fn go(go: Go) -> Self {
        Self {
            go: Some(go),
            ..Default::default()
        }
    }

    /// This, then `next`.
    fn and(mut self, next: Outcome) -> Self {
        self.effects.extend(next.effects);
        self.go = next.go.or(self.go);
        self.prompt = next.prompt.or(self.prompt);
        self
    }
}

impl From<Effect> for Outcome {
    fn from(effect: Effect) -> Self {
        Some(effect).into()
    }
}

impl From<Option<Effect>> for Outcome {
    fn from(effect: Option<Effect>) -> Self {
        Self {
            effects: effect.into_iter().collect(),
            ..Default::default()
        }
    }
}

/// A call to make; its answer comes back as an [`Update`].
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Send(ChatId, PromptRequest),
    Cancel(String),
    /// Ask a node what its agent offers, after a pause.
    FetchOptions {
        node: String,
        after: Duration,
    },
    /// Ask a node which sessions its agent has saved.
    FetchSessions(String),
    /// Have a node clone a repository (or reuse its clone) for a chat.
    PrepareProject {
        chat: ChatId,
        node: String,
        repository: String,
    },
    /// Ask a node which projects it has cloned, for a chat to browse.
    FetchProjects {
        chat: ChatId,
        node: String,
    },
    /// Ask a node's agent which model providers it knows, for a chat.
    FetchProviders {
        chat: ChatId,
        node: String,
    },
    /// Have a node's agent do a step of signing in to a provider, for a chat.
    Authenticate {
        chat: ChatId,
        node: String,
        provider: String,
        action: AuthAction,
    },
    /// Ask a node what was said in a session, for a chat resuming it.
    FetchHistory {
        chat: ChatId,
        node: String,
        session_id: String,
    },
    /// Connect (or disconnect) one of a node's MCP servers, for a chat.
    SwitchMcp {
        chat: ChatId,
        node: String,
        name: String,
        connect: bool,
    },
    /// Have a node with no agent start one.
    StartHarness {
        node: String,
        harness: String,
    },
}

impl Effect {
    pub fn fetch_options(node: &str) -> Self {
        Self::FetchOptions {
            node: node.to_string(),
            after: Duration::ZERO,
        }
    }
}

/// What background calls report.
pub enum Update {
    Chat(ChatId, Message),
    /// For every chat on the node.
    Options(String, Result<AgentOptions, Failure>),
    /// How the chat's MCP switch went: the node's options after it.
    McpSwitched {
        chat: ChatId,
        node: String,
        name: String,
        options: Result<AgentOptions, String>,
    },
    Nodes(Vec<NodeInfo>),
    Sessions(String, Result<Vec<AgentSession>, String>),
    /// The node, once it hosts the harness it was asked to start.
    HarnessStarted(String, Result<NodeInfo, String>),
}

/// Why a node couldn't say what its agent offers.
#[derive(Debug, Clone)]
pub struct Failure {
    pub message: String,
    /// It may answer in a moment: it timed out, or was out of reach.
    pub transient: bool,
}

impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        Self {
            message: message.to_string(),
            transient: false,
        }
    }
}

/// What choosing from a picker means.
#[derive(Debug, Clone, PartialEq)]
pub enum Choose {
    // A chat's: the setting it becomes (empty for the default), the command
    // to fill in, or the MCP server to switch.
    Agent(String),
    Model(String),
    Effort(String),
    Command(String),
    Mcp(String),
    /// A model provider, to sign in to or out of.
    Provider(String),
    /// Sign in to a provider: with an API key, or its OAuth method `oauth`.
    SignIn {
        provider: String,
        oauth: Option<u32>,
    },
    SignOut(String),
    /// Clone a new copy of a project (a repository, or its name) to work in.
    NewCopy(String),
    /// Work in an existing copy of a project, alongside its other sessions.
    Join(ProjectCopy),
    // The app's.
    NewSession,
    Chat(ChatId),
    Saved(AgentSession),
    Harness {
        node: String,
        harness: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub value: Choose,
    pub label: String,
    /// Said after the label.
    pub detail: String,
}

impl Choice {
    pub fn new(value: Choose, label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            value,
            label: label.into(),
            detail: detail.into(),
        }
    }
}

/// Something to choose from, for the front end to offer.
#[derive(Debug, Clone, PartialEq)]
pub struct Pick {
    /// Tells it from the pickers before it.
    pub id: u64,
    /// Changes whenever its choices do, while it stays open.
    pub revision: u64,
    pub title: &'static str,
    pub choices: Vec<Choice>,
    /// The choice in effect, to start on.
    pub current: Option<Choose>,
    /// What to narrow the choices down by, to start with.
    pub filter: String,
    /// More choices are on their way.
    pub loading: bool,
}

impl Pick {
    pub fn new(title: &'static str, choices: Vec<Choice>, current: Option<Choose>) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        Self {
            id,
            revision: id,
            title,
            choices,
            current,
            filter: String::new(),
            loading: false,
        }
    }

    pub fn with_filter(mut self, filter: &str) -> Self {
        self.filter = filter.to_string();
        self
    }

    /// The same picker, open, with new choices.
    fn revise(self, previous: &Pick) -> Self {
        Self {
            id: previous.id,
            ..self
        }
    }
}

#[derive(Default)]
pub struct State {
    pub nodes: Vec<NodeInfo>,
    /// Every open chat, on every node, oldest first.
    pub chats: Vec<Chat>,
    /// What a node's agent offers, once it has said; shared by its chats.
    options: HashMap<String, Arc<AgentOptions>>,
    /// Nodes asked for their options and yet to answer: one question each.
    fetching: HashSet<String>,
    /// How often each node has been asked again after failing to answer.
    retries: HashMap<String, u32>,
    /// The sessions each node has saved, as last listed.
    pub saved: HashMap<String, Vec<AgentSession>>,
    /// The session or harness picker, when open; a chat's own pickers live
    /// in the chat.
    pick: Option<Pick>,
    /// The chat the session picker was opened from.
    sessions_of: Option<ChatId>,
    /// A remark about the nodes.
    pub notice: String,
    /// Nodes starting a harness, which can take minutes.
    pub starting: HashSet<String>,
    /// What new chats start with; `--session` only goes to the first.
    defaults: Settings,
    next_id: ChatId,
}

impl State {
    pub fn new(nodes: Vec<NodeInfo>, defaults: Settings) -> Self {
        Self {
            nodes,
            defaults,
            next_id: 1,
            ..Default::default()
        }
    }

    pub fn chat(&self, id: ChatId) -> Option<&Chat> {
        self.chats.iter().find(|c| c.id == id)
    }

    fn chat_mut(&mut self, id: ChatId) -> Option<&mut Chat> {
        self.chats.iter_mut().find(|c| c.id == id)
    }

    /// The chats on a node, oldest first.
    pub fn chats_on<'a>(&'a self, node_id: &'a str) -> impl Iterator<Item = &'a Chat> {
        self.chats.iter().filter(move |c| c.node.id == node_id)
    }

    /// What there is to choose from in `scope`, if anything.
    pub fn pick(&self, scope: Scope) -> Option<&Pick> {
        match scope {
            Scope::App => self.pick.as_ref(),
            Scope::Chat(id) => self.chat(id)?.pick.as_ref(),
        }
    }

    /// Whether anything shown moves on its own: a spinner for a working
    /// chat, something loading, or a node starting its agent.
    pub fn busy(&self) -> bool {
        !self.starting.is_empty()
            || self.pick.as_ref().is_some_and(|p| p.loading)
            || self.chats.iter().any(|c| {
                matches!(c.activity, Activity::Working { .. })
                    || c.fetching_options
                    || c.loading_history
                    || c.loading()
                    || c.listing_providers.is_some()
                    || c.preparing.is_some()
                    || c.listing_projects
                    || matches!(c.auth, Some(chat::Auth::Waiting { .. }))
            })
    }

    /// Tasks still running, for quitting.
    pub fn running_tasks(&self) -> Vec<String> {
        self.chats.iter().filter_map(Chat::running_task).collect()
    }

    /// Opens a chat on `node` with `settings`: the first shown on start.
    pub fn open_with(&mut self, node: NodeInfo, settings: Settings) -> Outcome {
        let fetch = Outcome::from(Effect::FetchSessions(node.id.clone()));
        fetch.and(self.new_chat(node, settings))
    }

    pub fn intent(&mut self, intent: Intent) -> Outcome {
        match intent {
            Intent::Open { node, chat } => self.open(&node, chat),
            Intent::NewChat(id) => self.another_chat(id),
            Intent::Sessions(id) => {
                let Some(chat) = self.chat(id) else {
                    return Outcome::default();
                };
                let node = chat.node.id.clone();
                self.open_session_picker(id, &node, true);
                Effect::FetchSessions(node).into()
            }
            Intent::Close(id) => self.close(id),
            Intent::Submit(id, text) => {
                let Some(chat) = self.chat_mut(id) else {
                    return Outcome::default();
                };
                let (said, command) = chat.submit(&text);
                let said = self.ask_once(said);
                let then = match command {
                    None => Outcome::default(),
                    Some(AppCommand::Quit) => Outcome::go(Go::Quit),
                    Some(AppCommand::New) => self.another_chat(id),
                    Some(AppCommand::Sessions) => self.intent(Intent::Sessions(id)),
                    Some(AppCommand::Nodes) => Outcome::go(Go::Nodes),
                    Some(AppCommand::Close) => self.close(id),
                };
                said.and(then)
            }
            Intent::Choose(Scope::App, choice) => self.choose(choice),
            Intent::Choose(Scope::Chat(id), choice) => {
                let chosen = self.chat_mut(id).map(|c| c.choose(choice));
                self.ask_once(chosen.unwrap_or_default())
            }
            Intent::Dismiss(Scope::App) => {
                self.pick = None;
                self.sessions_of = None;
                Outcome::default()
            }
            Intent::Dismiss(Scope::Chat(id)) => {
                self.chat_mut(id).map(Chat::dismiss).unwrap_or_default()
            }
            Intent::Cancel(id) => self.chat_mut(id).and_then(Chat::cancel).into(),
            Intent::CycleAgent(id, step) => {
                let effect = self.chat_mut(id).and_then(|c| c.cycle_agent(step));
                self.ask_once(effect.into())
            }
            Intent::CycleEffort(id) => {
                let effect = self.chat_mut(id).and_then(Chat::cycle_effort);
                self.ask_once(effect.into())
            }
            Intent::Seen(id) => {
                if let Some(chat) = self.chat_mut(id) {
                    chat.unseen = None;
                }
                Outcome::default()
            }
        }
    }

    /// Drops requests for options a node is already answering.
    fn ask_once(&mut self, mut outcome: Outcome) -> Outcome {
        outcome.effects.retain(|effect| match effect {
            Effect::FetchOptions { node, .. } => self.fetching.insert(node.clone()),
            _ => true,
        });
        outcome
    }

    fn open(&mut self, id: &str, chat: Option<ChatId>) -> Outcome {
        let Some(node) = self.nodes.iter().find(|n| n.id == id).cloned() else {
            return Outcome::default();
        };
        self.notice.clear();
        // Its chats are no use until it hosts an agent again.
        if node.online && node.harnesses.is_empty() {
            self.choose_harness(node);
            return Outcome::default();
        }
        let mut outcome = Outcome::default();
        if node.online {
            outcome.effects.push(Effect::FetchSessions(node.id.clone()));
        }
        let open = chat
            .filter(|id| self.chats.iter().any(|c| c.id == *id))
            .or_else(|| self.chats_on(&node.id).last().map(|c| c.id));
        if let Some(id) = open {
            outcome.go = Some(Go::Chat(id));
            return outcome;
        }
        if !node.online {
            self.notice = format!("{} is offline", node.name);
            return Outcome::default();
        }
        let settings = self.defaults.clone();
        outcome.and(self.new_chat(node, settings))
    }

    /// Offers the harnesses a node without one can start.
    fn choose_harness(&mut self, node: NodeInfo) {
        if self.starting.contains(&node.id) {
            self.notice = format!("{} is still starting its agent…", node.name);
            return;
        }
        if node.can_host.is_empty() {
            self.notice = format!(
                "{}'s worker can't start an agent when asked; update it, or restart it with --harness",
                node.name
            );
            return;
        }
        let choices = node
            .can_host
            .iter()
            .map(|name| {
                let kind = name.parse::<commandant_common::harness::HarnessKind>();
                let detail = kind.map_or("", |k| k.description());
                let choice = Choose::Harness {
                    node: node.id.clone(),
                    harness: name.clone(),
                };
                Choice::new(choice, name, detail)
            })
            .collect();
        self.pick = Some(Pick::new("Start an agent on this node", choices, None));
    }

    fn start_harness(&mut self, node: String, harness: String) -> Outcome {
        let name = self.node_name(&node);
        self.notice =
            format!("starting {harness} on {name}; installing it first can take a few minutes…");
        self.starting.insert(node.clone());
        Effect::StartHarness { node, harness }.into()
    }

    /// A node's name, else its id.
    fn node_name(&self, id: &str) -> String {
        let node = self.nodes.iter().find(|n| n.id == id);
        node.map_or_else(|| id.to_string(), |n| n.name.clone())
    }

    /// Starts a chat to show; it asks for the options the node hasn't given.
    fn new_chat(&mut self, node: NodeInfo, settings: Settings) -> Outcome {
        let id = self.next_id;
        self.next_id += 1;
        let options = self.options.get(&node.id).cloned();
        let fetch = options.is_none().then(|| Effect::fetch_options(&node.id));
        self.chats.push(Chat::new(id, node, settings, options));
        let outcome = Outcome {
            go: Some(Go::Chat(id)),
            ..fetch.into()
        };
        self.ask_once(outcome)
    }

    /// Another chat on the node `id` is on, with its settings but a new session.
    fn another_chat(&mut self, id: ChatId) -> Outcome {
        let Some(chat) = self.chat(id) else {
            return Outcome::default();
        };
        let settings = Settings {
            session_id: String::new(),
            ..chat.settings.clone()
        };
        self.new_chat(chat.node.clone(), settings)
    }

    /// Closes a chat, unless its agent is still at work, and says which of
    /// its node's chats to show instead: the one after it, else the one
    /// before, else the node list.
    fn close(&mut self, id: ChatId) -> Outcome {
        let Some(chat) = self.chat_mut(id) else {
            return Outcome::default();
        };
        if matches!(chat.activity, Activity::Working { .. }) {
            chat.info("the agent is still working: cancel with Esc first, or switch away");
            return Outcome::default();
        }
        let node = chat.node.id.clone();
        let siblings: Vec<ChatId> = self.chats_on(&node).map(|c| c.id).collect();
        let at = siblings.iter().position(|&c| c == id).unwrap_or(0);
        self.chats.retain(|c| c.id != id);
        let next = siblings
            .get(at + 1)
            .or_else(|| at.checked_sub(1).and_then(|before| siblings.get(before)));
        Outcome::go(next.map_or(Go::Nodes, |&next| Go::Chat(next)))
    }

    /// The node's open chats, then the sessions it saved that aren't open,
    /// for the chat `from`. `loading` while the node is asked for them again.
    fn open_session_picker(&mut self, from: ChatId, node_id: &str, loading: bool) {
        let mut choices = vec![Choice::new(Choose::NewSession, "+ New session", "ctrl-n")];
        for chat in self.chats_on(node_id) {
            let state = match &chat.activity {
                Activity::Working { .. } => "open · working",
                Activity::Idle => "open",
            };
            let detail = match chat.settings.session_id.as_str() {
                "" => state.to_string(),
                id => format!("{state} · {id}"),
            };
            choices.push(Choice::new(Choose::Chat(chat.id), chat.title(), detail));
        }
        let open: Vec<&str> = self
            .chats_on(node_id)
            .map(|c| c.settings.session_id.as_str())
            .collect();
        for session in self.saved.get(node_id).into_iter().flatten() {
            if open.contains(&session.id.as_str()) {
                continue;
            }
            let mut detail = vec![ago(session.updated)];
            if session.busy {
                detail.push("working".into());
            }
            detail.push(session.directory.clone());
            let label = chat::or(&session.title, &session.id).to_string();
            choices.push(Choice::new(
                Choose::Saved(session.clone()),
                label,
                detail.join(" · "),
            ));
        }
        let mut pick = Pick::new(SESSIONS, choices, Some(Choose::Chat(from)));
        pick.loading = loading;
        // Still open: the same picker, with what the node has said since.
        if let Some(open) = self
            .pick
            .as_ref()
            .filter(|_| self.sessions_of == Some(from))
        {
            pick = pick.revise(open);
        }
        self.pick = Some(pick);
        self.sessions_of = Some(from);
    }

    /// Does what the app's picker offered, which closes.
    fn choose(&mut self, choice: Choose) -> Outcome {
        self.pick = None;
        let from = self.sessions_of.take();
        match choice {
            Choose::NewSession => from.map_or_else(Outcome::default, |id| self.another_chat(id)),
            Choose::Chat(id) => Outcome::go(Go::Chat(id)),
            Choose::Saved(saved) => from.map_or_else(Outcome::default, |id| self.resume(id, saved)),
            Choose::Harness { node, harness } => self.start_harness(node, harness),
            _ => Outcome::default(),
        }
    }

    /// Opens a chat on a session the node of chat `from` saved, as it was
    /// last used.
    fn resume(&mut self, from: ChatId, saved: AgentSession) -> Outcome {
        let Some(chat) = self.chat(from) else {
            return Outcome::default();
        };
        let node = chat.node.clone();
        let id = self.next_id;
        let fetch = Effect::FetchHistory {
            chat: id,
            node: node.id.clone(),
            session_id: saved.id.clone(),
        };
        let settings = Settings {
            session_id: saved.id,
            cwd: saved.directory,
            model: saved.model,
            agent: saved.agent,
            effort: saved.variant,
        };
        let outcome = self.new_chat(node, settings);
        if let Some(chat) = self.chat_mut(id) {
            chat.title = saved.title;
            chat.spent = saved.cost;
            chat.loading_history = true;
        }
        outcome.and(fetch.into())
    }

    /// Records a node's options and hands them to its chats.
    fn set_options(
        &mut self,
        node: &str,
        options: Result<AgentOptions, String>,
    ) -> Option<Arc<AgentOptions>> {
        let options = options.map(Arc::new);
        if let Ok(options) = &options {
            self.options.insert(node.to_string(), options.clone());
        }
        for chat in self.chats.iter_mut().filter(|c| c.node.id == node) {
            chat.on_message(Message::Options(options.clone()));
        }
        options.ok()
    }

    /// Takes in what a background call reports, which may call for more.
    pub fn update(&mut self, update: Update) -> Outcome {
        match update {
            Update::Chat(id, message) => {
                let effect = self.chat_mut(id).and_then(|c| c.on_message(message));
                // Signing in asks for the options again, maybe while they're being asked for.
                self.ask_once(effect.into())
            }
            Update::Options(node, Err(failure)) if failure.transient => {
                // Busy starting its agent, say: ask again before saying so.
                let tries = self.retries.entry(node.clone()).or_default();
                if *tries < OPTIONS_RETRIES {
                    *tries += 1;
                    return Effect::FetchOptions {
                        node,
                        after: OPTIONS_RETRY,
                    }
                    .into();
                }
                self.update(Update::Options(
                    node,
                    Err(Failure {
                        transient: false,
                        ..failure
                    }),
                ))
            }
            Update::Options(node, options) => {
                self.retries.remove(&node);
                let options = self.set_options(&node, options.map_err(|f| f.message));
                // The rest is in; the commands follow once they've loaded,
                // and the request stays open until then.
                if options.is_some_and(|o| o.loading) {
                    return Effect::FetchOptions {
                        node,
                        after: OPTIONS_RETRY,
                    }
                    .into();
                }
                self.fetching.remove(&node);
                Outcome::default()
            }
            Update::McpSwitched {
                chat,
                node,
                name,
                options,
            } => {
                let switched = options.clone().map(Arc::new);
                if options.is_ok() {
                    self.set_options(&node, options);
                }
                if let Some(chat) = self.chat_mut(chat) {
                    chat.mcp_switched(&name, &switched);
                }
                Outcome::default()
            }
            Update::Nodes(nodes) if nodes == self.nodes => Outcome::default(),
            Update::Nodes(nodes) => {
                for chat in &mut self.chats {
                    if let Some(node) = nodes.iter().find(|n| n.id == chat.node.id) {
                        chat.on_message(Message::Node(node.clone()));
                    }
                }
                self.nodes = nodes;
                Outcome::default()
            }
            Update::HarnessStarted(id, started) => {
                self.starting.remove(&id);
                let name = self.node_name(&id);
                let node = match started {
                    Ok(node) => node,
                    Err(e) => {
                        self.notice = format!("couldn't start an agent on {name}: {e}");
                        return Outcome::default();
                    }
                };
                let hosts = node.harnesses.join(", ");
                if let Some(known) = self.nodes.iter_mut().find(|n| n.id == id) {
                    *known = node;
                }
                // What its chats knew came from an agent that's gone.
                self.options.remove(&id);
                let mut outcome = Outcome::default();
                if self.chats_on(&id).next().is_some() {
                    outcome = self.ask_once(Effect::fetch_options(&id).into());
                }
                self.notice = format!("{name} now hosts {hosts}");
                outcome.go = Some(Go::Ready(id));
                outcome
            }
            Update::Sessions(node, sessions) => {
                let picking = self.pick.as_ref().is_some_and(|p| p.title == SESSIONS);
                let from = self.sessions_of.filter(|_| picking);
                match sessions {
                    Ok(sessions) => {
                        self.saved.insert(node.clone(), sessions);
                    }
                    Err(e) => {
                        // Only worth saying to someone looking for them.
                        if let Some(chat) = from.and_then(|id| self.chat_mut(id)) {
                            chat.info(&format!("couldn't list the node's sessions: {e}"));
                        }
                    }
                }
                if let Some(from) = from
                    && self.chat(from).is_some_and(|c| c.node.id == node)
                {
                    self.open_session_picker(from, &node, false);
                }
                Outcome::default()
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::chat::{Role, Unseen};
    use super::*;

    pub(crate) fn node(id: &str, online: bool) -> NodeInfo {
        NodeInfo {
            id: id.into(),
            name: format!("box-{id}"),
            online,
            harnesses: vec!["opencode".into()],
            ..Default::default()
        }
    }

    /// An online node hosting no agent, which can start one.
    pub(crate) fn bare(id: &str) -> NodeInfo {
        NodeInfo {
            harnesses: Vec::new(),
            can_host: vec!["opencode".into()],
            ..node(id, true)
        }
    }

    pub(crate) fn state() -> State {
        let nodes = vec![node("n1", true), node("n2", true), node("n3", false)];
        State::new(nodes, Settings::default())
    }

    fn open(state: &mut State, node: &str) -> Outcome {
        state.intent(Intent::Open {
            node: node.into(),
            chat: None,
        })
    }

    /// The chat an outcome goes to.
    fn shown(outcome: &Outcome) -> ChatId {
        match outcome.go {
            Some(Go::Chat(id)) => id,
            ref go => panic!("should show a chat, not {go:?}"),
        }
    }

    /// Opens a chat on `node` and says which.
    fn chat_on(state: &mut State, node: &str) -> ChatId {
        shown(&open(state, node))
    }

    fn another(state: &mut State, id: ChatId) -> ChatId {
        shown(&state.intent(Intent::NewChat(id)))
    }

    fn submit(state: &mut State, id: ChatId, text: &str) -> Outcome {
        state.intent(Intent::Submit(id, text.into()))
    }

    /// Sends `text` from a chat and returns the request.
    fn send(state: &mut State, id: ChatId, text: &str) -> PromptRequest {
        match submit(state, id, text).effects.pop() {
            Some(Effect::Send(chat, request)) if chat == id => request,
            _ => panic!("{text:?} should be sent"),
        }
    }

    fn finished(session_id: &str) -> Message {
        Message::Task(task_event::Event::Finished(TaskFinished {
            exit_code: Some(0),
            session_id: session_id.into(),
            ..Default::default()
        }))
    }

    pub(crate) fn started(task_id: &str) -> Message {
        Message::Task(task_event::Event::Started(TaskStarted {
            task_id: task_id.into(),
            ..Default::default()
        }))
    }

    fn chat(state: &State, id: ChatId) -> &Chat {
        state.chat(id).expect("the chat is open")
    }

    fn fetches(outcome: &Outcome) -> usize {
        let fetch = |e: &&Effect| matches!(e, Effect::FetchOptions { .. });
        outcome.effects.iter().filter(fetch).count()
    }

    #[test]
    fn a_node_opens_on_its_chat_or_a_new_one() {
        let mut state = state();
        let outcome = open(&mut state, "n2");
        // It asks the node for its sessions and options.
        assert!(matches!(&outcome.effects[..],
            [Effect::FetchSessions(a), Effect::FetchOptions { node: b, .. }] if a == "n2" && b == "n2"));
        let id = shown(&outcome);
        assert_eq!(chat(&state, id).node.id, "n2");

        // An offline node with no chat yet can't be opened.
        let offline = open(&mut state, "n3");
        assert!(offline.effects.is_empty() && offline.go.is_none());
        assert_eq!(state.notice, "box-n3 is offline");

        // Going back to a node shows its chat again.
        let again = state.intent(Intent::Open {
            node: "n2".into(),
            chat: Some(id),
        });
        assert_eq!(again.go, Some(Go::Chat(id)));
        assert_eq!(state.chats.len(), 1);
        assert!(state.notice.is_empty());
    }

    #[test]
    fn sessions_on_a_node_run_side_by_side() {
        let mut state = state();
        let first = chat_on(&mut state, "n1");
        let request = send(&mut state, first, "fix the parser");
        assert_eq!(request.node, "n1");

        // A second session starts while the first is still working.
        let second = another(&mut state, first);
        assert_ne!(first, second);
        assert_eq!(send(&mut state, second, "write the docs").session_id, "");
        assert_eq!(state.chats_on("n1").count(), 2);

        // Each says how it ended until it is seen.
        state.update(Update::Chat(first, finished("ses_a")));
        state.update(Update::Chat(second, finished("ses_b")));
        assert_eq!(chat(&state, first).unseen, Some(Unseen::Done));
        state.intent(Intent::Seen(first));
        assert_eq!(chat(&state, first).unseen, None);
        assert_eq!(chat(&state, second).unseen, Some(Unseen::Done));

        // Each continues its own session.
        let request = send(&mut state, first, "and the lexer");
        assert_eq!(request.session_id, "ses_a");
        assert_eq!(state.running_tasks().len(), 0, "no task id until started");
    }

    #[test]
    fn saved_sessions_are_resumed_from_the_picker() {
        let mut state = state();
        let first = chat_on(&mut state, "n1");
        state.update(Update::Sessions(
            "n1".into(),
            Ok(vec![AgentSession {
                id: "ses_old".into(),
                title: "refactor the store".into(),
                directory: "/src/app".into(),
                agent: "plan".into(),
                model: "a/smart".into(),
                cost: 0.25,
                ..Default::default()
            }]),
        ));
        let outcome = state.intent(Intent::Sessions(first));
        assert!(matches!(&outcome.effects[..], [Effect::FetchSessions(n)] if n == "n1"));
        let pick = state.pick(Scope::App).expect("the sessions to pick from");
        // New session, the open chat, the saved one.
        assert_eq!(pick.choices.len(), 3);
        assert_eq!(pick.current, Some(Choose::Chat(first)));

        let saved = pick.choices[2].value.clone();
        assert_eq!(pick.choices[2].label, "refactor the store");
        let outcome = state.intent(Intent::Choose(Scope::App, saved));
        let resumed = shown(&outcome);
        let Some(Effect::FetchHistory {
            chat: asking,
            session_id,
            ..
        }) = outcome.effects.last()
        else {
            panic!("resuming asks for the session's earlier messages");
        };
        assert_eq!((*asking, session_id.as_str()), (resumed, "ses_old"));
        assert!(state.pick(Scope::App).is_none());
        assert!(state.busy(), "loading them shows a spinner");

        // What was said since resuming stays after them.
        state.update(Update::Chat(resumed, started("t1")));
        state.update(Update::Chat(
            resumed,
            chat::tests::output(OutputStream::Stdout, b"new"),
        ));
        let entry = |role: &str, text: &str| HistoryEntry {
            role: role.into(),
            text: text.into(),
        };
        let history = Ok(vec![
            entry("user", "old question"),
            entry("agent", "old answer"),
        ]);
        state.update(Update::Chat(resumed, Message::History(history)));
        let chat = chat(&state, resumed);
        assert!(!chat.loading_history);
        let thread: Vec<_> = chat
            .thread
            .iter()
            .map(|e| (e.role, e.text.as_str()))
            .collect();
        assert_eq!(
            thread,
            [
                (Role::User, "old question"),
                (Role::Agent, "old answer"),
                (Role::Agent, "new"),
            ]
        );
        assert_eq!(chat.title, "refactor the store");
        assert_eq!(chat.settings.session_id, "ses_old");
        assert_eq!(chat.settings.cwd, "/src/app");
        assert_eq!(
            (chat.settings.agent.as_str(), chat.settings.model.as_str()),
            ("plan", "a/smart")
        );
        assert_eq!(state.chats.len(), 2);

        // An open session isn't offered twice.
        state.intent(Intent::Sessions(resumed));
        assert_eq!(state.pick(Scope::App).unwrap().choices.len(), 3);
    }

    #[test]
    fn a_working_chat_stays_open() {
        let mut state = state();
        let first = chat_on(&mut state, "n1");
        send(&mut state, first, "hi");
        let refused = state.intent(Intent::Close(first));
        assert!(refused.go.is_none());
        assert_eq!(chat(&state, first).thread.last().unwrap().role, Role::Info);

        // `/close` closes the chat it is typed in.
        let second = another(&mut state, first);
        let closed = submit(&mut state, second, "/close");
        assert_eq!(closed.go, Some(Go::Chat(first)));
        assert_eq!(closed.prompt, Some((second, Edit::Clear)));
        assert_eq!(state.chats.len(), 1);

        state.update(Update::Chat(first, finished("ses_a")));
        assert_eq!(state.intent(Intent::Close(first)).go, Some(Go::Nodes));
        assert!(state.chats.is_empty());
    }

    #[test]
    fn options_reach_every_chat_on_the_node() {
        let mut state = state();
        let first = chat_on(&mut state, "n1");
        another(&mut state, first);
        let options = AgentOptions {
            default_agent: "build".into(),
            ..Default::default()
        };
        state.update(Update::Options("n1".into(), Ok(options)));
        assert!(state.chats.iter().all(|c| c.agent() == "build"));
        // A later chat starts with them, without asking again.
        let later = state.intent(Intent::NewChat(first));
        assert!(later.effects.is_empty());
        assert_eq!(chat(&state, shown(&later)).agent(), "build");
    }

    #[test]
    fn closing_a_chat_shows_its_neighbour() {
        let mut state = state();
        let first = chat_on(&mut state, "n1");
        let middle = another(&mut state, first);
        let last = another(&mut state, middle);

        // The middle one gives way to the one after it...
        assert_eq!(state.intent(Intent::Close(middle)).go, Some(Go::Chat(last)));
        // ...the last one to the one before.
        assert_eq!(state.intent(Intent::Close(last)).go, Some(Go::Chat(first)));
        // Updates for closed chats are dropped.
        let late = state.update(Update::Chat(middle, started("t1")));
        assert!(late.effects.is_empty());
        assert!(state.running_tasks().is_empty());
    }

    #[test]
    fn quitting_leaves_the_tasks_on_every_node_to_cancel() {
        let mut state = state();
        let one = chat_on(&mut state, "n1");
        send(&mut state, one, "one");
        let two = chat_on(&mut state, "n2");
        send(&mut state, two, "two");
        state.update(Update::Chat(one, started("t1")));
        state.update(Update::Chat(two, started("t2")));
        let mut running = state.running_tasks();
        running.sort();
        assert_eq!(running, ["t1", "t2"]);
        assert_eq!(submit(&mut state, one, "/quit").go, Some(Go::Quit));
        assert_eq!(submit(&mut state, two, "/exit").go, Some(Go::Quit));
    }

    #[test]
    fn cancelling_before_a_background_task_starts_still_cancels_it() {
        let mut state = state();
        let id = chat_on(&mut state, "n1");
        send(&mut state, id, "hi");
        assert!(state.intent(Intent::Cancel(id)).effects.is_empty());
        // Even if another chat is shown when the task starts.
        another(&mut state, id);
        let outcome = state.update(Update::Chat(id, started("t1")));
        assert!(matches!(&outcome.effects[..], [Effect::Cancel(t)] if t == "t1"));
    }

    #[test]
    fn chats_follow_their_node() {
        let mut state = state();
        let id = chat_on(&mut state, "n1");
        let mut offline = node("n1", false);
        offline.version = "0.2.0".into();
        state.update(Update::Nodes(vec![node("n2", true), offline]));
        let chat = chat(&state, id);
        assert!(!chat.node.online);
        assert_eq!(chat.node.version, "0.2.0");

        // An offline node's chat is still there to show.
        let reopened = state.intent(Intent::Open {
            node: "n1".into(),
            chat: None,
        });
        assert_eq!(reopened.go, Some(Go::Chat(id)));
        assert!(
            reopened.effects.is_empty(),
            "nothing to ask an offline node"
        );
        // A node that's gone opens nothing.
        state.update(Update::Nodes(Vec::new()));
        let gone = open(&mut state, "n1");
        assert!(gone.go.is_none() && gone.effects.is_empty());
    }

    #[test]
    fn an_old_worker_without_an_agent_isnt_opened() {
        let mut old = bare("n9");
        old.can_host.clear();
        let mut state = State::new(vec![old], Settings::default());
        let outcome = open(&mut state, "n9");
        assert!(outcome.effects.is_empty() && outcome.go.is_none());
        assert!(state.pick(Scope::App).is_none());
        assert!(
            state.notice.contains("can't start an agent"),
            "{}",
            state.notice
        );
    }

    #[test]
    fn the_session_picker_keeps_up_with_the_node() {
        let mut state = state();
        let id = chat_on(&mut state, "n1");
        let saved = |id: &str| AgentSession {
            id: id.into(),
            title: format!("about {id}"),
            ..Default::default()
        };
        // Failing to list sessions isn't news unless someone is looking.
        state.update(Update::Sessions("n1".into(), Err("offline".into())));
        assert!(chat(&state, id).thread.is_empty());

        state.intent(Intent::Sessions(id));
        let asked = state.pick(Scope::App).unwrap().clone();
        assert!(asked.loading);
        state.update(Update::Sessions(
            "n1".into(),
            Ok(vec![saved("a"), saved("b")]),
        ));
        // The same picker, revised, done loading.
        let listed = state.pick(Scope::App).unwrap().clone();
        assert_eq!(listed.id, asked.id);
        assert_ne!(listed.revision, asked.revision);
        assert!(!listed.loading);
        assert_eq!(listed.choices.len(), 4);
        // Another node's sessions don't land in it.
        state.update(Update::Sessions("n2".into(), Ok(vec![saved("z")])));
        assert_eq!(state.pick(Scope::App).unwrap().choices.len(), 4);

        state.update(Update::Sessions("n1".into(), Err("offline".into())));
        let last = &chat(&state, id).thread.last().unwrap().text;
        assert!(last.contains("couldn't list the node's sessions"), "{last}");

        // "+ New session" is a new chat.
        let new = state.intent(Intent::Choose(Scope::App, Choose::NewSession));
        assert_ne!(shown(&new), id);
        assert_eq!(state.chats.len(), 2);
    }

    #[test]
    fn a_node_is_asked_for_its_options_once_at_a_time() {
        let mut state = state();
        let first = open(&mut state, "n1");
        assert_eq!(fetches(&first), 1);
        let first = shown(&first);
        // A second chat waits for the same answer.
        let second = state.intent(Intent::NewChat(first));
        assert_eq!(fetches(&second), 0);
        let second = shown(&second);
        state.update(Update::Options("n1".into(), Err("timed out".into())));

        // Both were waiting, so both hear it failed, once each.
        let errors = |state: &State, id| {
            let thread = &chat(state, id).thread;
            thread
                .iter()
                .filter(|e| e.text.contains("timed out"))
                .count()
        };
        assert_eq!((errors(&state, first), errors(&state, second)), (1, 1));
        // Once answered, it can be asked again, but not twice at once.
        assert_eq!(fetches(&state.intent(Intent::CycleAgent(second, 1))), 1);
        assert_eq!(fetches(&state.intent(Intent::CycleAgent(first, 1))), 0);
    }

    #[test]
    fn a_node_without_an_agent_offers_to_start_one() {
        let mut state = State::new(vec![bare("n1")], Settings::default());
        assert!(open(&mut state, "n1").effects.is_empty());
        let pick = state.pick(Scope::App).expect("a harness to choose");
        let harness = Choose::Harness {
            node: "n1".into(),
            harness: "opencode".into(),
        };
        assert_eq!(pick.choices[0].value, harness);
        assert!(!pick.choices[0].detail.is_empty());

        // Backing out starts nothing.
        state.intent(Intent::Dismiss(Scope::App));
        assert!(state.pick(Scope::App).is_none());
        assert!(state.starting.is_empty());

        open(&mut state, "n1");
        let outcome = state.intent(Intent::Choose(Scope::App, harness));
        assert!(matches!(&outcome.effects[..],
            [Effect::StartHarness { node, harness }] if node == "n1" && harness == "opencode"));
        assert!(state.starting.contains("n1"));
        assert!(
            state.notice.contains("starting opencode"),
            "{}",
            state.notice
        );
        // Asking again while it starts doesn't start another.
        assert!(open(&mut state, "n1").effects.is_empty());
        assert!(state.pick(Scope::App).is_none());
        assert!(state.notice.contains("still starting"), "{}", state.notice);

        // Once it hosts it, the node is worth opening.
        let ready = NodeInfo {
            harnesses: vec!["opencode".into()],
            ..bare("n1")
        };
        let outcome = state.update(Update::HarnessStarted("n1".into(), Ok(ready)));
        assert!(outcome.effects.is_empty());
        assert_eq!(outcome.go, Some(Go::Ready("n1".into())));
        assert!(state.starting.is_empty());
        assert_eq!(state.notice, "box-n1 now hosts opencode");
        assert_eq!(state.nodes[0].harnesses, ["opencode"]);
        let opened = open(&mut state, "n1");
        assert!(matches!(
            &opened.effects[..],
            [Effect::FetchSessions(_), Effect::FetchOptions { .. }]
        ));
        assert_eq!(chat(&state, shown(&opened)).node.harnesses, ["opencode"]);
    }

    #[test]
    fn a_harness_that_fails_to_start_is_reported() {
        let mut state = State::new(vec![bare("n1")], Settings::default());
        open(&mut state, "n1");
        let harness = state.pick(Scope::App).unwrap().choices[0].value.clone();
        state.intent(Intent::Choose(Scope::App, harness));
        let failed = Update::HarnessStarted("n1".into(), Err("no curl on the node".into()));
        let outcome = state.update(failed);
        assert!(outcome.effects.is_empty() && outcome.go.is_none());
        assert!(
            state
                .notice
                .contains("couldn't start an agent on box-n1: no curl"),
            "{}",
            state.notice
        );
        assert!(state.starting.is_empty(), "it can be tried again");
    }

    #[test]
    fn commands_that_are_still_loading_are_asked_for_again() {
        let mut state = state();
        let id = chat_on(&mut state, "n1");
        let loading = AgentOptions {
            default_agent: "build".into(),
            loading: true,
            ..Default::default()
        };
        let outcome = state.update(Update::Options("n1".into(), Ok(loading)));
        assert!(matches!(&outcome.effects[..],
            [Effect::FetchOptions { node, after }] if node == "n1" && !after.is_zero()));
        // The rest is usable already.
        assert_eq!(chat(&state, id).agent(), "build");

        // Meanwhile a command waits in the prompt, rather than going out as text.
        let waits = submit(&mut state, id, "/review the parser");
        assert!(waits.effects.is_empty());
        assert_eq!(waits.prompt, None);
        let said = &chat(&state, id).thread.last().unwrap().text;
        assert!(said.contains("still loading"), "{said}");

        let loaded = AgentOptions {
            default_agent: "build".into(),
            commands: vec![AgentCommand {
                name: "review".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let outcome = state.update(Update::Options("n1".into(), Ok(loaded)));
        assert!(outcome.effects.is_empty());
        let request = send(&mut state, id, "/review the parser");
        assert_eq!(
            (request.command.as_str(), request.prompt.as_str()),
            ("review", "the parser")
        );
    }

    #[test]
    fn an_mcp_switch_answers_its_chat_and_leaves_other_requests_alone() {
        let mut state = state();
        // Opening the node asks for its options; that request is open.
        let asking = chat_on(&mut state, "n1");
        let other = another(&mut state, asking);
        let switched = AgentOptions {
            default_agent: "plan".into(),
            ..Default::default()
        };
        let update = Update::McpSwitched {
            chat: asking,
            node: "n1".into(),
            name: "docs".into(),
            options: Ok(switched),
        };
        assert!(state.update(update).effects.is_empty());
        // Every chat on the node has the new options...
        assert!(state.chats.iter().all(|c| c.agent() == "plan"));
        // ...the open request is still open...
        assert!(state.fetching.contains("n1"));
        // ...and only the chat that switched hears how it went.
        let failed = Update::McpSwitched {
            chat: asking,
            node: "n1".into(),
            name: "docs".into(),
            options: Err("timed out".into()),
        };
        state.update(failed);
        let said = |state: &State, id| {
            let thread = &chat(state, id).thread;
            thread
                .iter()
                .any(|e| e.text.contains("couldn't switch docs"))
        };
        assert!(said(&state, asking));
        assert!(!said(&state, other));
    }

    #[test]
    fn busy_while_something_moves() {
        let mut state = state();
        assert!(!state.busy());
        let id = chat_on(&mut state, "n1");
        assert!(state.busy(), "asking for the options shows a spinner");
        state.update(Update::Options("n1".into(), Ok(AgentOptions::default())));
        send(&mut state, id, "hi");
        assert!(state.busy(), "a working chat's spinner turns");
        state.update(Update::Chat(id, finished("ses_a")));
        assert!(!state.busy());
        state.starting.insert("n2".into());
        assert!(state.busy(), "so does a node starting its agent");
    }

    #[test]
    fn a_node_that_lost_its_agent_offers_one_again_then_refreshes_its_chats() {
        let mut state = state();
        let id = chat_on(&mut state, "n1");
        state.update(Update::Options("n1".into(), Ok(AgentOptions::default())));

        // Its worker restarted without --harness.
        let mut nodes = state.nodes.clone();
        nodes[0] = bare("n1");
        state.update(Update::Nodes(nodes));
        let open_it = Intent::Open {
            node: "n1".into(),
            chat: Some(id),
        };
        let outcome = state.intent(open_it.clone());
        assert!(outcome.go.is_none(), "not the stale chat");
        let harness = state.pick(Scope::App).expect("the agent picker");
        let harness = harness.choices[0].value.clone();
        state.intent(Intent::Choose(Scope::App, harness));

        let ready = NodeInfo {
            harnesses: vec!["opencode".into()],
            ..bare("n1")
        };
        let outcome = state.update(Update::HarnessStarted("n1".into(), Ok(ready)));
        // Its chat asks the new agent what it offers, and can be shown again.
        let asks = |e: &Effect| matches!(e, Effect::FetchOptions { node, .. } if node == "n1");
        assert!(outcome.effects.iter().any(asks));
        assert_eq!(state.intent(open_it).go, Some(Go::Chat(id)));
    }

    #[test]
    fn a_node_that_may_answer_in_a_moment_is_asked_again_quietly() {
        let mut state = state();
        let id = chat_on(&mut state, "n1");
        let timed_out = || {
            let failure = Failure {
                message: "node box-n1 didn't say what its agent offers".into(),
                transient: true,
            };
            Update::Options("n1".into(), Err(failure))
        };
        let errors = |state: &State| {
            let thread = &chat(state, id).thread;
            thread.iter().filter(|e| e.role == Role::Error).count()
        };
        // Twice it asks again, saying nothing...
        for _ in 0..OPTIONS_RETRIES {
            let outcome = state.update(timed_out());
            assert!(
                matches!(&outcome.effects[..], [Effect::FetchOptions { after, .. }] if !after.is_zero())
            );
            assert_eq!(errors(&state), 0);
        }
        // ...then it says why, and stops asking.
        assert!(state.update(timed_out()).effects.is_empty());
        assert_eq!(errors(&state), 1);
        assert!(!state.fetching.contains("n1"));

        // An answer resets the count; a refusal is shown at once.
        state.update(Update::Options("n1".into(), Ok(AgentOptions::default())));
        assert!(!state.retries.contains_key("n1"));
        another(&mut state, id);
        let refused = Update::Options("n1".into(), Err("node box-n1 runs no agent harness".into()));
        assert!(state.update(refused).effects.is_empty());
    }
}
