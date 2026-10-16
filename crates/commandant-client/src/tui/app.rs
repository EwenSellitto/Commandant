//! The terminal's side of the client: which screen is shown, the
//! highlighted node and task, each chat's prompt, the pickers and the help
//! as shown, and what keys ask of the [`Core`].

use std::collections::HashMap;

use commandant_client_core::{
    Ask, Chat, ChatId, Core, Go, Intent, Next, OutputId, Scope, Update, cycle,
};
use commandant_proto::TaskInfo;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::asks;
use super::chat::{ChatView, Input};
use super::help::{Does, Help};
use super::picker::{Picked, Picker};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Screen {
    #[default]
    Nodes,
    Chat(ChatId),
    Tasks,
    Output(OutputId),
}

/// Lines moved by PageUp / PageDown in an output.
const PAGE: u16 = 10;

pub struct App {
    pub core: Core,
    pub screen: Screen,
    /// The highlighted row of the node list.
    pub selected: usize,
    /// The highlighted row of the task list.
    pub selected_task: usize,
    /// Where Esc goes from the tasks, and from an output.
    tasks_back: Screen,
    output_back: Screen,
    /// How many lines the output shown is scrolled up from the bottom.
    pub output_scroll: u16,
    /// The line the app asks for, as typed.
    pub entry: Input,
    /// The chat last shown on each node.
    last: HashMap<String, ChatId>,
    /// What the terminal keeps of each chat, kept in step with the core's
    /// chats by [`sync`](Self::sync).
    pub views: HashMap<ChatId, ChatView>,
    /// The pickers of the app and its chats, as shown.
    pub pickers: HashMap<Scope, Picker>,
    /// The help, open over everything else.
    pub help: Option<Help>,
    /// What to put on the terminal's clipboard next.
    pub copying: Option<String>,
    /// What was put on it last, to say it was.
    pub copied: Option<String>,
    /// Set once the person has asked to leave.
    pub quit: bool,
    /// Set when leaving forgets the server, for another's link.
    pub logging_out: bool,
}

impl App {
    pub fn new(core: Core) -> Self {
        // Start on the first node that can take a prompt.
        let selected = core
            .nodes()
            .iter()
            .position(|n| n.online && !n.harnesses.is_empty())
            .unwrap_or(0);
        Self {
            core,
            screen: Screen::default(),
            selected,
            selected_task: 0,
            tasks_back: Screen::default(),
            output_back: Screen::default(),
            output_scroll: 0,
            entry: Input::default(),
            last: HashMap::new(),
            views: HashMap::new(),
            pickers: HashMap::new(),
            help: None,
            copying: None,
            copied: None,
            quit: false,
            logging_out: false,
        }
    }

    /// Where questions are shown on screen, the one on top first: the
    /// app's, then the shown chat's.
    pub fn scopes(&self) -> Vec<Scope> {
        let mut scopes = vec![Scope::App];
        if let Screen::Chat(id) = self.screen {
            scopes.push(Scope::Chat(id));
        }
        scopes
    }

    /// The chat on screen, if any.
    pub fn chat(&self) -> Option<&Chat> {
        match self.screen {
            Screen::Chat(id) => self.core.chat(id),
            _ => None,
        }
    }

    /// The highlighted task, if any.
    fn highlighted(&self) -> Option<&TaskInfo> {
        self.core.tasks()?.tasks.get(self.selected_task)
    }

    /// The id of the highlighted task, if any.
    fn task_id(&self) -> Option<&str> {
        self.highlighted().map(|t| t.id.as_str())
    }

    /// Whether the app asks for a line, typed in a box of its own.
    pub fn entering(&self) -> bool {
        self.core.ask(Scope::App).is_some_and(Ask::wants_line)
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
        let chat = self.core.chat(id)?;
        Some((chat, self.views.entry(id).or_default()))
    }

