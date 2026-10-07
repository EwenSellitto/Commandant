//! What a client knows and does, apart from how it is shown: the nodes, and
//! every chat open on them. Chats on any node run side by side.
//!
//! A front end sends [`Intent`]s and the background results it gets back as
//! [`Update`]s; both answer with an [`Outcome`]: the calls to make, maybe
//! where to go next, and what to do to a chat's prompt. Which screen is
//! shown, typing and scrolling stay with the front end.

pub mod chat;
#[cfg(test)]
pub(crate) mod fixtures;
mod intent;
mod pick;
#[cfg(test)]
mod tests;

pub use self::intent::{Edit, Effect, Failure, Go, Intent, Outcome, Scope, Update};
pub use self::pick::{Choice, Choose, Pick};

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
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
                c.activity.working()
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
        let Some(node) = self.node(id).cloned() else {
            return Outcome::default();
        };
        self.notice.clear();
        // Its chats are no use until it hosts an agent again.
        if lacks_agent(&node) {
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

    fn node(&self, id: &str) -> Option<&NodeInfo> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// A node's name, else its id.
    fn node_name(&self, id: &str) -> String {
        self.node(id)
            .map_or_else(|| id.to_string(), |n| n.name.clone())
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
        if chat.activity.working() {
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
            let label = commandant_common::or(&session.title, &session.id).to_string();
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

/// Online, but its chats are no use until it hosts an agent again.
pub fn lacks_agent(node: &NodeInfo) -> bool {
    node.online && node.harnesses.is_empty()
}
