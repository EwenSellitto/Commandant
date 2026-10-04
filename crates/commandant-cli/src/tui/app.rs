//! The whole UI's state: the nodes, and every chat open on them. Chats on
//! any node run side by side; one is shown at a time.

use std::collections::HashMap;
use std::sync::Arc;

use commandant_common::time::ago;
use commandant_proto::*;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::chat::{self, Activity, Chat, Message, Settings};
use super::picker::{Choice, Outcome, Pick, Picker};

/// Tells chats apart for as long as the UI runs.
pub type ChatId = u64;

/// What the session picker offers besides the chats and saved sessions.
const NEW_SESSION: &str = "new";

/// Something for the event loop to do, or (the chat-level ones) for the app.
pub enum Action {
    Send(ChatId, PromptRequest),
    Cancel(String),
    /// Ask a node what its agent offers.
    FetchOptions(String),
    /// Ask a node which sessions its agent has saved.
    FetchSessions(String),
    /// Connect (or disconnect) one of a node's MCP servers.
    SwitchMcp {
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
    Quit,
}

/// What background tasks report to the UI.
pub enum Update {
    Chat(ChatId, Message),
    /// For every chat on the node.
    Options(String, Result<AgentOptions, String>),
    Nodes(Vec<NodeInfo>),
    Sessions(String, Result<Vec<AgentSession>, String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Nodes,
    Chat(ChatId),
}

pub struct App {
    pub screen: Screen,
    pub nodes: Vec<NodeInfo>,
    /// The highlighted row of the node list.
    pub selected: usize,
    /// Every open chat, on every node, oldest first.
    pub chats: Vec<Chat>,
    /// What a node's agent offers, once it has said; shared by its chats.
    options: HashMap<String, Arc<AgentOptions>>,
    /// The sessions each node has saved, as last listed.
    pub saved: HashMap<String, Vec<AgentSession>>,
    /// The chat last shown on each node.
    last: HashMap<String, ChatId>,
    /// The session picker, when open; a chat's own pickers live in the chat.
    pub picker: Option<Picker>,
    /// A remark on the node screen.
    pub notice: String,
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
            screen: Screen::Nodes,
            nodes,
            selected,
            chats: Vec::new(),
            options: HashMap::new(),
            saved: HashMap::new(),
            last: HashMap::new(),
            picker: None,
            notice: String::new(),
            defaults,
            next_id: 1,
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
        if self.screen == Screen::Nodes {
            return self.on_node_key(key);
        }
        if let Some(picker) = &mut self.picker {
            match picker.on_key(key) {
                Outcome::Open => {}
                Outcome::Closed => self.picker = None,
                Outcome::Chosen(value) => {
                    self.picker = None;
                    return self.choose_session(&value);
                }
            }
            return Vec::new();
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
                self.open_session_picker(&node.id);
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
            action => vec![action],
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
        let mut actions = Vec::new();
        if node.online && !node.harnesses.is_empty() {
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
        if node.harnesses.is_empty() {
            self.notice = format!(
                "{} runs no agent harness; start its worker with --harness opencode",
                node.name
            );
            return Vec::new();
        }
        if !node.online {
            self.notice = format!("{} is offline", node.name);
            return Vec::new();
        }
        let settings = self.defaults.clone();
        actions.extend(self.new_chat(node, settings));
        actions
    }

    /// Starts a chat and shows it; it asks for the options the node hasn't given.
    fn new_chat(&mut self, node: NodeInfo, settings: Settings) -> Vec<Action> {
        let id = self.next_id;
        self.next_id += 1;
        let options = self.options.get(&node.id).cloned();
        let fetch = options
            .is_none()
            .then(|| Action::FetchOptions(node.id.clone()));
        self.chats.push(Chat::new(id, node, settings, options));
        self.show(id);
        fetch.into_iter().collect()
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
        let ids: Vec<String> = self
            .chats_on(&chat.node.id)
            .map(|c| c.id.to_string())
            .collect();
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        if let Some(next) = chat::cycle(&ids, &chat.id.to_string(), step) {
            let next = next.parse().expect("an id");
            self.show(next);
        }
        Vec::new()
    }

    /// The node's open chats, then the sessions it saved that aren't open.
    fn open_session_picker(&mut self, node_id: &str) {
        let filter = self.picker.take().map(|p| p.filter).unwrap_or_default();
        let current = self.chat().map(|c| c.id.to_string()).unwrap_or_default();
        let mut choices = vec![Choice::new(NEW_SESSION, "+ New session", "ctrl-n")];
        for chat in self.chats_on(node_id) {
            let state = match &chat.activity {
                Activity::Working { .. } => "open · working",
                Activity::Idle => "open",
            };
            let detail = match chat.settings.session_id.as_str() {
                "" => state.to_string(),
                id => format!("{state} · {id}"),
            };
            choices.push(Choice::new(chat.id.to_string(), title(chat), detail));
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
            let label = match session.title.as_str() {
                "" => session.id.clone(),
                title => title.to_string(),
            };
            choices.push(Choice::new(
                format!("ses:{}", session.id),
                label,
                detail.join(" · "),
            ));
        }
        let mut picker = Picker::new(Pick::Session, choices, &current);
        for c in filter.chars() {
            picker.on_key(KeyEvent::from(KeyCode::Char(c)));
        }
        self.picker = Some(picker);
    }

    /// Shows an open chat, starts a new one, or resumes a saved session.
    fn choose_session(&mut self, value: &str) -> Vec<Action> {
        let Some(chat) = self.chat() else {
            return Vec::new();
        };
        let node = chat.node.clone();
        if value == NEW_SESSION {
            return self.handle(Action::NewChat);
        }
        let Some(session_id) = value.strip_prefix("ses:") else {
            if let Ok(id) = value.parse() {
                self.show(id);
            }
            return Vec::new();
        };
        let saved = self
            .saved
            .get(&node.id)
            .and_then(|all| all.iter().find(|s| s.id == session_id))
            .cloned()
            .unwrap_or_default();
        let settings = Settings {
            session_id: session_id.to_string(),
            cwd: saved.directory,
            model: saved.model,
            agent: saved.agent,
            effort: saved.variant,
        };
        let actions = self.new_chat(node, settings);
        if let Some(chat) = self.chat_mut() {
            chat.title = saved.title;
            chat.spent = saved.cost;
            chat.info("continuing this session; its earlier messages aren't shown here");
        }
        actions
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
                action.into_iter().collect()
            }
            Update::Options(node, options) => {
                if let Ok(options) = &options {
                    self.options.insert(node.clone(), Arc::new(options.clone()));
                }
                for chat in self.chats.iter_mut().filter(|c| c.node.id == node) {
                    chat.on_message(Message::Options(options.clone()));
                }
                Vec::new()
            }
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
                let picking = self
                    .picker
                    .as_ref()
                    .is_some_and(|p| p.pick == Pick::Session);
                if picking && self.chat().is_some_and(|c| c.node.id == node) {
                    self.open_session_picker(&node);
                }
                Vec::new()
            }
        }
    }
}

/// What a chat is called in its tab and the session picker.
pub fn title(chat: &Chat) -> String {
    match chat.title.as_str() {
        "" => "new session".to_string(),
        title => title.to_string(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::tui::chat::Unseen;

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
            matches!(&actions[..], [Action::FetchSessions(a), Action::FetchOptions(b)] if a == "n2" && b == "n2")
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
        app.on_key(key(KeyCode::Enter));
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
}