    pub fn on_input(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.on_key(key),
            Event::Paste(text) if self.entering() => self.entry.paste(&text),
            Event::Paste(text) => {
                if let Some((_, view)) = self.shown() {
                    view.paste(&text);
                }
            }
            _ => {}
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        self.sync();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if ctrl && matches!(key.code, KeyCode::Char('c' | 'd')) {
            self.quit = true;
            return;
        }
        if let Some(help) = &mut self.help {
            if let Some(picked) = help.list.on_key(key) {
                self.help = None;
                if let Picked::Chosen(entry) = picked {
                    self.follow_help(entry.does);
                }
            }
            return;
        }
        if key.code == KeyCode::F(1) {
            return self.open_help();
        }
        if self.entering() {
            return self.on_entry_key(key);
        }
        // A question takes the keys, the one on top first; a line asked for
        // is typed in the prompt.
        for scope in self.scopes() {
            if let Some(ask) = self.core.ask(scope).filter(|a| !a.wants_line()) {
                if let (Ask::Show { text, .. }, KeyCode::Char('c')) = (ask, key.code) {
                    let text = text.clone();
                    return self.copy(&text);
                }
                if let Some(intent) = asks::on_key(ask, self.pickers.get_mut(&scope), scope, key) {
                    self.act(intent);
                }
                return;
            }
        }
        match key.code {
            KeyCode::Char('g') if ctrl => return self.show_nodes(),
            KeyCode::Char('h') if ctrl => return self.act(Intent::ListTasks { node: None }),
            _ => {}
        }
        match self.screen {
            Screen::Nodes => return self.on_node_key(key),
            Screen::Tasks => return self.on_tasks_key(key),
            Screen::Output(id) => return self.on_output_key(id, key),
            Screen::Chat(_) => {}
        }
        let Some((chat, view)) = self.shown() else {
            return;
        };
        let id = chat.id;
        let intent = match key.code {
            KeyCode::Char('n') if ctrl => Some(Intent::NewChat(id)),
            KeyCode::Char('o') if ctrl => Some(Intent::Sessions(id)),
            KeyCode::Char('w') if ctrl => Some(Intent::Close(id)),
            KeyCode::Left if alt => return self.cycle_chat(-1),
            KeyCode::Right if alt => return self.cycle_chat(1),
            _ => view.on_key(chat, key),
        };
        if let Some(intent) = intent {
            self.act(intent);
        }
    }

