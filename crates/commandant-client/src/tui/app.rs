//! The whole UI's state: the nodes, and every chat open on them. Chats on
//! any node run side by side; one is shown at a time.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use commandant_common::time::ago;
use commandant_proto::*;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::chat::{self, Activity, Chat, Message, Settings};
use super::picker::{Choice, Picker};

/// Tells chats apart for as long as the UI runs.
pub type ChatId = u64;

/// How long to wait before asking again for commands that were loading.
const OPTIONS_RETRY: Duration = Duration::from_secs(3);
const SESSIONS: &str = "Sessions on this node";
/// How often a node that may answer in a moment is asked again for its
/// options before the failure is shown.
const OPTIONS_RETRIES: u32 = 2;

/// Something for the event loop to do, or (the chat-level ones) for the app.
pub enum Action {
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
    /// Start another chat on the same node.
    NewChat,
    /// Pick one of the node's chats or saved sessions.
    ShowSessions,
    ShowNodes,
    CloseChat,
    /// Have a node with no agent start one.
    StartHarness {
        node: String,
        harness: String,
    },
    Quit,
}

impl Action {
    pub fn fetch_options(node: &str) -> Self {
        Self::FetchOptions {
            node: node.to_string(),
            after: Duration::ZERO,
        }
    }
}

/// What background tasks report to the UI.
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Screen {
    #[default]
    Nodes,
    Chat(ChatId),
}

/// A choice in the app's own pickers.
#[derive(Debug, Clone, PartialEq)]
pub enum AppChoice {
    NewSession,
    Chat(ChatId),
    Saved(AgentSession),
    Harness { node: String, harness: String },
}

#[derive(Default)]
pub struct App {
    pub screen: Screen,
    pub nodes: Vec<NodeInfo>,
    /// The highlighted row of the node list.
    pub selected: usize,
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
    /// The chat last shown on each node.
    last: HashMap<String, ChatId>,
    /// The session or harness picker, when open; a chat's own pickers live
    /// in the chat.
    pub picker: Option<Picker<AppChoice>>,
    /// A remark on the node screen.
    pub notice: String,
    /// Nodes starting a harness, which can take minutes.
    pub starting: HashSet<String>,
    /// What new chats start with; `--session` only goes to the first.
    defaults: Settings,
    next_id: ChatId,
}

impl App {
    pub fn new(nodes: Vec<NodeInfo>, defaults: Settings) -> Self {
        // Start on the first node that can take a prompt.
        let selected = nodes
            .iter()
            .position(|n| n.online && !n.harnesses.is_empty())
            .unwrap_or(0);
        Self {
            nodes,
            selected,
            defaults,
            next_id: 1,
            ..Default::default()
        }
    }

    /// The chat on screen, if any.
    pub fn chat(&self) -> Option<&Chat> {
        match self.screen {
            Screen::Chat(id) => self.chats.iter().find(|c| c.id == id),
            Screen::Nodes => None,
        }
    }

    fn chat_mut(&mut self) -> Option<&mut Chat> {
        match self.screen {
            Screen::Chat(id) => self.chats.iter_mut().find(|c| c.id == id),
            Screen::Nodes => None,
        }
    }

    /// The chats on a node, oldest first.
    pub fn chats_on<'a>(&'a self, node_id: &'a str) -> impl Iterator<Item = &'a Chat> {
        self.chats.iter().filter(move |c| c.node.id == node_id)
    }

