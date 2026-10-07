//! The terminal's side of the client: which screen is shown, the
//! highlighted node, each chat's prompt and picker as shown, and what keys
//! ask of the [`State`].

use std::collections::HashMap;

use commandant_proto::*;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::chat::ChatView;
use super::picker::{Outcome as Picked, Picker};
use crate::state::chat::{Chat, Settings, cycle};
use crate::state::{ChatId, Effect, Go, Intent, Outcome, Scope, State, Update};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Screen {
    #[default]
    Nodes,
    Chat(ChatId),
}

pub struct App {
    pub state: State,
    pub screen: Screen,
    /// The highlighted row of the node list.
    pub selected: usize,
    /// The chat last shown on each node.
    last: HashMap<String, ChatId>,
    /// What the terminal keeps of each chat, kept in step with the state's
    /// chats by [`sync`](Self::sync).
    pub views: HashMap<ChatId, ChatView>,
    /// The app's picker, as shown.
    pub picker: Option<Picker>,
    /// Set once the person has asked to leave.
    pub quit: bool,
}

impl App {
    pub fn new(nodes: Vec<NodeInfo>, defaults: Settings) -> Self {
        // Start on the first node that can take a prompt.
        let selected = nodes
            .iter()
            .position(|n| n.online && !n.harnesses.is_empty())
            .unwrap_or(0);
        Self {
            state: State::new(nodes, defaults),
            screen: Screen::default(),
            selected,
            last: HashMap::new(),
            views: HashMap::new(),
            picker: None,
            quit: false,
        }
    }

    /// The chat on screen, if any.
    pub fn chat(&self) -> Option<&Chat> {
        match self.screen {
            Screen::Chat(id) => self.state.chat(id),
            Screen::Nodes => None,
        }
    }

    /// What the terminal keeps of a chat.
    pub fn view(&mut self, id: ChatId) -> &mut ChatView {
        self.views.entry(id).or_default()
    }

    /// The chat on screen, and what the terminal keeps of it.
    pub fn shown(&mut self) -> Option<(&Chat, &mut ChatView)> {
        let Screen::Chat(id) = self.screen else {
            return None;
        };
        let chat = self.state.chats.iter().find(|c| c.id == id)?;
        Some((chat, self.views.entry(id).or_default()))
    }

    /// Opens a chat on `node` with `settings`: the first shown on start.
    pub fn open_node_with(&mut self, node: NodeInfo, settings: Settings) -> Vec<Effect> {
        let outcome = self.state.open_with(node, settings);
        self.apply(outcome)
    }