    fn on_node_key(&mut self, key: KeyEvent) {
        let last = self.core.nodes().len().saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(last),
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.open_help(),
            KeyCode::Enter => self.open_selected_node(),
            KeyCode::Char('r') => {
                if let Some(node) = self.selected_node() {
                    self.entry.clear();
                    self.act(Intent::Run {
                        node,
                        line: String::new(),
                    });
                }
            }
            KeyCode::Char('a') => {
                self.entry.clear();
                self.act(Intent::AddNode);
            }
            KeyCode::Char('d') => {
                if let Some(node) = self.selected_node() {
                    self.act(Intent::RemoveNode(node));
                }
            }
            KeyCode::Char('s') => {
                if let Some(node) = self.selected_node() {
                    self.act(Intent::StartAgent(node));
                }
            }
            _ => {}
        }
    }

    /// The id of the highlighted node, if any.
    fn selected_node(&self) -> Option<String> {
        self.core.nodes().get(self.selected).map(|n| n.id.clone())
    }

    /// Puts `text` on the terminal's clipboard.
    fn copy(&mut self, text: &str) {
        self.copying = Some(text.to_string());
        self.copied = Some(text.to_string());
    }

    fn on_tasks_key(&mut self, key: KeyEvent) {
        let listed = self.core.tasks().map_or(0, |t| t.tasks.len());
        let task = self.task_id().map(str::to_string);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected_task = self.selected_task.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected_task = (self.selected_task + 1).min(listed.saturating_sub(1))
            }
            KeyCode::Enter => {
                if let Some(task) = task {
                    self.act(Intent::OpenTask(task));
                }
            }
            KeyCode::Char('f') => self.act(Intent::FilterTasks),
            KeyCode::Char('x') => {
                let running = self.highlighted().filter(|t| t.status == "running");
                if let Some(task) = running.map(|t| t.id.clone()) {
                    self.act(Intent::CancelTask(task));
                }
            }
            KeyCode::Char('?') => self.open_help(),
            KeyCode::Esc | KeyCode::Char('q') => self.go_to(self.tasks_back),
            _ => {}
        }
    }

    fn on_output_key(&mut self, id: OutputId, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.output_scroll = self.output_scroll.saturating_add(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.output_scroll = self.output_scroll.saturating_sub(1)
            }
            KeyCode::PageUp => self.output_scroll = self.output_scroll.saturating_add(PAGE),
            KeyCode::PageDown => self.output_scroll = self.output_scroll.saturating_sub(PAGE),
            KeyCode::Char('x') => {
                let output = self.core.output(id);
                if let Some(task) = output.and_then(|o| o.run.task()).cloned() {
                    self.act(Intent::CancelTask(task));
                }
            }
            KeyCode::Char('?') => self.open_help(),
            KeyCode::Esc | KeyCode::Char('q') => self.go_to(self.output_back),
            _ => {}
        }
    }

    /// Types the line the app asks for, in its own box.
    fn on_entry_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            // Not typed: the app's keys wait until the line is in.
            KeyCode::Char(c) if ctrl && c != 'u' => {}
            KeyCode::Enter => {
                let line = std::mem::take(&mut self.entry).text;
                self.act(Intent::Enter(Scope::App, line));
            }
            KeyCode::Esc => {
                self.entry.clear();
                self.act(Intent::Dismiss(Scope::App));
            }
            _ => self.entry.edit(key),
        }
    }

    /// Shows what every command and key of the screen shown does.
    fn open_help(&mut self) {
        self.help = Some(match self.screen {
            Screen::Chat(_) => self.chat().map_or_else(Help::nodes, Help::chat),
            Screen::Nodes => Help::nodes(),
            Screen::Tasks => Help::tasks(),
            Screen::Output(_) => Help::output(),
        });
    }

    /// Does what was chosen in the help.
    fn follow_help(&mut self, does: Does) {
        match does {
            Does::Press(key) => self.on_key(key),
            Does::Run(line) => {
                if let Screen::Chat(id) = self.screen {
                    // The line isn't the prompt's: what is typed there stays.
                    let next = self.core.act(Intent::Submit(id, line));
                    self.apply(Next {
                        prompt: None,
                        ..next
                    });
                }
            }
            Does::Start(line) => {
                if let Some((_, view)) = self.shown() {
                    view.input.set(&line);
                }
            }
            Does::Nothing => {}
        }
    }

    /// Shows the highlighted node's last chat, or starts one.
    fn open_selected_node(&mut self) {
        let Some(node) = self.core.nodes().get(self.selected) else {
            return;
        };
        let chat = self.last.get(&node.id).copied();
        let node = node.id.clone();
        self.act(Intent::Open { node, chat });
    }

    /// Shows the next (or previous) chat on the same node.
    fn cycle_chat(&mut self, step: isize) {
        let Some(chat) = self.chat() else {
            return;
        };
        let ids: Vec<ChatId> = self.core.chats_on(&chat.node.id).map(|c| c.id).collect();
        if let Some(next) = cycle(&ids, chat.id, step) {
            self.show(next);
        }
    }

    pub fn on_update(&mut self, update: Update) {
        let selected = self.core.nodes().get(self.selected).map(|n| n.id.clone());
        let task = self.task_id().map(str::to_string);
        let next = self.core.on_update(update);
        // Keep the same node, and the same task, highlighted.
        let nodes = self.core.nodes();
        if let Some(at) = selected.and_then(|id| nodes.iter().position(|n| n.id == id)) {
            self.selected = at;
        }
        self.selected = self.selected.min(nodes.len().saturating_sub(1));
        let tasks = self.core.tasks().map_or(&[][..], |t| &t.tasks);
        if let Some(at) = task.and_then(|id| tasks.iter().position(|t| t.id == id)) {
            self.selected_task = at;
        }
        self.selected_task = self.selected_task.min(tasks.len().saturating_sub(1));
        self.apply(next);
        // What happens in the chat on screen is seen as it happens.
        if let Screen::Chat(id) = self.screen {
            self.core.act(Intent::Seen(id));
        }
    }

    /// Asks the core for something, and follows where that leads.
    pub fn act(&mut self, intent: Intent) {
        let next = self.core.act(intent);
        self.apply(next);
    }

    /// Does the terminal's part of what the core says comes next.
    fn apply(&mut self, next: Next) {
        let Next { go, prompt, sent } = next;
        if let Some((id, edit)) = prompt {
            self.view(id).edit(edit);
        }
        // A prompt sent: back to the bottom of its thread.
        if let Some(id) = sent {
            self.view(id).scroll = 0;
        }
        match go {
            Some(Go::Chat(id)) => self.show(id),
            Some(Go::Nodes) => self.show_nodes(),
            // A new list, from its top.
            Some(Go::Tasks) => {
                self.selected_task = 0;
                self.go_to(Screen::Tasks);
            }
            Some(Go::Output(id)) => self.go_to(Screen::Output(id)),
            Some(Go::Quit) => self.quit = true,
            Some(Go::LogOut) => {
                self.quit = true;
                self.logging_out = true;
            }
            Some(Go::Help) => self.open_help(),
            // Still looking at it: open it.
            Some(Go::Ready(node)) => {
                let selected = self.core.nodes().get(self.selected).map(|n| &n.id);
                if self.screen == Screen::Nodes && selected == Some(&node) {
                    self.open_selected_node();
                }
            }
            None => {}
        }
        self.sync();
    }

    fn show(&mut self, id: ChatId) {
        let Some(chat) = self.core.chat(id) else {
            return;
        };
        self.last.insert(chat.node.id.clone(), id);
        self.go_to(Screen::Chat(id));
        self.core.act(Intent::Seen(id));
    }

    fn show_nodes(&mut self) {
        if let Some(chat) = self.chat()
            && let Some(at) = self.core.nodes().iter().position(|n| n.id == chat.node.id)
        {
            self.selected = at;
        }
        self.core.act(Intent::Dismiss(Scope::App));
        self.go_to(Screen::Nodes);
    }

    /// Shows `screen`, telling the core what is no longer shown: an output
    /// is no longer watched, and the tasks no longer refreshed once left for
    /// a chat or the nodes.
    fn go_to(&mut self, screen: Screen) {
        let from = self.screen;
        if from == screen {
            return;
        }
        if let Screen::Output(id) = from {
            self.core.act(Intent::CloseOutput(id));
        }
        match screen {
            // Back from an output opened on the list: where the list was
            // opened from; else where the output was.
            Screen::Tasks => match from {
                Screen::Output(_) if self.output_back == Screen::Tasks => {}
                Screen::Output(_) => self.tasks_back = self.output_back,
                from => self.tasks_back = from,
            },
            Screen::Output(_) => {
                self.output_back = from;
                self.output_scroll = 0;
            }
            Screen::Nodes | Screen::Chat(_) => {
                self.core.act(Intent::CloseTasks);
            }
        }
        // A chat closed while away isn't there to go back to.
        for back in [&mut self.tasks_back, &mut self.output_back] {
            if let Screen::Chat(id) = *back
                && self.core.chat(id).is_none()
            {
                *back = Screen::Nodes;
            }
        }
        self.screen = screen;
    }

    /// Shows what the core asks to choose, and forgets closed chats.
    pub fn sync(&mut self) {
        let core = &self.core;
        self.views.retain(|id, _| core.chat(*id).is_some());
        for chat in core.chats() {
            self.views.entry(chat.id).or_default();
        }
        let mut shown = std::mem::take(&mut self.pickers);
        let chats = core.chats().iter().map(|c| Scope::Chat(c.id));
        self.pickers = std::iter::once(Scope::App)
            .chain(chats)
            .filter_map(|scope| {
                let choices = core.ask(scope).and_then(Ask::choices);
                Some((scope, Picker::sync(shown.remove(&scope), choices)?))
            })
            .collect();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use commandant_client_core::chat::Unseen;
    use commandant_client_core::state::fixtures::{acme, bare, method, node, started};
    use commandant_client_core::{Choose, Effect, Settings};
    use commandant_proto::*;

    pub(crate) fn app() -> App {
        let nodes = vec![node("n1", true), node("n2", true), node("n3", false)];
        App::new(Core::offline(nodes, Settings::default()))
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

    /// The calls pressing `key` has the core make.
    pub(crate) fn press(app: &mut App, key: KeyEvent) -> Vec<Effect> {
        app.core.take_effects();
        app.on_key(key);
        app.core.take_effects()
    }

    /// The calls taking in `update` has the core make.
    fn update(app: &mut App, update: Update) -> Vec<Effect> {
        app.core.take_effects();
        app.on_update(update);
        app.core.take_effects()
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
    }

    /// The shown chat's picker, if it is choosing.
    fn chat_picker(app: &App) -> Option<&Picker> {
        app.pickers.get(&Scope::Chat(shown(app)))
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
        match press(app, key(KeyCode::Enter)).pop() {
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
        let message =
            commandant_client_core::chat::Message::Task(task_event::Event::Finished(finished));
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
        let effects = press(&mut app, key(KeyCode::Enter));
        assert!(matches!(&effects[..], [Effect::FetchSessions(n), _] if n == "n2"));
        assert_eq!(app.chat().unwrap().node.id, "n2");

        // An offline node with no chat yet stays on the list.
        app.on_key(ctrl('g'));
        assert_eq!(app.screen, Screen::Nodes);
        app.on_key(key(KeyCode::Down));
        assert!(press(&mut app, key(KeyCode::Enter)).is_empty());
        assert_eq!(app.screen, Screen::Nodes);
        assert_eq!(app.core.notice(), "box-n3 is offline");

        // Going back to a node shows its chat again.
        app.on_key(key(KeyCode::Up));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.core.chats().len(), 1);
        assert_eq!(app.screen, Screen::Chat(app.core.chats()[0].id));
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
        let unseen = |app: &App, id| app.core.chat(id).unwrap().unseen;
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
        assert_eq!(app.core.nodes()[app.selected].id, "n1");
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
        assert!(!typing.logging_out, "quitting keeps the server");
    }

    #[test]
    fn logging_out_leaves_and_forgets_the_server() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        type_text(&mut app, "/logout");
        app.on_key(key(KeyCode::Enter));
        assert!(app.quit && app.logging_out);
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
        assert_eq!(app.core.nodes()[app.selected].id, "n2");

        // A shrinking list keeps the highlight on it, and an empty one is inert.
        app.on_key(key(KeyCode::Down));
        app.on_update(Update::Nodes(vec![node("n2", true)]));
        assert_eq!(app.selected, 0);
        app.on_update(Update::Nodes(Vec::new()));
        assert!(press(&mut app, key(KeyCode::Enter)).is_empty());
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.selected, 0);

        // Its chat is still there when the node comes back.
        app.on_update(Update::Nodes(vec![node("n1", true)]));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(shown(&app), id);
        // Pasting on the node list does nothing.
        app.on_key(ctrl('g'));
        app.core.take_effects();
        app.on_input(Event::Paste("x".into()));
        assert!(app.core.take_effects().is_empty());
    }

    #[test]
    fn app_keys_wait_while_a_chat_picker_is_open() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        with_agents(&mut app);
        type_text(&mut app, "/agent");
        app.on_key(key(KeyCode::Enter));
        assert!(chat_picker(&app).is_some());
        for keys in [ctrl('n'), ctrl('o'), ctrl('g'), ctrl('w')] {
            app.on_key(keys);
        }
        assert_eq!(app.core.chats().len(), 1);
        assert!(!app.pickers.contains_key(&Scope::App));
        assert!(matches!(app.screen, Screen::Chat(_)));

        // Its keys went to its filter; cleared, they move in it and choose.
        assert_eq!(chat_picker(&app).unwrap().list.filter, "nogw");
        app.on_key(ctrl('u'));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Enter));
        assert!(chat_picker(&app).is_none());
        assert_eq!(app.chat().unwrap().agent(), "plan");
    }

    #[test]
    fn a_picker_keeps_its_filter_as_its_choices_come_in() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        app.on_key(ctrl('o'));
        type_text(&mut app, "about");
        assert_eq!(app.pickers.get(&Scope::App).unwrap().list.shown_len(), 0);
        let saved = |id: &str| AgentSession {
            id: id.into(),
            title: format!("about {id}"),
            ..Default::default()
        };
        app.on_update(Update::Sessions(
            "n1".into(),
            Ok(vec![saved("a"), saved("b")]),
        ));
        let picker = app.pickers.get(&Scope::App).unwrap();
        assert_eq!(
            (picker.list.filter.as_str(), picker.list.shown_len()),
            ("about", 2)
        );
        assert!(!picker.loading());

        // "+ New session" is a new chat.
        app.on_key(ctrl('u'));
        app.on_key(key(KeyCode::Up));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.core.chats().len(), 2);
        assert!(!app.pickers.contains_key(&Scope::App));
    }

    #[test]
    fn a_node_starting_its_agent_opens_once_ready_if_still_in_view() {
        let mut app = App::new(Core::offline(
            vec![bare("n1"), bare("n2")],
            Settings::default(),
        ));
        app.on_key(key(KeyCode::Enter));
        assert!(
            app.pickers.contains_key(&Scope::App),
            "the harness to start"
        );
        // Esc backs out, and nothing starts.
        app.on_key(key(KeyCode::Esc));
        assert!(!app.pickers.contains_key(&Scope::App));
        assert!(!app.core.starting("n1"));
        app.on_key(key(KeyCode::Enter));
        let effects = press(&mut app, key(KeyCode::Enter));
        assert!(matches!(&effects[..], [Effect::StartHarness { .. }]));

        let ready = |id: &str| NodeInfo {
            harnesses: vec!["opencode".into()],
            ..bare(id)
        };
        let effects = update(
            &mut app,
            Update::HarnessStarted("n1".into(), Ok(ready("n1"))),
        );
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
            update(
                &mut app,
                Update::HarnessStarted("n2".into(), Ok(ready("n2")))
            )
            .is_empty()
        );
        assert_eq!(app.screen, Screen::Nodes);
        assert_eq!(app.core.notice(), "box-n2 now hosts opencode");
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
        let effects = press(&mut app, key(KeyCode::Enter));
        assert!(matches!(&effects[..], [Effect::FetchSessions(_)]));
        assert!(app.pickers.contains_key(&Scope::App), "the sessions");
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
    fn only_a_prompt_sent_scrolls_back_to_the_bottom() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        let scroll = |app: &mut App| app.shown().unwrap().1.scroll;
        app.shown().unwrap().1.scroll = 5;
        type_text(&mut app, "/help");
        app.on_key(key(KeyCode::Enter));
        app.on_key(key(KeyCode::Esc));
        assert_eq!(scroll(&mut app), 5, "a command keeps the place read");
        send(&mut app, "hi");
        assert_eq!(scroll(&mut app), 0);
    }

    #[test]
    fn the_help_does_what_is_chosen_in_it() {
        let mut app = app();
        assert!(app.help.is_none());
        app.on_key(KeyEvent::from(KeyCode::Char('?')));
        assert!(app.help.is_some(), "on the nodes too");
        app.on_key(key(KeyCode::Esc));
        app.on_key(key(KeyCode::Enter));

        with_agents(&mut app);
        type_text(&mut app, "/?");
        app.on_key(key(KeyCode::Enter));
        assert!(app.help.is_some());
        assert_eq!(input(&mut app), "");
        // A command chosen runs, and what was being typed stays.
        app.on_key(key(KeyCode::Esc));
        type_text(&mut app, "draft");
        app.on_key(key(KeyCode::F(1)));
        type_text(&mut app, "choose an agent");
        app.on_key(key(KeyCode::Enter));
        assert!(app.help.is_none());
        assert_eq!(chat_picker(&app).unwrap().title(), "Agent");
        assert_eq!(input(&mut app), "draft");

        // One of the agent's is started, for its arguments.
        app.on_key(key(KeyCode::Esc));
        app.on_key(key(KeyCode::F(1)));
        type_text(&mut app, "/review");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(input(&mut app), "/review ");

        // A key chosen is pressed.
        app.on_key(key(KeyCode::Esc));
        app.on_key(key(KeyCode::F(1)));
        type_text(&mut app, "clear the prompt");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(input(&mut app), "");
    }

    #[test]
    fn the_prompt_keeps_what_isnt_taken() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        send(&mut app, "hi");
        assert_eq!(input(&mut app), "");
        // Typed while the agent works: kept to send later.
        type_text(&mut app, "next");
        assert!(press(&mut app, key(KeyCode::Enter)).is_empty());
        assert_eq!(input(&mut app), "next");
        // Pasted lines join the one line.
        app.on_input(Event::Paste(" and\nmore".into()));
        assert_eq!(input(&mut app), "next and more");
        // Esc cancels the turn rather than clearing the prompt.
        assert!(press(&mut app, key(KeyCode::Esc)).is_empty());
        assert_eq!(input(&mut app), "next and more");
    }

    /// Picks signing in to a provider with an API key, from the chat on
    /// screen, which then asks for the key.
    pub(crate) fn ask_for_a_key(app: &mut App) {
        let id = shown(app);
        type_text(app, "/providers");
        app.on_key(key(KeyCode::Enter));
        let acme = acme(vec![method("API key", false, 0)]);
        let listed = commandant_client_core::chat::Message::Providers(Ok(vec![acme]));
        app.on_update(Update::Chat(id, listed));
        app.on_key(key(KeyCode::Enter));
        app.on_key(key(KeyCode::Enter));
        assert!(app.core.ask(Scope::Chat(id)).is_some_and(Ask::secret));
    }

    #[test]
    fn a_key_asked_for_is_typed_in_the_prompt() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        let id = shown(&app);
        ask_for_a_key(&mut app);
        // The app's keys still work meanwhile.
        app.on_key(ctrl('n'));
        assert_ne!(shown(&app), id);
        app.on_key(alt(KeyCode::Left));
        assert_eq!(shown(&app), id);

        type_text(&mut app, "sk-");
        assert!(press(&mut app, key(KeyCode::Esc)).is_empty());
        assert_eq!(input(&mut app), "");
        assert!(app.core.ask(Scope::Chat(id)).is_none());

        // Entered, it goes to the node and nowhere else.
        ask_for_a_key(&mut app);
        type_text(&mut app, "sk-secret");
        let effects = press(&mut app, key(KeyCode::Enter));
        assert!(matches!(&effects[..], [Effect::Authenticate { .. }]));
        assert_eq!(input(&mut app), "");
        app.on_key(key(KeyCode::Up));
        assert_ne!(input(&mut app), "sk-secret", "not brought back");
    }

    #[test]
    fn a_question_to_confirm_or_read_takes_the_keys() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        let id = shown(&app);
        let confirm = || Ask::Confirm {
            text: "Start opencode?".into(),
            yes: Choose::Harness {
                node: "n2".into(),
                harness: "opencode".into(),
            },
        };
        app.core.set_ask(Scope::App, confirm());
        // Other keys do nothing; Esc says no.
        assert!(press(&mut app, ctrl('n')).is_empty());
        assert_eq!(app.core.chats().len(), 1);
        app.on_key(key(KeyCode::Esc));
        assert!(app.core.ask(Scope::App).is_none());
        app.core.set_ask(Scope::App, confirm());
        let effects = press(&mut app, key(KeyCode::Char('y')));
        assert!(matches!(&effects[..], [Effect::StartHarness { .. }]));

        // A chat's takes the chat's keys.
        let show = Ask::Show {
            title: "Link".into(),
            text: "commandant://x".into(),
        };
        app.core.set_ask(Scope::Chat(id), show);
        type_text(&mut app, "hi");
        assert_eq!(input(&mut app), "");
        app.on_key(key(KeyCode::Enter));
        assert!(app.core.ask(Scope::Chat(id)).is_none());
    }

    fn task(id: &str, kind: &str, status: &str) -> TaskInfo {
        TaskInfo {
            id: id.into(),
            node_id: "n1".into(),
            node_name: "box-n1".into(),
            kind: kind.into(),
            status: status.into(),
            argv: vec!["sh".into(), "-c".into(), format!("echo {id}")],
            session_id: format!("ses_{id}"),
            ..Default::default()
        }
    }

    /// Opens the tasks with Ctrl-H and lists `tasks` on them.
    pub(crate) fn tasks_listed(app: &mut App, tasks: Vec<TaskInfo>) {
        let effects = press(app, ctrl('h'));
        let [
            Effect::FetchTasks {
                node: None, list, ..
            },
        ] = effects[..]
        else {
            panic!("should list every node's tasks, not {effects:?}");
        };
        app.on_update(Update::Tasks {
            list,
            tasks: Ok(tasks),
        });
    }

    pub(crate) fn listed_tasks() -> Vec<TaskInfo> {
        vec![
            task("t2", "command", "running"),
            task("t1", "prompt", "succeeded"),
        ]
    }

    #[test]
    fn the_tasks_are_read_followed_and_cancelled() {
        let mut app = app();
        tasks_listed(&mut app, listed_tasks());
        assert_eq!(app.screen, Screen::Tasks);

        // x cancels a running task, and only a running one.
        assert!(matches!(&press(&mut app, key(KeyCode::Char('x')))[..],
            [Effect::CancelTask(t)] if t == "t2"));
        app.on_key(key(KeyCode::Down));
        assert!(press(&mut app, key(KeyCode::Char('x'))).is_empty());

        // A command opens on its output; Esc goes back, and stops watching.
        app.on_key(key(KeyCode::Up));
        let effects = press(&mut app, key(KeyCode::Enter));
        assert!(matches!(&effects[..], [Effect::Watch { task_id, .. }] if task_id == "t2"));
        assert!(matches!(app.screen, Screen::Output(_)));
        let effects = press(&mut app, key(KeyCode::Esc));
        assert!(matches!(&effects[..], [Effect::StopWatching(_)]));
        assert_eq!(app.screen, Screen::Tasks);

        // f chooses which node's tasks to show.
        assert!(press(&mut app, key(KeyCode::Char('f'))).is_empty());
        assert_eq!(
            app.pickers.get(&Scope::App).unwrap().title(),
            "Show the tasks of"
        );
        app.on_key(key(KeyCode::Down));
        let effects = press(&mut app, key(KeyCode::Enter));
        assert!(matches!(&effects[..],
            [Effect::FetchTasks { node: Some(n), .. }] if n == "n1"));

        // A prompt resumes its session in a chat.
        let mut app = self::app();
        tasks_listed(&mut app, listed_tasks());
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Enter));
        let chat = app.chat().expect("the prompt's session is open");
        assert_eq!(chat.settings.session_id, "ses_t1");
    }

    #[test]
    fn the_tasks_go_back_where_they_were_opened() {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        let id = shown(&app);
        type_text(&mut app, "/tasks");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.screen, Screen::Tasks);
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.screen, Screen::Chat(id));
        // Closed, the list isn't asked for again.
        tasks_listed(&mut app, Vec::new());
        app.on_key(ctrl('g'));
        assert_eq!(app.screen, Screen::Nodes);
        assert!(app.core.tasks().is_none());
    }

    #[test]
    fn r_runs_a_command_on_the_highlighted_node() {
        let mut app = app();
        app.on_key(key(KeyCode::Down));
        assert!(press(&mut app, key(KeyCode::Char('r'))).is_empty());
        assert!(app.core.ask(Scope::App).is_some_and(Ask::wants_line));
        // Typed in a line of its own; Esc gives up.
        type_text(&mut app, "lsx");
        app.on_key(key(KeyCode::Backspace));
        app.on_input(Event::Paste(" -l".into()));
        assert_eq!(app.entry.text, "ls -l");
        app.on_key(key(KeyCode::Esc));
        assert!(app.core.ask(Scope::App).is_none());
        assert_eq!(app.screen, Screen::Nodes);

        app.on_key(key(KeyCode::Char('r')));
        assert_eq!(app.entry.text, "", "a fresh line");
        type_text(&mut app, "make test");
        let effects = press(&mut app, key(KeyCode::Enter));
        let [Effect::RunOn { request, .. }] = &effects[..] else {
            panic!("should run it, not {effects:?}");
        };
        assert_eq!(
            (request.node.as_str(), request.argv[2].as_str()),
            ("n2", "make test")
        );
        assert!(matches!(app.screen, Screen::Output(_)));
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.screen, Screen::Nodes);
        assert_eq!(app.core.nodes()[app.selected].id, "n2");
    }

    #[test]
    fn a_new_list_starts_at_its_top() {
        let mut app = app();
        tasks_listed(&mut app, listed_tasks());
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Char('f')));
        app.on_key(key(KeyCode::Down));
        let effects = press(&mut app, key(KeyCode::Enter));
        let [Effect::FetchTasks { list, .. }] = effects[..] else {
            panic!("should list n1's tasks, not {effects:?}");
        };
        app.on_update(Update::Tasks {
            list,
            tasks: Ok(vec![
                task("t3", "command", "running"),
                task("t4", "command", "running"),
            ]),
        });
        assert_eq!(app.selected_task, 0);
    }

    #[test]
    fn the_tasks_opened_from_an_output_go_back_where_it_came_from() {
        let mut app = app();
        app.on_key(key(KeyCode::Char('r')));
        // Keys with Ctrl type nothing in the line.
        type_text(&mut app, "ls");
        app.on_key(ctrl('h'));
        assert_eq!(app.entry.text, "ls");
        app.on_key(key(KeyCode::Enter));
        assert!(matches!(app.screen, Screen::Output(_)));
        app.on_key(ctrl('h'));
        assert_eq!(app.screen, Screen::Tasks);
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.screen, Screen::Nodes);
    }

    #[test]
    fn a_node_is_added_by_copying_the_command_shown() {
        let mut app = app();
        assert!(press(&mut app, key(KeyCode::Char('a'))).is_empty());
        assert!(app.entering(), "how long its token lasts");
        let effects = press(&mut app, key(KeyCode::Enter));
        assert!(matches!(&effects[..], [Effect::CreateToken { .. }]));
        app.on_update(Update::TokenCreated {
            ttl: std::time::Duration::from_secs(3600),
            link: Ok("commandant://abc".into()),
        });

        // c copies the command, which stays to read; Esc closes it.
        assert!(app.copying.is_none());
        app.on_key(key(KeyCode::Char('c')));
        let command = "commandant-server worker commandant://abc";
        assert_eq!(app.copying.take().as_deref(), Some(command));
        assert_eq!(app.copied.as_deref(), Some(command));
        assert!(app.core.ask(Scope::App).is_some());
        app.on_key(key(KeyCode::Esc));
        assert!(app.core.ask(Scope::App).is_none());
        assert_eq!(app.screen, Screen::Nodes);
    }

    #[test]
    fn d_removes_the_highlighted_node_once_confirmed() {
        let mut app = app();
        app.on_key(key(KeyCode::Down));
        // n saying no changes nothing.
        assert!(press(&mut app, key(KeyCode::Char('d'))).is_empty());
        assert!(matches!(
            app.core.ask(Scope::App),
            Some(Ask::Confirm { .. })
        ));
        assert!(press(&mut app, key(KeyCode::Char('n'))).is_empty());
        assert_eq!(app.core.nodes().len(), 3);

        app.on_key(key(KeyCode::Char('d')));
        let effects = press(&mut app, key(KeyCode::Enter));
        assert!(matches!(&effects[..], [Effect::RemoveNode(n)] if n == "n2"));
        app.on_update(Update::NodeRemoved("n2".into(), Ok(())));
        let left: Vec<_> = app.core.nodes().iter().map(|n| n.id.as_str()).collect();
        assert_eq!(left, ["n1", "n3"]);
        assert!(app.selected < left.len());
    }

    #[test]
    fn s_starts_an_agent_on_the_highlighted_node() {
        let nodes = vec![node("n1", true), bare("n2")];
        let mut app = App::new(Core::offline(nodes, Settings::default()));
        // One with an agent already says so.
        app.selected = 0;
        assert!(press(&mut app, key(KeyCode::Char('s'))).is_empty());
        assert_eq!(app.core.notice(), "box-n1 already hosts opencode");

        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Char('s')));
        assert_eq!(
            app.pickers.get(&Scope::App).unwrap().title(),
            "Start an agent on this node"
        );
        let effects = press(&mut app, key(KeyCode::Enter));
        assert!(matches!(&effects[..], [Effect::StartHarness { node, .. }] if node == "n2"));
        assert!(app.core.starting("n2"));
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