    /// Whether anything on screen moves on its own: a spinner for a working
    /// chat, something loading, or a node starting its agent.
    pub fn busy(&self) -> bool {
        !self.starting.is_empty()
            || self.picker.as_ref().is_some_and(|p| p.loading)
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
    pub fn open_node_with(&mut self, node: NodeInfo, settings: Settings) -> Vec<Action> {
        let mut actions = vec![Action::FetchSessions(node.id.clone())];
        actions.extend(self.new_chat(node, settings));
        actions
    }

    pub fn on_input(&mut self, event: Event) -> Vec<Action> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.on_key(key),
            Event::Paste(_) => match self.chat_mut() {
                Some(chat) => chat.on_input(event).into_iter().collect(),
                None => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if ctrl && matches!(key.code, KeyCode::Char('c' | 'd')) {
            return vec![Action::Quit];
        }
        if let Some(chosen) = Picker::take_key(&mut self.picker, key) {
            return chosen.map_or_else(Vec::new, |choice| self.choose(choice));
        }
        if self.screen == Screen::Nodes {
            return self.on_node_key(key);
        }
        let chat_picking = self.chat().is_some_and(|c| c.picker.is_some());
        let action = match key.code {
            _ if chat_picking => None,
            KeyCode::Char('n') if ctrl => Some(Action::NewChat),
            KeyCode::Char('o') if ctrl => Some(Action::ShowSessions),
            KeyCode::Char('g') if ctrl => Some(Action::ShowNodes),
            KeyCode::Char('w') if ctrl => Some(Action::CloseChat),
            KeyCode::Left if alt => return self.cycle_chat(-1),
            KeyCode::Right if alt => return self.cycle_chat(1),
            _ => None,
        };
        let action = match action {
            Some(action) => Some(action),
            None => self.chat_mut().and_then(|chat| chat.on_key(key)),
        };
        action.map_or_else(Vec::new, |action| self.handle(action))
    }

    /// Carries out what is the app's to do, and passes the rest on.
    fn handle(&mut self, action: Action) -> Vec<Action> {
        let Some(chat) = self.chat() else {
            return vec![action];
        };
        let node = chat.node.clone();
        match action {
            Action::NewChat => {
                let settings = Settings {
                    session_id: String::new(),
                    ..chat.settings.clone()
                };
                self.new_chat(node, settings)
            }
            Action::ShowSessions => {
                self.open_session_picker(&node.id, true);
                vec![Action::FetchSessions(node.id)]
            }
            Action::ShowNodes => {
                self.show_nodes();
                Vec::new()
            }
            Action::CloseChat => {
                self.close_chat();
                Vec::new()
            }
            action => self.ask_once(action).into_iter().collect(),
        }
    }

    /// Drops a request for options a node is already answering.
    fn ask_once(&mut self, action: Action) -> Option<Action> {
        match &action {
            Action::FetchOptions { node, .. } if !self.fetching.insert(node.clone()) => None,
            _ => Some(action),
        }
    }

    fn on_node_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let last = self.nodes.len().saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(last),
            KeyCode::Char('q') => return vec![Action::Quit],
            KeyCode::Enter => return self.open_selected_node(),
            _ => {}
        }
        Vec::new()
    }

    /// Shows the node's last chat, or starts one.
    fn open_selected_node(&mut self) -> Vec<Action> {
        let Some(node) = self.nodes.get(self.selected).cloned() else {
            return Vec::new();
        };
        self.notice.clear();
        // Its chats are no use until it hosts an agent again.
        if node.online && node.harnesses.is_empty() {
            self.choose_harness(node);
            return Vec::new();
        }
        let mut actions = Vec::new();
        if node.online {
            actions.push(Action::FetchSessions(node.id.clone()));
        }
        let last = self.last.get(&node.id).copied();
        let open = last
            .filter(|id| self.chats.iter().any(|c| c.id == *id))
            .or_else(|| self.chats_on(&node.id).last().map(|c| c.id));
        if let Some(id) = open {
            self.show(id);
            return actions;
        }
        if !node.online {
            self.notice = format!("{} is offline", node.name);
            return Vec::new();
        }
        let settings = self.defaults.clone();
        actions.extend(self.new_chat(node, settings));
        actions
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
                let choice = AppChoice::Harness {
                    node: node.id.clone(),
                    harness: name.clone(),
                };
                Choice::new(choice, name, detail)
            })
            .collect();
        self.picker = Some(Picker::new("Start an agent on this node", choices, None));
    }

    fn start_harness(&mut self, node: String, harness: String) -> Vec<Action> {
        let name = self.node_name(&node);
        self.notice =
            format!("starting {harness} on {name}; installing it first can take a few minutes…");
        self.starting.insert(node.clone());
        vec![Action::StartHarness { node, harness }]
    }

    /// A node's name, else its id.
    fn node_name(&self, id: &str) -> String {
        let node = self.nodes.iter().find(|n| n.id == id);
        node.map_or_else(|| id.to_string(), |n| n.name.clone())
    }

    /// Starts a chat and shows it; it asks for the options the node hasn't given.
    fn new_chat(&mut self, node: NodeInfo, settings: Settings) -> Vec<Action> {
        let id = self.next_id;
        self.next_id += 1;
        let options = self.options.get(&node.id).cloned();
        let fetch = options.is_none().then(|| Action::fetch_options(&node.id));
        self.chats.push(Chat::new(id, node, settings, options));
        self.show(id);
        fetch.into_iter().filter_map(|a| self.ask_once(a)).collect()
    }

    fn show(&mut self, id: ChatId) {
        if let Some(chat) = self.chats.iter_mut().find(|c| c.id == id) {
            chat.unseen = None;
            self.last.insert(chat.node.id.clone(), id);
            self.screen = Screen::Chat(id);
        }
    }

    fn show_nodes(&mut self) {
        if let Some(chat) = self.chat()
            && let Some(at) = self.nodes.iter().position(|n| n.id == chat.node.id)
        {
            self.selected = at;
        }
        self.picker = None;
        self.screen = Screen::Nodes;
    }

    /// Closes the chat on screen, unless its agent is still at work.
    fn close_chat(&mut self) {
        let Some(chat) = self.chat_mut() else {
            return;
        };
        if matches!(chat.activity, Activity::Working { .. }) {
            chat.info("the agent is still working: cancel with Esc first, or switch away");
            return;
        }
        let (id, node) = (chat.id, chat.node.id.clone());
        let siblings: Vec<ChatId> = self.chats_on(&node).map(|c| c.id).collect();
        let at = siblings.iter().position(|&c| c == id).unwrap_or(0);
        self.chats.retain(|c| c.id != id);
        self.last.remove(&node);
        // The neighbour on the same node, else the node list.
        let next = siblings
            .get(at + 1)
            .or_else(|| at.checked_sub(1).and_then(|before| siblings.get(before)));
        match next {
            Some(&next) => self.show(next),
            None => self.show_nodes(),
        }
    }

    /// Shows the next (or previous) chat on the same node.
    fn cycle_chat(&mut self, step: isize) -> Vec<Action> {
        let Some(chat) = self.chat() else {
            return Vec::new();
        };
        let ids: Vec<ChatId> = self.chats_on(&chat.node.id).map(|c| c.id).collect();
        if let Some(next) = chat::cycle(&ids, chat.id, step) {
            self.show(next);
        }
        Vec::new()
    }

    /// The node's open chats, then the sessions it saved that aren't open.
    /// `loading` while the node is asked for them again.
    fn open_session_picker(&mut self, node_id: &str, loading: bool) {
        let filter = self.picker.take().map(|p| p.filter).unwrap_or_default();
        let current = self.chat().map(|c| AppChoice::Chat(c.id));
        let mut choices = vec![Choice::new(
            AppChoice::NewSession,
            "+ New session",
            "ctrl-n",
        )];
        for chat in self.chats_on(node_id) {
            let state = match &chat.activity {
                Activity::Working { .. } => "open · working",
                Activity::Idle => "open",
            };
            let detail = match chat.settings.session_id.as_str() {
                "" => state.to_string(),
                id => format!("{state} · {id}"),
            };
            choices.push(Choice::new(AppChoice::Chat(chat.id), chat.title(), detail));
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
                AppChoice::Saved(session.clone()),
                label,
                detail.join(" · "),
            ));
        }
        let mut picker = Picker::new(SESSIONS, choices, current.as_ref()).with_filter(&filter);
        picker.loading = loading;
        self.picker = Some(picker);
    }

    /// Does what the app's picker offered.
    fn choose(&mut self, choice: AppChoice) -> Vec<Action> {
        match choice {
            AppChoice::NewSession => self.handle(Action::NewChat),
            AppChoice::Chat(id) => {
                self.show(id);
                Vec::new()
            }
            AppChoice::Saved(saved) => self.resume(saved),
            AppChoice::Harness { node, harness } => self.start_harness(node, harness),
        }
    }

    /// Opens a chat on a session the node saved, as it was last used.
    fn resume(&mut self, saved: AgentSession) -> Vec<Action> {
        let Some(chat) = self.chat() else {
            return Vec::new();
        };
        let node = chat.node.clone();
        let fetch = Action::FetchHistory {
            chat: self.next_id,
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
        let mut actions = self.new_chat(node, settings);
        if let Some(chat) = self.chat_mut() {
            chat.title = saved.title;
            chat.spent = saved.cost;
            chat.loading_history = true;
        }
        actions.push(fetch);
        actions
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

    /// Takes in what a background task reports, which may call for actions.
    pub fn on_update(&mut self, update: Update) -> Vec<Action> {
        match update {
            Update::Chat(id, message) => {
                let shown = self.screen == Screen::Chat(id);
                let Some(chat) = self.chats.iter_mut().find(|c| c.id == id) else {
                    return Vec::new();
                };
                let action = chat.on_message(message);
                if shown {
                    chat.unseen = None;
                }
                // Signing in asks for the options again, maybe while they're being asked for.
                action.and_then(|a| self.ask_once(a)).into_iter().collect()
            }
            Update::Options(node, Err(failure)) if failure.transient => {
                // Busy starting its agent, say: ask again before saying so.
                let tries = self.retries.entry(node.clone()).or_default();
                if *tries < OPTIONS_RETRIES {
                    *tries += 1;
                    return vec![Action::FetchOptions {
                        node,
                        after: OPTIONS_RETRY,
                    }];
                }
                self.on_update(Update::Options(
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
                    return vec![Action::FetchOptions {
                        node,
                        after: OPTIONS_RETRY,
                    }];
                }
                self.fetching.remove(&node);
                Vec::new()
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
                if let Some(chat) = self.chats.iter_mut().find(|c| c.id == chat) {
                    chat.mcp_switched(&name, &switched);
                }
                Vec::new()
            }
            Update::Nodes(nodes) if nodes == self.nodes => Vec::new(),
            Update::Nodes(nodes) => {
                let selected = self.nodes.get(self.selected).map(|n| n.id.clone());
                for chat in &mut self.chats {
                    if let Some(node) = nodes.iter().find(|n| n.id == chat.node.id) {
                        chat.on_message(Message::Node(node.clone()));
                    }
                }
                self.nodes = nodes;
                // Keep the same node highlighted.
                if let Some(at) = selected.and_then(|id| self.nodes.iter().position(|n| n.id == id))
                {
                    self.selected = at;
                }
                self.selected = self.selected.min(self.nodes.len().saturating_sub(1));
                Vec::new()
            }
            Update::HarnessStarted(id, started) => {
                self.starting.remove(&id);
                let name = self.node_name(&id);
                match started {
                    Ok(node) => {
                        let hosts = node.harnesses.join(", ");
                        if let Some(known) = self.nodes.iter_mut().find(|n| n.id == id) {
                            *known = node;
                        }
                        // What its chats knew came from an agent that's gone.
                        self.options.remove(&id);
                        let mut actions = Vec::new();
                        if self.chats_on(&id).next().is_some() {
                            actions.extend(self.ask_once(Action::fetch_options(&id)));
                        }
                        // Still looking at it: open it.
                        let selected = self.nodes.get(self.selected).map(|n| n.id.as_str());
                        if self.screen == Screen::Nodes && selected == Some(id.as_str()) {
                            actions.extend(self.open_selected_node());
                            return actions;
                        }
                        self.notice = format!("{name} now hosts {hosts}");
                        return actions;
                    }
                    Err(e) => self.notice = format!("couldn't start an agent on {name}: {e}"),
                }
                Vec::new()
            }
            Update::Sessions(node, sessions) => {
                match sessions {
                    Ok(sessions) => {
                        self.saved.insert(node.clone(), sessions);
                    }
                    Err(e) => {
                        // Only worth saying to someone looking for them.
                        if self.picker.is_some()
                            && let Some(chat) = self.chat_mut()
                        {
                            chat.info(&format!("couldn't list the node's sessions: {e}"));
                        }
                    }
                }
                let picking = self.picker.as_ref().is_some_and(|p| p.title == SESSIONS);
                if picking && self.chat().is_some_and(|c| c.node.id == node) {
                    self.open_session_picker(&node, false);
                }
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::tui::chat::Unseen;
    use crate::tui::chat::tests::started;

    pub(crate) fn node(id: &str, online: bool) -> NodeInfo {
        NodeInfo {
            id: id.into(),
            name: format!("box-{id}"),
            online,
            harnesses: vec!["opencode".into()],
            ..Default::default()
        }
    }

    pub(crate) fn app() -> App {
        let nodes = vec![node("n1", true), node("n2", true), node("n3", false)];
        App::new(nodes, Settings::default())
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
    }

    /// Sends `text` from the chat on screen and returns where it went.
    fn send(app: &mut App, text: &str) -> (ChatId, PromptRequest) {
        type_text(app, text);
        let mut actions = app.on_key(key(KeyCode::Enter));
        match actions.pop() {
            Some(Action::Send(id, request)) => (id, request),
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

    #[test]
    fn nodes_are_chosen_from_a_list() {
        let mut app = app();
        assert_eq!(app.screen, Screen::Nodes);
        app.on_key(key(KeyCode::Down));
        let actions = app.on_key(key(KeyCode::Enter));
        // It asks the node for its sessions and options.
        assert!(
            matches!(&actions[..], [Action::FetchSessions(a), Action::FetchOptions { node: b, .. }] if a == "n2" && b == "n2")
        );
        assert_eq!(app.chat().unwrap().node.id, "n2");

        // An offline node with no chat yet can't be opened.
        app.on_key(ctrl('g'));
        assert_eq!(app.screen, Screen::Nodes);
        app.on_key(key(KeyCode::Down));
        assert!(app.on_key(key(KeyCode::Enter)).is_empty());
        assert_eq!(app.notice, "box-n3 is offline");

        // Going back to a node shows its chat again.
        app.on_key(key(KeyCode::Up));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.chats.len(), 1);
        assert_eq!(app.screen, Screen::Chat(app.chats[0].id));
    }

    #[test]
    fn sessions_on_a_node_run_side_by_side() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        let (first, request) = send(&mut app, "fix the parser");
        assert_eq!(request.node, "n1");

        // A second session starts while the first is still working.
        app.on_key(ctrl('n'));
        let (second, request) = send(&mut app, "write the docs");
        assert_ne!(first, second);
        assert_eq!(request.session_id, "");
        assert_eq!(app.chats_on("n1").count(), 2);

        // The first finishes in the background, and says so in its tab.
        app.on_update(Update::Chat(first, finished("ses_a")));
        let chat = |app: &App, id| app.chats.iter().find(|c| c.id == id).unwrap().unseen;
        assert_eq!(chat(&app, first), Some(Unseen::Done));
        app.on_update(Update::Chat(second, finished("ses_b")));
        assert_eq!(chat(&app, second), None, "the shown chat needs no mark");

        // Switching to it clears the mark, and it continues its own session.
        app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(app.screen, Screen::Chat(first));
        assert_eq!(chat(&app, first), None);
        let (id, request) = send(&mut app, "and the lexer");
        assert_eq!((id, request.session_id.as_str()), (first, "ses_a"));
        assert_eq!(app.running_tasks().len(), 0, "no task id until started");
    }

    #[test]
    fn saved_sessions_are_resumed_from_the_picker() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        app.on_update(Update::Sessions(
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
        let actions = app.on_key(ctrl('o'));
        assert!(matches!(&actions[..], [Action::FetchSessions(n)] if n == "n1"));
        let picker = app.picker.as_ref().expect("ctrl-o opens the sessions");
        // New session, the open chat, the saved one.
        assert_eq!(picker.total(), 3);

        type_text(&mut app, "store");
        let actions = app.on_key(key(KeyCode::Enter));
        let Some(Action::FetchHistory {
            chat: resumed,
            session_id,
            ..
        }) = actions.last()
        else {
            panic!("resuming asks for the session's earlier messages");
        };
        assert_eq!((*resumed, session_id.as_str()), (shown(&app), "ses_old"));
        assert!(app.busy(), "loading them shows a spinner");
        // What was said since resuming stays after them.
        let resumed = *resumed;
        app.on_update(Update::Chat(resumed, started("t1")));
        app.on_update(Update::Chat(
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
        app.on_update(Update::Chat(resumed, Message::History(history)));
        let chat = app.chat().unwrap();
        assert!(!chat.loading_history);
        let thread: Vec<_> = chat
            .thread
            .iter()
            .map(|e| (e.role, e.text.as_str()))
            .collect();
        assert_eq!(
            thread,
            [
                (chat::Role::User, "old question"),
                (chat::Role::Agent, "old answer"),
                (chat::Role::Agent, "new"),
            ]
        );
        let chat = app.chat().unwrap();
        assert_eq!(chat.title, "refactor the store");
        assert_eq!(chat.settings.session_id, "ses_old");
        assert_eq!(chat.settings.cwd, "/src/app");
        assert_eq!(
            (chat.settings.agent.as_str(), chat.settings.model.as_str()),
            ("plan", "a/smart")
        );
        assert_eq!(app.chats.len(), 2);

        // An open session isn't offered twice.
        app.on_key(ctrl('o'));
        assert_eq!(app.picker.as_ref().unwrap().total(), 3);
    }

    #[test]
    fn a_working_chat_stays_open() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        let first = app.chat().unwrap().id;
        send(&mut app, "hi");
        app.on_key(ctrl('w'));
        assert_eq!(app.screen, Screen::Chat(first));

        app.on_key(ctrl('n'));
        type_text(&mut app, "/close");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.screen, Screen::Chat(first));
        assert_eq!(app.chats.len(), 1);

        app.on_update(Update::Chat(first, finished("ses_a")));
        app.on_key(ctrl('w'));
        assert_eq!(app.screen, Screen::Nodes);
        assert!(app.chats.is_empty());
    }

    #[test]
    fn options_reach_every_chat_on_the_node() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        app.on_key(ctrl('n'));
        let options = AgentOptions {
            default_agent: "build".into(),
            ..Default::default()
        };
        app.on_update(Update::Options("n1".into(), Ok(options)));
        assert!(app.chats.iter().all(|c| c.agent() == "build"));
        // A later chat starts with them, without asking again.
        assert!(app.on_key(ctrl('n')).is_empty());
        assert_eq!(app.chat().unwrap().agent(), "build");
    }

    fn shown(app: &App) -> ChatId {
        app.chat().expect("a chat is shown").id
    }

    #[test]
    fn closing_a_chat_shows_its_neighbour() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        let first = shown(&app);
        app.on_key(ctrl('n'));
        let middle = shown(&app);
        app.on_key(ctrl('n'));
        let last = shown(&app);

        // The middle one gives way to the one after it...
        app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(shown(&app), middle);
        app.on_key(ctrl('w'));
        assert_eq!(shown(&app), last);
        // ...the last one to the one before.
        app.on_key(ctrl('w'));
        assert_eq!(shown(&app), first);
        // Updates for closed chats are dropped.
        assert!(
            app.on_update(Update::Chat(middle, started("t1")))
                .is_empty()
        );
        assert!(app.running_tasks().is_empty());
    }

    #[test]
    fn switching_wraps_round_and_stays_on_the_node() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        let first = shown(&app);
        // Alone, it stays put.
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT));
        assert_eq!(shown(&app), first);

        // A chat on another node isn't among them.
        app.on_key(ctrl('g'));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Enter));
        let elsewhere = shown(&app);
        app.on_key(ctrl('g'));
        app.on_key(key(KeyCode::Up));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(shown(&app), first, "the node's last chat comes back");
        app.on_key(ctrl('n'));
        let second = shown(&app);
        for expected in [first, second, first] {
            app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT));
            assert_eq!(shown(&app), expected);
            assert_ne!(shown(&app), elsewhere);
        }
    }

    #[test]
    fn quitting_cancels_the_tasks_on_every_node() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        let (first, _) = send(&mut app, "one");
        app.on_key(ctrl('g'));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Enter));
        let (second, _) = send(&mut app, "two");
        app.on_update(Update::Chat(first, started("t1")));
        app.on_update(Update::Chat(second, started("t2")));
        let mut running = app.running_tasks();
        running.sort();
        assert_eq!(running, ["t1", "t2"]);
        assert!(matches!(app.on_key(ctrl('c'))[..], [Action::Quit]));
        app.on_key(ctrl('g'));
        assert!(matches!(
            app.on_key(key(KeyCode::Char('q')))[..],
            [Action::Quit]
        ));
    }

    #[test]
    fn esc_before_a_background_task_starts_still_cancels_it() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        let (id, _) = send(&mut app, "hi");
        assert!(app.on_key(key(KeyCode::Esc)).is_empty());
        // Even if the chat is no longer shown when the task starts.
        app.on_key(ctrl('n'));
        let actions = app.on_update(Update::Chat(id, started("t1")));
        assert!(matches!(&actions[..], [Action::Cancel(t)] if t == "t1"));
    }

    #[test]
    fn the_node_list_follows_the_nodes() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        let id = shown(&app);
        app.on_key(ctrl('g'));
        app.on_key(key(KeyCode::Down));
        // Reordered, n2 stays highlighted; n1 going offline reaches its chat.
        let mut offline = node("n1", false);
        offline.version = "0.2.0".into();
        app.on_update(Update::Nodes(vec![node("n2", true), offline]));
        assert_eq!(app.nodes[app.selected].id, "n2");
        let chat = app.chats.iter().find(|c| c.id == id).unwrap();
        assert!(!chat.node.online);
        assert_eq!(chat.node.version, "0.2.0");

        // A shrinking list keeps the highlight on it, and an empty one is inert.
        app.on_key(key(KeyCode::Down));
        app.on_update(Update::Nodes(vec![node("n2", true)]));
        assert_eq!(app.selected, 0);
        app.on_update(Update::Nodes(Vec::new()));
        assert!(app.on_key(key(KeyCode::Enter)).is_empty());
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.selected, 0);

        // Its chat is still there, offline, when the node comes back.
        app.on_update(Update::Nodes(vec![node("n1", true)]));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(shown(&app), id);
    }

    #[test]
    fn an_old_worker_without_an_agent_isnt_opened() {
        let mut bare = node("n9", true);
        bare.harnesses.clear();
        let mut app = App::new(vec![bare], Settings::default());
        assert!(app.on_key(key(KeyCode::Enter)).is_empty());
        assert_eq!(app.screen, Screen::Nodes);
        assert!(app.picker.is_none());
        assert!(
            app.notice.contains("can't start an agent"),
            "{}",
            app.notice
        );
        // Pasting on the node list does nothing.
        assert!(app.on_input(Event::Paste("x".into())).is_empty());
    }

    #[test]
    fn the_session_picker_keeps_up_with_the_node() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        let saved = |id: &str| AgentSession {
            id: id.into(),
            title: format!("about {id}"),
            ..Default::default()
        };
        // Failing to list sessions isn't news unless someone is looking.
        app.on_update(Update::Sessions("n1".into(), Err("offline".into())));
        assert!(app.chat().unwrap().thread.is_empty());

        app.on_key(ctrl('o'));
        type_text(&mut app, "about");
        assert_eq!(app.picker.as_ref().unwrap().shown_len(), 0);
        app.on_update(Update::Sessions(
            "n1".into(),
            Ok(vec![saved("a"), saved("b")]),
        ));
        let picker = app.picker.as_ref().unwrap();
        assert_eq!((picker.filter.as_str(), picker.shown_len()), ("about", 2));
        // Another node's sessions don't land in it.
        app.on_update(Update::Sessions("n2".into(), Ok(vec![saved("z")])));
        assert_eq!(app.picker.as_ref().unwrap().total(), 4);

        app.on_update(Update::Sessions("n1".into(), Err("offline".into())));
        let last = &app.chat().unwrap().thread.last().unwrap().text;
        assert!(last.contains("couldn't list the node's sessions"), "{last}");

        // "+ New session" is a new chat.
        app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        app.on_key(key(KeyCode::Up));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.chats.len(), 2);
    }

    #[test]
    fn app_keys_wait_while_a_chat_picker_is_open() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        app.on_update(Update::Options(
            "n1".into(),
            Ok(AgentOptions {
                agents: vec![AgentChoice {
                    name: "build".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
        ));
        type_text(&mut app, "/agent");
        app.on_key(key(KeyCode::Enter));
        assert!(app.chat().unwrap().picker.is_some());
        for keys in [ctrl('n'), ctrl('o'), ctrl('g'), ctrl('w')] {
            app.on_key(keys);
        }
        assert_eq!(app.chats.len(), 1);
        assert!(app.picker.is_none());
        assert!(matches!(app.screen, Screen::Chat(_)));
    }

    #[test]
    fn a_node_is_asked_for_its_options_once_at_a_time() {
        let mut app = app();
        let fetches = |actions: Vec<Action>| {
            actions
                .iter()
                .filter(|a| matches!(a, Action::FetchOptions { .. }))
                .count()
        };
        assert_eq!(fetches(app.on_key(key(KeyCode::Enter))), 1);
        let first = shown(&app);
        // A second chat waits for the same answer.
        assert_eq!(fetches(app.on_key(ctrl('n'))), 0);
        let second = shown(&app);
        app.on_update(Update::Options("n1".into(), Err("timed out".into())));

        // Both were waiting, so both hear it failed, once each.
        let errors = |app: &App, id| {
            let chat = app.chats.iter().find(|c| c.id == id).unwrap();
            chat.thread
                .iter()
                .filter(|e| e.text.contains("timed out"))
                .count()
        };
        assert_eq!((errors(&app, first), errors(&app, second)), (1, 1));
        // Once answered, it can be asked again, but not twice at once.
        assert_eq!(fetches(app.on_key(key(KeyCode::Tab))), 1);
        app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(fetches(app.on_key(key(KeyCode::Tab))), 0);
    }

    pub(crate) fn bare(id: &str) -> NodeInfo {
        NodeInfo {
            harnesses: Vec::new(),
            can_host: vec!["opencode".into()],
            ..node(id, true)
        }
    }

    #[test]
    fn a_node_without_an_agent_offers_to_start_one() {
        let mut app = App::new(vec![bare("n1")], Settings::default());
        assert!(app.on_key(key(KeyCode::Enter)).is_empty());
        let picker = app.picker.as_ref().expect("a harness to choose");
        assert_eq!(
            picker.shown().next().unwrap().value,
            AppChoice::Harness {
                node: "n1".into(),
                harness: "opencode".into()
            }
        );
        assert!(!picker.shown().next().unwrap().detail.is_empty());

        // Esc backs out, and nothing starts.
        app.on_key(key(KeyCode::Esc));
        assert!(app.picker.is_none());
        assert!(app.starting.is_empty());

        app.on_key(key(KeyCode::Enter));
        let actions = app.on_key(key(KeyCode::Enter));
        assert!(matches!(&actions[..],
            [Action::StartHarness { node, harness }] if node == "n1" && harness == "opencode"));
        assert!(app.starting.contains("n1"));
        assert!(app.notice.contains("starting opencode"), "{}", app.notice);
        // Asking again while it starts doesn't start another.
        assert!(app.on_key(key(KeyCode::Enter)).is_empty());
        assert!(app.picker.is_none());
        assert!(app.notice.contains("still starting"), "{}", app.notice);

        // Once it hosts it, the node opens, as it is still the one in view.
        let ready = NodeInfo {
            harnesses: vec!["opencode".into()],
            ..bare("n1")
        };
        let actions = app.on_update(Update::HarnessStarted("n1".into(), Ok(ready)));
        assert!(matches!(
            &actions[..],
            [Action::FetchSessions(_), Action::FetchOptions { .. }]
        ));
        assert!(app.starting.is_empty());
        assert_eq!(app.chat().unwrap().node.harnesses, ["opencode"]);
    }

    #[test]
    fn a_harness_that_fails_or_finishes_out_of_view_is_reported() {
        let mut app = App::new(vec![bare("n1"), bare("n2")], Settings::default());
        app.on_key(key(KeyCode::Enter));
        app.on_key(key(KeyCode::Enter));
        let failed = Update::HarnessStarted("n1".into(), Err("no curl on the node".into()));
        assert!(app.on_update(failed).is_empty());
        assert!(
            app.notice
                .contains("couldn't start an agent on box-n1: no curl"),
            "{}",
            app.notice
        );
        assert!(app.starting.is_empty(), "it can be tried again");

        // Started while another node is highlighted: say so, don't jump.
        app.on_key(key(KeyCode::Enter));
        app.on_key(key(KeyCode::Enter));
        app.on_key(key(KeyCode::Down));
        let ready = NodeInfo {
            harnesses: vec!["opencode".into()],
            ..bare("n1")
        };
        assert!(
            app.on_update(Update::HarnessStarted("n1".into(), Ok(ready)))
                .is_empty()
        );
        assert_eq!(app.screen, Screen::Nodes);
        assert_eq!(app.notice, "box-n1 now hosts opencode");
        assert_eq!(app.nodes[0].harnesses, ["opencode"]);
    }

    #[test]
    fn commands_that_are_still_loading_are_asked_for_again() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        let loading = AgentOptions {
            default_agent: "build".into(),
            loading: true,
            ..Default::default()
        };
        let actions = app.on_update(Update::Options("n1".into(), Ok(loading)));
        assert!(matches!(&actions[..],
            [Action::FetchOptions { node, after }] if node == "n1" && !after.is_zero()));
        // The rest is usable already.
        assert_eq!(app.chat().unwrap().agent(), "build");

        // Meanwhile a command waits, rather than going out as text.
        type_text(&mut app, "/review the parser");
        assert!(app.on_key(key(KeyCode::Enter)).is_empty());
        assert_eq!(app.chat().unwrap().input.text, "/review the parser");
        let said = &app.chat().unwrap().thread.last().unwrap().text;
        assert!(said.contains("still loading"), "{said}");
        // Asking again in the meantime doesn't add a request.
        assert!(
            app.on_key(key(KeyCode::Tab))
                .iter()
                .all(|a| !matches!(a, Action::FetchOptions { .. }))
        );

        let loaded = AgentOptions {
            default_agent: "build".into(),
            commands: vec![AgentCommand {
                name: "review".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(
            app.on_update(Update::Options("n1".into(), Ok(loaded)))
                .is_empty()
        );
        let (_, request) = match app.on_key(key(KeyCode::Enter)).pop() {
            Some(Action::Send(id, request)) => (id, request),
            _ => panic!("sent once the commands are known"),
        };
        assert_eq!(
            (request.command.as_str(), request.prompt.as_str()),
            ("review", "the parser")
        );
    }

    #[test]
    fn an_mcp_switch_answers_its_chat_and_leaves_other_requests_alone() {
        let mut app = app();
        // Opening the node asks for its options; that request is open.
        app.on_key(key(KeyCode::Enter));
        let asking = shown(&app);
        app.on_key(ctrl('n'));
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
        assert!(app.on_update(update).is_empty());
        // Every chat on the node has the new options...
        assert!(app.chats.iter().all(|c| c.agent() == "plan"));
        // ...the open request is still open...
        assert!(app.fetching.contains("n1"));
        // ...and only the chat that switched hears how it went.
        let failed = Update::McpSwitched {
            chat: asking,
            node: "n1".into(),
            name: "docs".into(),
            options: Err("timed out".into()),
        };
        app.on_update(failed);
        let said = |app: &App, id| {
            let chat = app.chats.iter().find(|c| c.id == id).unwrap();
            chat.thread
                .iter()
                .any(|e| e.text.contains("couldn't switch docs"))
        };
        let other = app.chats.iter().find(|c| c.id != asking).unwrap().id;
        assert!(said(&app, asking));
        assert!(!said(&app, other));
    }

    #[test]
    fn the_screen_only_ticks_while_something_moves() {
        let mut app = app();
        assert!(!app.busy());
        app.on_key(key(KeyCode::Enter));
        assert!(app.busy(), "asking for the options shows a spinner");
        app.on_update(Update::Options("n1".into(), Ok(AgentOptions::default())));
        let (id, _) = send(&mut app, "hi");
        assert!(app.busy(), "a working chat's spinner turns");
        app.on_update(Update::Chat(id, finished("ses_a")));
        assert!(!app.busy());
        app.starting.insert("n2".into());
        assert!(app.busy(), "so does a node starting its agent");
    }

    #[test]
    fn a_node_that_lost_its_agent_offers_one_again_then_refreshes_its_chats() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        let chat = shown(&app);
        app.on_update(Update::Options("n1".into(), Ok(AgentOptions::default())));
        app.on_key(ctrl('g'));

        // Its worker restarted without --harness.
        let mut nodes = app.nodes.clone();
        nodes[0] = bare("n1");
        app.on_update(Update::Nodes(nodes));
        assert!(app.on_key(key(KeyCode::Enter)).is_empty());
        assert!(app.picker.is_some(), "the agent picker, not the stale chat");
        app.on_key(key(KeyCode::Enter));

        let ready = NodeInfo {
            harnesses: vec!["opencode".into()],
            ..bare("n1")
        };
        let actions = app.on_update(Update::HarnessStarted("n1".into(), Ok(ready)));
        // Back in its chat, asking the new agent what it offers.
        assert_eq!(app.screen, Screen::Chat(chat));
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::FetchOptions { node, .. } if node == "n1"))
        );
    }

    #[test]
    fn a_node_that_may_answer_in_a_moment_is_asked_again_quietly() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        let timed_out = || {
            let failure = Failure {
                message: "node box-n1 didn't say what its agent offers".into(),
                transient: true,
            };
            Update::Options("n1".into(), Err(failure))
        };
        let errors = |app: &App| {
            let thread = &app.chat().unwrap().thread;
            thread
                .iter()
                .filter(|e| e.role == chat::Role::Error)
                .count()
        };
        // Twice it asks again, saying nothing...
        for _ in 0..OPTIONS_RETRIES {
            let actions = app.on_update(timed_out());
            assert!(
                matches!(&actions[..], [Action::FetchOptions { after, .. }] if !after.is_zero())
            );
            assert_eq!(errors(&app), 0);
        }
        // ...then it says why, and stops asking.
        assert!(app.on_update(timed_out()).is_empty());
        assert_eq!(errors(&app), 1);
        assert!(!app.fetching.contains("n1"));

        // An answer resets the count; a refusal is shown at once.
        app.on_update(Update::Options("n1".into(), Ok(AgentOptions::default())));
        assert!(!app.retries.contains_key("n1"));
        app.on_key(ctrl('n'));
        let refused = Update::Options("n1".into(), Err("node box-n1 runs no agent harness".into()));
        assert!(app.on_update(refused).is_empty());
    }
}
