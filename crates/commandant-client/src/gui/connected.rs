//! The window once connected: the sidebar, the chat shown beside it, and
//! what the core asks over both.

use std::collections::HashMap;

use commandant_client_core::{ChatId, Core, Edit, Go, Intent, Next, Scope, Updates};
use gpui::{Context, IntoElement, ParentElement as _, Render, Styled as _, Task, Window, div};
use gpui_component::h_flex;

use super::asks::Question;
use super::chat::Pane;
use super::rows::{Change, rows};

/// A core, and what the window keeps of its chats.
pub(super) struct Connected {
    pub(super) core: Core,
    /// The chat in the main pane: none until one is opened, or once the one
    /// shown closes with no other on its node.
    pub(super) shown: Option<ChatId>,
    /// The chat last shown on each node, to go back to.
    pub(super) last: HashMap<String, ChatId>,
    /// What the window keeps of each open chat.
    pub(super) panes: HashMap<ChatId, Pane>,
    /// What the window keeps of each open question, by scope.
    pub(super) questions: HashMap<Scope, Question>,
    /// Hands the core what its calls report, for as long as it is shown.
    _updates: Task<()>,
}

impl Connected {
    /// Shows `core`, opening `open` first if it is given.
    pub(super) fn new(
        core: Core,
        mut updates: Updates,
        open: Option<Intent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let pump = cx.spawn_in(window, async move |this, cx| {
            while let Some(update) = updates.next().await {
                let taken = this.update_in(cx, |this, window, cx| {
                    // A reply streams in many small pieces: take what has
                    // come before drawing again.
                    let next = this.core.on_update(update);
                    this.apply(next, window, cx);
                    while let Some(update) = updates.ready() {
                        let next = this.core.on_update(update);
                        this.apply(next, window, cx);
                    }
                    // What happens in the chat shown is seen as it happens.
                    if let Some(id) = this.shown {
                        this.core.act(Intent::Seen(id));
                    }
                    this.sync(window, cx);
                    cx.notify();
                });
                if taken.is_err() {
                    break;
                }
            }
        });
        let mut connected = Self {
            core,
            shown: None,
            last: HashMap::new(),
            panes: HashMap::new(),
            questions: HashMap::new(),
            _updates: pump,
        };
        if let Some(open) = open {
            connected.act(open, window, cx);
        }
        connected
    }

    /// Asks the core for something, and follows where that leads.
    pub(super) fn act(&mut self, intent: Intent, window: &mut Window, cx: &mut Context<Self>) {
        let next = self.core.act(intent);
        self.apply(next, window, cx);
        self.sync(window, cx);
        cx.notify();
    }

    /// Does the window's part of what the core says comes next.
    fn apply(&mut self, next: Next, window: &mut Window, cx: &mut Context<Self>) {
        let Next { go, prompt, sent } = next;
        if let Some((id, edit)) = prompt {
            let composer = self.pane(id, window, cx).composer.clone();
            composer.update(cx, |composer, cx| match edit {
                Edit::Clear => composer.set_value("", window, cx),
                Edit::Insert(text) => composer.insert(text, window, cx),
            });
        }
        // A prompt sent: back to the bottom of its thread.
        if let Some(id) = sent {
            let scroller = self.pane(id, window, cx).scroller.clone();
            scroller.update(cx, |scroller, cx| scroller.scroll_to_end(cx));
        }
        match go {
            Some(Go::Chat(id)) => self.show(id, window, cx),
            Some(Go::Nodes) => self.shown = None,
            // Its agent started while nothing else was opened: open it.
            Some(Go::Ready(node)) if self.shown.is_none() => {
                let next = self.core.act(Intent::Open { node, chat: None });
                self.apply(next, window, cx);
            }
            // Quitting, logging out, the help, and the nodes, tasks and
            // output views come with the views that go there.
            Some(Go::Quit | Go::LogOut | Go::Help | Go::Ready(_) | Go::Tasks | Go::Output(_))
            | None => {}
        }
    }