    pub fn on_input(&mut self, event: Event) -> Vec<Effect> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.on_key(key),
            Event::Paste(text) => {
                if let Some((_, view)) = self.shown() {
                    view.paste(&text);
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        self.sync();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if ctrl && matches!(key.code, KeyCode::Char('c' | 'd')) {
            self.quit = true;
            return Vec::new();
        }
        if let Some(picker) = &mut self.picker {
            let intent = match picker.on_key(key) {
                Picked::Open => return Vec::new(),
                Picked::Closed => Intent::Dismiss(Scope::App),
                Picked::Chosen(choice) => Intent::Choose(Scope::App, choice),
            };
            self.picker = None;
            return self.ask(intent);
        }
        let Some((chat, view)) = self.shown() else {
            return self.on_node_key(key);
        };
        let id = chat.id;
        let intent = match key.code {
            _ if view.picker.is_some() => view.on_key(chat, key),
            KeyCode::Char('n') if ctrl => Some(Intent::NewChat(id)),
            KeyCode::Char('o') if ctrl => Some(Intent::Sessions(id)),
            KeyCode::Char('w') if ctrl => Some(Intent::Close(id)),
            KeyCode::Char('g') if ctrl => {
                self.show_nodes();
                return Vec::new();
            }
            KeyCode::Left if alt => return self.cycle_chat(-1),
            KeyCode::Right if alt => return self.cycle_chat(1),
            _ => view.on_key(chat, key),
        };
        intent.map_or_else(Vec::new, |intent| self.ask(intent))
    }

    fn on_node_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let last = self.state.nodes.len().saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(last),
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Enter => return self.open_selected_node(),
            _ => {}
        }
        Vec::new()
    }

    /// Shows the highlighted node's last chat, or starts one.
    fn open_selected_node(&mut self) -> Vec<Effect> {
        let Some(node) = self.state.nodes.get(self.selected) else {
            return Vec::new();
        };
        let chat = self.last.get(&node.id).copied();
        let node = node.id.clone();
        self.ask(Intent::Open { node, chat })
    }

    /// Shows the next (or previous) chat on the same node.
    fn cycle_chat(&mut self, step: isize) -> Vec<Effect> {
        let Some(chat) = self.chat() else {
            return Vec::new();
        };
        let ids: Vec<ChatId> = self.state.chats_on(&chat.node.id).map(|c| c.id).collect();
        if let Some(next) = cycle(&ids, chat.id, step) {
            self.show(next);
        }
        Vec::new()
    }

    pub fn on_update(&mut self, update: Update) -> Vec<Effect> {
        let selected = self.state.nodes.get(self.selected).map(|n| n.id.clone());
        let outcome = self.state.update(update);
        // Keep the same node highlighted.
        if let Some(at) = selected.and_then(|id| self.state.nodes.iter().position(|n| n.id == id)) {
            self.selected = at;
        }
        self.selected = self.selected.min(self.state.nodes.len().saturating_sub(1));
        let effects = self.apply(outcome);
        // What happens in the chat on screen is seen as it happens.
        if let Screen::Chat(id) = self.screen {
            self.state.intent(Intent::Seen(id));
        }
        effects
    }

    /// Asks the state for something, and follows where that leads.
    fn ask(&mut self, intent: Intent) -> Vec<Effect> {
        let outcome = self.state.intent(intent);
        self.apply(outcome)
    }

    /// Does the terminal's part of an outcome, and returns the calls to make.
    fn apply(&mut self, outcome: Outcome) -> Vec<Effect> {
        let Outcome {
            mut effects,
            go,
            prompt,
        } = outcome;
        if let Some((id, edit)) = prompt {
            self.view(id).edit(edit);
        }
        for effect in &effects {
            // A prompt sent: back to the bottom of its thread.
            if let Effect::Send(id, _) = effect {
                self.view(*id).scroll = 0;
            }
        }
        match go {
            Some(Go::Chat(id)) => self.show(id),
            Some(Go::Nodes) => self.show_nodes(),
            Some(Go::Quit) => self.quit = true,
            // Still looking at it: open it.
            Some(Go::Ready(node)) => {
                let selected = self.state.nodes.get(self.selected).map(|n| &n.id);
                if self.screen == Screen::Nodes && selected == Some(&node) {
                    effects.extend(self.open_selected_node());
                }
            }
            None => {}
        }
        self.sync();
        effects
    }

    fn show(&mut self, id: ChatId) {
        let Some(chat) = self.state.chat(id) else {
            return;
        };
        self.last.insert(chat.node.id.clone(), id);
        self.screen = Screen::Chat(id);
        self.state.intent(Intent::Seen(id));
    }

    fn show_nodes(&mut self) {
        if let Some(chat) = self.chat()
            && let Some(at) = self.state.nodes.iter().position(|n| n.id == chat.node.id)
        {
            self.selected = at;
        }
        self.state.intent(Intent::Dismiss(Scope::App));
        self.screen = Screen::Nodes;
    }

    /// Shows the pickers the state has open, and forgets closed chats.
    pub fn sync(&mut self) {
        Picker::sync(&mut self.picker, self.state.pick(Scope::App));
        let state = &self.state;
        self.views.retain(|id, _| state.chat(*id).is_some());
        for chat in &state.chats {
            let view = self.views.entry(chat.id).or_default();
            Picker::sync(&mut view.picker, chat.pick.as_ref());
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::state::chat::Unseen;
    use crate::state::tests::{bare, node, started};

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

    fn alt(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::ALT)
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
    }

    fn shown(app: &App) -> ChatId {
        app.chat().expect("a chat is shown").id
    }

    fn input(app: &mut App) -> String {
        app.shown().unwrap().1.input.text.clone()
    }

    /// Sends `text` from the chat on screen and returns where it went.
    fn send(app: &mut App, text: &str) -> (ChatId, PromptRequest) {
        type_text(app, text);
        match app.on_key(key(KeyCode::Enter)).pop() {
            Some(Effect::Send(id, request)) => (id, request),
            _ => panic!("{text:?} should be sent"),
        }
    }

    fn finish(app: &mut App, id: ChatId, session_id: &str) {
        let finished = TaskFinished {
            exit_code: Some(0),
            session_id: session_id.into(),
            ..Default::default()
        };
        let message = crate::state::chat::Message::Task(task_event::Event::Finished(finished));
        app.on_update(Update::Chat(id, message));
    }

    fn with_agents(app: &mut App) {
        let agent = |name: &str| AgentChoice {
            name: name.into(),
            ..Default::default()
        };
        let options = AgentOptions {
            default_agent: "build".into(),
            agents: vec![agent("build"), agent("plan")],
            commands: vec![AgentCommand {
                name: "review".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        app.on_update(Update::Options("n1".into(), Ok(options)));
    }

    #[test]
    fn nodes_are_chosen_from_a_list() {
        let mut app = app();
        assert_eq!(app.screen, Screen::Nodes);
        app.on_key(key(KeyCode::Down));
        let effects = app.on_key(key(KeyCode::Enter));
        assert!(matches!(&effects[..], [Effect::FetchSessions(n), _] if n == "n2"));
        assert_eq!(app.chat().unwrap().node.id, "n2");

        // An offline node with no chat yet stays on the list.
        app.on_key(ctrl('g'));
        assert_eq!(app.screen, Screen::Nodes);
        app.on_key(key(KeyCode::Down));
        assert!(app.on_key(key(KeyCode::Enter)).is_empty());
        assert_eq!(app.screen, Screen::Nodes);
        assert_eq!(app.state.notice, "box-n3 is offline");

        // Going back to a node shows its chat again.
        app.on_key(key(KeyCode::Up));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.state.chats.len(), 1);
        assert_eq!(app.screen, Screen::Chat(app.state.chats[0].id));
    }

    #[test]
    fn the_shown_chat_is_seen_and_switching_shows_the_others() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        let (first, _) = send(&mut app, "fix the parser");
        app.on_key(ctrl('n'));
        let (second, request) = send(&mut app, "write the docs");
        assert_ne!(first, second);
        assert_eq!(request.session_id, "");

        // The first finishes in the background, and says so in its tab.
        finish(&mut app, first, "ses_a");
        let unseen = |app: &App, id| app.state.chat(id).unwrap().unseen;
        assert_eq!(unseen(&app, first), Some(Unseen::Done));
        finish(&mut app, second, "ses_b");
        assert_eq!(unseen(&app, second), None, "the shown chat needs no mark");

        // Switching to it clears the mark, and it continues its own session.
        app.on_key(alt(KeyCode::Left));
        assert_eq!(app.screen, Screen::Chat(first));
        assert_eq!(unseen(&app, first), None);
        let (id, request) = send(&mut app, "and the lexer");
        assert_eq!((id, request.session_id.as_str()), (first, "ses_a"));
    }

    #[test]
    fn switching_wraps_round_and_stays_on_the_node() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        let first = shown(&app);
        // Alone, it stays put.
        app.on_key(alt(KeyCode::Right));
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
            app.on_key(alt(KeyCode::Right));
            assert_eq!(shown(&app), expected);
            assert_ne!(shown(&app), elsewhere);
        }

        // Closing shows a neighbour; the last one closed shows the nodes.
        app.on_key(ctrl('w'));
        assert_eq!(shown(&app), second);
        app.on_key(ctrl('w'));
        assert_eq!(app.screen, Screen::Nodes);
        assert_eq!(app.state.nodes[app.selected].id, "n1");
    }

    #[test]
    fn quitting_takes_ctrl_c_or_q_on_the_nodes() {
        let mut working = app();
        working.on_key(key(KeyCode::Enter));
        let (id, _) = send(&mut working, "one");
        working.on_update(Update::Chat(id, started("t1")));
        working.on_key(ctrl('c'));
        assert!(working.quit);

        let mut listing = app();
        listing.on_key(key(KeyCode::Char('q')));
        assert!(listing.quit);

        let mut typing = app();
        typing.on_key(key(KeyCode::Enter));
        type_text(&mut typing, "/quit");
        typing.on_key(key(KeyCode::Enter));
        assert!(typing.quit);
    }

    #[test]
    fn the_node_list_keeps_its_highlight() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        let id = shown(&app);
        app.on_key(ctrl('g'));
        app.on_key(key(KeyCode::Down));
        // Reordered, n2 stays highlighted.
        app.on_update(Update::Nodes(vec![node("n2", true), node("n1", false)]));
        assert_eq!(app.state.nodes[app.selected].id, "n2");

        // A shrinking list keeps the highlight on it, and an empty one is inert.
        app.on_key(key(KeyCode::Down));
        app.on_update(Update::Nodes(vec![node("n2", true)]));
        assert_eq!(app.selected, 0);
        app.on_update(Update::Nodes(Vec::new()));
        assert!(app.on_key(key(KeyCode::Enter)).is_empty());
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.selected, 0);

        // Its chat is still there when the node comes back.
        app.on_update(Update::Nodes(vec![node("n1", true)]));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(shown(&app), id);
        // Pasting on the node list does nothing.
        app.on_key(ctrl('g'));
        assert!(app.on_input(Event::Paste("x".into())).is_empty());
    }

    #[test]
    fn app_keys_wait_while_a_chat_picker_is_open() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        with_agents(&mut app);
        type_text(&mut app, "/agent");
        app.on_key(key(KeyCode::Enter));
        assert!(app.shown().unwrap().1.picker.is_some());
        for keys in [ctrl('n'), ctrl('o'), ctrl('g'), ctrl('w')] {
            app.on_key(keys);
        }
        assert_eq!(app.state.chats.len(), 1);
        assert!(app.picker.is_none());
        assert!(matches!(app.screen, Screen::Chat(_)));

        // Its keys went to its filter; cleared, they move in it and choose.
        assert_eq!(
            app.shown().unwrap().1.picker.as_ref().unwrap().filter,
            "nogw"
        );
        app.on_key(ctrl('u'));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Enter));
        assert!(app.shown().unwrap().1.picker.is_none());
        assert_eq!(app.chat().unwrap().agent(), "plan");
    }

    #[test]
    fn a_picker_keeps_its_filter_as_its_choices_come_in() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        app.on_key(ctrl('o'));
        type_text(&mut app, "about");
        assert_eq!(app.picker.as_ref().unwrap().shown_len(), 0);
        let saved = |id: &str| AgentSession {
            id: id.into(),
            title: format!("about {id}"),
            ..Default::default()
        };
        app.on_update(Update::Sessions(
            "n1".into(),
            Ok(vec![saved("a"), saved("b")]),
        ));
        let picker = app.picker.as_ref().unwrap();
        assert_eq!((picker.filter.as_str(), picker.shown_len()), ("about", 2));
        assert!(!picker.loading());

        // "+ New session" is a new chat.
        app.on_key(ctrl('u'));
        app.on_key(key(KeyCode::Up));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.state.chats.len(), 2);
        assert!(app.picker.is_none());
    }

    #[test]
    fn a_node_starting_its_agent_opens_once_ready_if_still_in_view() {
        let mut app = App::new(vec![bare("n1"), bare("n2")], Settings::default());
        app.on_key(key(KeyCode::Enter));
        assert!(app.picker.is_some(), "the harness to start");
        // Esc backs out, and nothing starts.
        app.on_key(key(KeyCode::Esc));
        assert!(app.picker.is_none());
        assert!(app.state.starting.is_empty());
        app.on_key(key(KeyCode::Enter));
        let effects = app.on_key(key(KeyCode::Enter));
        assert!(matches!(&effects[..], [Effect::StartHarness { .. }]));

        let ready = |id: &str| NodeInfo {
            harnesses: vec!["opencode".into()],
            ..bare(id)
        };
        let effects = app.on_update(Update::HarnessStarted("n1".into(), Ok(ready("n1"))));
        assert!(matches!(
            &effects[..],
            [Effect::FetchSessions(_), Effect::FetchOptions { .. }]
        ));
        assert_eq!(app.chat().unwrap().node.harnesses, ["opencode"]);

        // Started while another node is highlighted: say so, don't jump.
        app.on_key(ctrl('g'));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Enter));
        app.on_key(key(KeyCode::Enter));
        app.on_key(key(KeyCode::Up));
        assert!(
            app.on_update(Update::HarnessStarted("n2".into(), Ok(ready("n2"))))
                .is_empty()
        );
        assert_eq!(app.screen, Screen::Nodes);
        assert_eq!(app.state.notice, "box-n2 now hosts opencode");
    }

    #[test]
    fn slash_commands_complete_as_they_are_typed() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        with_agents(&mut app);
        let suggestions = |app: &mut App| -> Vec<String> {
            let (chat, view) = app.shown().unwrap();
            view.suggestions(chat).into_iter().map(|(n, _)| n).collect()
        };
        type_text(&mut app, "/re");
        assert_eq!(suggestions(&mut app), ["review"]);
        // Tab fills it in, ready for arguments, rather than switching agent.
        app.on_key(key(KeyCode::Tab));
        assert_eq!(input(&mut app), "/review ");
        assert_eq!(app.chat().unwrap().agent(), "build");
        assert!(suggestions(&mut app).is_empty());

        // Arrows pick among several; Enter on a partial name runs the one picked.
        app.on_key(ctrl('u'));
        type_text(&mut app, "/s");
        assert_eq!(suggestions(&mut app), ["skills", "sessions"]);
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.shown().unwrap().1.suggested, 1);
        app.on_key(key(KeyCode::Up));
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.shown().unwrap().1.suggested, 1, "wraps round");
        let effects = app.on_key(key(KeyCode::Enter));
        assert!(matches!(&effects[..], [Effect::FetchSessions(_)]));
        assert!(app.picker.is_some(), "the sessions");
        assert_eq!(input(&mut app), "");

        // Typing again starts from the top; no match, no list.
        app.on_key(key(KeyCode::Esc));
        type_text(&mut app, "/zzz");
        assert!(suggestions(&mut app).is_empty());
    }

    #[test]
    fn arrows_bring_back_what_was_sent_without_completing_it() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        with_agents(&mut app);
        let (id, _) = send(&mut app, "first");
        finish(&mut app, id, "ses_a");
        type_text(&mut app, "/agent");
        app.on_key(key(KeyCode::Enter));
        app.on_key(key(KeyCode::Esc));
        type_text(&mut app, "draft");

        app.on_key(key(KeyCode::Up));
        assert_eq!(input(&mut app), "/agent");
        let (chat, view) = app.shown().unwrap();
        assert!(
            view.suggestions(chat).is_empty(),
            "no list for a recalled command"
        );
        app.on_key(key(KeyCode::Up));
        app.on_key(key(KeyCode::Up));
        assert_eq!(input(&mut app), "first", "stops at the oldest");
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down));
        assert_eq!(input(&mut app), "draft", "past the latest, what was typed");
        app.on_key(key(KeyCode::Down));
        assert_eq!(input(&mut app), "draft");

        // Editing a recalled command completes it again, and the arrows then
        // move in the list instead.
        app.on_key(key(KeyCode::Up));
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(input(&mut app), "/agen");
        app.on_key(key(KeyCode::Up));
        assert_eq!(input(&mut app), "/agen");
    }

    #[test]
    fn the_prompt_keeps_what_isnt_taken() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        send(&mut app, "hi");
        assert_eq!(input(&mut app), "");
        // Typed while the agent works: kept to send later.
        type_text(&mut app, "next");
        assert!(app.on_key(key(KeyCode::Enter)).is_empty());
        assert_eq!(input(&mut app), "next");
        // Pasted lines join the one line.
        app.on_input(Event::Paste(" and\nmore".into()));
        assert_eq!(input(&mut app), "next and more");
        // Esc cancels the turn rather than clearing the prompt.
        assert!(app.on_key(key(KeyCode::Esc)).is_empty());
        assert_eq!(input(&mut app), "next and more");
    }

    #[test]
    fn esc_backs_out_of_typing_a_secret() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        let id = shown(&app);
        app.state.chats[0].auth = Some(crate::state::chat::Auth::Key {
            provider: "acme".into(),
        });
        type_text(&mut app, "sk-");
        assert!(app.on_key(key(KeyCode::Esc)).is_empty());
        assert_eq!(input(&mut app), "");
        assert!(app.state.chat(id).unwrap().auth.is_none());
    }

    #[test]
    fn input_edits_by_character() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        type_text(&mut app, "héllo");
        for code in [
            KeyCode::Home,
            KeyCode::Right,
            KeyCode::Delete,
            KeyCode::End,
            KeyCode::Backspace,
        ] {
            app.on_key(key(code));
        }
        let input = &app.shown().unwrap().1.input;
        assert_eq!((input.text.as_str(), input.cursor), ("hll", 3));
    }
}