    /// Shows a chat, which its turn's end is then no news to.
    pub(super) fn show(&mut self, id: ChatId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = self.core.chat(id) else {
            return;
        };
        self.last.insert(chat.node.id.clone(), id);
        if let Some(from) = self.shown.filter(|from| *from != id) {
            self.core.act(Intent::Leave { chat: from, to: id });
        }
        self.shown = Some(id);
        self.core.act(Intent::Seen(id));
        self.pane(id, window, cx);
        // A pane new to the window learns its thread.
        self.sync(window, cx);
        self.refocus(window, cx);
    }

    /// Puts the keys where they go: the question over the window, else the
    /// one over the chat shown, else its prompt.
    fn refocus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let question = [Some(Scope::App), self.shown.map(Scope::Chat)]
            .into_iter()
            .flatten()
            .find_map(|scope| self.questions.get(&scope));
        if let Some(question) = question {
            return question.focus(window, cx);
        }
        if let Some(pane) = self.shown.and_then(|id| self.panes.get(&id)) {
            let composer = pane.composer.clone();
            composer.update(cx, |composer, cx| composer.focus(window, cx));
        }
    }

    /// What the window keeps of the chat `id`, kept from now on.
    fn pane(&mut self, id: ChatId, window: &mut Window, cx: &mut Context<Self>) -> &mut Pane {
        self.panes
            .entry(id)
            .or_insert_with(|| Pane::new(id, window, cx))
    }

    /// Forgets closed chats, follows what the core asks, and tells each
    /// thread's scroller how its thread has changed.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let core = &self.core;
        self.panes.retain(|id, _| core.chat(*id).is_some());
        let scopes: Vec<Scope> = std::iter::once(Scope::App)
            .chain(core.chats().iter().map(|c| Scope::Chat(c.id)))
            .collect();
        // A question that comes or goes takes the keys or gives them back.
        let mut shown = std::mem::take(&mut self.questions);
        let mut moved = false;
        for scope in scopes {
            let kept = shown.remove(&scope);
            let was = kept.as_ref().map(|q| q.focus.clone());
            let question = Question::sync(self.core.ask(scope), kept, scope, window, cx);
            moved |= question.as_ref().map(|q| q.focus.clone()) != was;
            self.questions.extend(question.map(|q| (scope, q)));
        }
        if moved {
            self.refocus(window, cx);
        }
        let core = &self.core;
        for (id, pane) in &mut self.panes {
            let Some(chat) = core.chat(*id) else {
                continue;
            };
            pane.rows = rows(&chat.thread);
            let count = pane.rows.len();
            let scrolled_up = pane.scroller.read(cx).is_scrolled_up();
            let change = pane.tally.change(chat, &pane.rows, scrolled_up);
            // The rows open are known by their first entry, which the
            // earlier turns coming have moved.
            if let Change::Reset(_) = change {
                pane.open.clear();
            }
            pane.scroller.update(cx, |scroller, cx| match change {
                Change::Reset(count) => scroller.reset(count, cx),
                Change::Grew { added, settled } => {
                    if added > 0 {
                        scroller.append(added, cx);
                    }
                    if settled {
                        scroller.remeasure(cx);
                    } else {
                        // The rows still streaming in.
                        let from = count.saturating_sub(added + 1);
                        scroller.remeasure_items(from..count, cx);
                    }
                }
            });
        }
    }
}

impl Render for Connected {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let main = match self.shown {
            Some(id) => self.render_chat(id, window, cx).into_any_element(),
            None => div().into_any_element(),
        };
        div()
            .relative()
            .size_full()
            .child(
                h_flex()
                    .size_full()
                    .child(self.render_sidebar(cx))
                    .child(div().flex_1().h_full().min_w_0().child(main)),
            )
            .children(self.render_ask(Scope::App, cx))
    }
}
