//! The window once connected: the sidebar, the chat shown beside it, and
//! what the core asks over both.

use std::collections::HashMap;

use commandant_client_core::{ChatId, Core, Edit, Go, Intent, Next, OutputId, Scope, Updates};
use gpui::{
    Context, EventEmitter, FocusHandle, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, ScrollHandle, Styled as _, Task, Window, div, prelude::FluentBuilder as _,
};
use gpui_component::h_flex;

use super::asks::Question;
use super::chat::Pane;
use super::chords::KEYS;
use super::rows::{Change, rows};

/// A core, and what the window keeps of its chats.
pub(super) struct Connected {
    pub(super) core: Core,
    /// The chat in the main pane: none until one is opened, or once the one
    /// shown closes with no other on its node.
    pub(super) shown: Option<ChatId>,
    /// The output in the main pane, over the tasks; none is open while a
    /// chat is shown, nor are the tasks.
    pub(super) output: Option<OutputId>,
    /// Where the output shown is scrolled to.
    pub(super) output_scroll: ScrollHandle,
    /// Whether the nodes are shown, under any output; not while a chat or
    /// the tasks are.
    pub(super) nodes_shown: bool,
    /// The node the nodes view highlights: the one it opened on, or last
    /// acted on.
    pub(super) marked: Option<String>,
    /// The chat last shown on each node, to go back to.
    pub(super) last: HashMap<String, ChatId>,
    /// What the window keeps of each open chat.
    pub(super) panes: HashMap<ChatId, Pane>,
    /// What the window keeps of each open question, by scope.
    pub(super) questions: HashMap<Scope, Question>,
    /// How wide the sidebar is.
    pub(super) sidebar: f32,
    /// Whether the sidebar is shown, or folded away with Ctrl-B.
    pub(super) sidebar_open: bool,
    /// Takes the chords while no chat is shown to take them.
    focus: FocusHandle,
    /// Hands the core what its calls report, for as long as it is shown.
    _updates: Task<()>,
}

impl Connected {
    /// Shows `core`, opening `open` first if it is given.
    pub(super) fn new(
        core: Core,
        mut updates: Updates,
        open: Option<Intent>,
        sidebar: f32,
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
            output: None,
            output_scroll: ScrollHandle::new(),
            nodes_shown: false,
            marked: None,
            last: HashMap::new(),
            panes: HashMap::new(),
            questions: HashMap::new(),
            sidebar,
            sidebar_open: true,
            focus: cx.focus_handle(),
            _updates: pump,
        };
        match open {
            Some(open) => connected.act(open, window, cx),
            None => connected.refocus(window, cx),
        }
        connected
    }

    /// Asks the core for something, and follows where that leads.
    pub(super) fn act(&mut self, intent: Intent, window: &mut Window, cx: &mut Context<Self>) {
        let chats = self.core.chats().len();
        let mut next = self.core.act(intent);
        // A chat closed from the sidebar while another, or the tasks, is
        // shown leaves the view where it is.
        let shown_kept = self.shown.is_none_or(|id| self.core.chat(id).is_some());
        if self.core.chats().len() < chats && shown_kept {
            next.go = None;
        }
        self.apply(next, window, cx);
        self.sync(window, cx);
        cx.notify();
    }

    /// Does the window's part of what the core says comes next.
    pub(super) fn apply(&mut self, next: Next, window: &mut Window, cx: &mut Context<Self>) {
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
        self.go(go, window, cx);
    }

    /// Goes where the core says to, if anywhere.
    pub(super) fn go(&mut self, go: Option<Go>, window: &mut Window, cx: &mut Context<Self>) {
        match go {
            Some(Go::Chat(id)) => self.show(id, window, cx),
            Some(Go::Nodes) => self.show_nodes(None, window, cx),
            // The list, back from any output.
            Some(Go::Tasks) => {
                if let Some(id) = self.output.take() {
                    self.core.act(Intent::CloseOutput(id));
                }
                self.shown = None;
                self.nodes_shown = false;
                self.refocus(window, cx);
            }
            Some(Go::Output(id)) => {
                if let Some(from) = self.output.replace(id).filter(|from| *from != id) {
                    self.core.act(Intent::CloseOutput(from));
                }
                self.output_scroll = ScrollHandle::new();
                self.shown = None;
                self.refocus(window, cx);
            }
            // Its agent started while nothing else was opened, or while
            // its row is marked: open it.
            Some(Go::Ready(node))
                if self.shown.is_none()
                    && self.output.is_none()
                    && self.core.tasks().is_none()
                    && (!self.nodes_shown || self.marked.as_ref() == Some(&node)) =>
            {
                let next = self.core.act(Intent::Open { node, chat: None });
                self.apply(next, window, cx);
            }
            // Quitting, logging out and the help come with the views that
            // go there.
            Some(Go::Quit | Go::LogOut | Go::Help | Go::Ready(_)) | None => {}
        }
    }

    /// Shows a chat, which its turn's end is then no news to.
    pub(super) fn show(&mut self, id: ChatId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = self.core.chat(id) else {
            return;
        };
        self.last.insert(chat.node.id.clone(), id);
        self.close_views();
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

    /// Shows the nodes, marking `node`, else the shown chat's.
    pub(super) fn show_nodes(
        &mut self,
        node: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chat = self.shown.and_then(|id| self.core.chat(id));
        if let Some(node) = node.or_else(|| chat.map(|c| c.node.id.clone())) {
            self.marked = Some(node);
        }
        self.close_views();
        self.shown = None;
        self.nodes_shown = true;
        // The prompt that had the keys is gone with its chat.
        self.refocus(window, cx);
        cx.notify();
    }

    /// Goes back from the output shown to the tasks, or the nodes.
    pub(super) fn close_output(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.output.take() {
            self.act(Intent::CloseOutput(id), window, cx);
        }
    }

    /// Leaves the output, the tasks and the nodes: the output is no longer
    /// watched, nor the tasks refreshed.
    fn close_views(&mut self) {
        self.nodes_shown = false;
        if let Some(id) = self.output.take() {
            self.core.act(Intent::CloseOutput(id));
        }
        if self.core.tasks().is_some() {
            self.core.act(Intent::CloseTasks);
        }
    }

    /// Puts the keys where they go: the question over the window, else the
    /// one over the chat shown, else its prompt, else the chords.
    pub(super) fn refocus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let question = [Some(Scope::App), self.shown.map(Scope::Chat)]
            .into_iter()
            .flatten()
            .find_map(|scope| self.questions.get(&scope));
        if let Some(question) = question {
            return question.focus(window, cx);
        }
        match self.shown.and_then(|id| self.panes.get(&id)) {
            Some(pane) => {
                let composer = pane.composer.clone();
                composer.update(cx, |composer, cx| composer.focus(window, cx));
            }
            None => window.focus(&self.focus, cx),
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
            // Cached, the thread draws what streams in only once told; an
            // empty one names the agent, which changes with no row.
            if change != Change::Unchanged || chat.thread.is_empty() {
                pane.thread.update(cx, |_, cx| cx.notify());
            }
            pane.scroller.update(cx, |scroller, cx| match change {
                Change::Reset(count) => scroller.reset(count, cx),
                Change::Grew {
                    added,
                    written,
                    settled,
                } => {
                    if added > 0 {
                        scroller.append(added, cx);
                    }
                    if settled {
                        return scroller.remeasure(cx);
                    }
                    // The rows still streaming in.
                    for at in written {
                        scroller.remeasure_items(at..at + 1, cx);
                    }
                    let from = count.saturating_sub(added);
                    scroller.remeasure_items(from..count, cx);
                }
                Change::Unchanged => {}
            });
        }
    }
}

impl Render for Connected {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let main = match (self.output, self.shown) {
            (Some(id), _) => self.render_output(id, cx).into_any_element(),
            (None, Some(id)) => self.render_chat(id, window, cx).into_any_element(),
            (None, None) if self.core.tasks().is_some() => self.render_tasks(cx).into_any_element(),
            (None, None) if self.nodes_shown => self.render_nodes(cx).into_any_element(),
            (None, None) => div().into_any_element(),
        };
        let sidebar = self
            .sidebar_open
            .then(|| self.render_sidebar(cx).into_any_element());
        let asked = self
            .shown
            .is_some_and(|id| self.core.ask(Scope::Chat(id)).is_some());
        div()
            .relative()
            .size_full()
            .child(
                h_flex()
                    .size_full()
                    .key_context(KEYS)
                    .track_focus(&self.focus)
                    // A question over the chat shown takes its keys.
                    .when(!asked, |chords| {
                        chords
                            .on_action(cx.listener(Self::on_new_chat))
                            .on_action(cx.listener(Self::on_sessions))
                            .on_action(cx.listener(Self::on_close_chat))
                            .on_action(cx.listener(Self::on_show_nodes))
                            .on_action(cx.listener(Self::on_show_tasks))
                            .on_action(cx.listener(Self::on_previous_chat))
                            .on_action(cx.listener(Self::on_next_chat))
                            .on_action(cx.listener(Self::on_next_effort))
                    })
                    .on_action(cx.listener(Self::on_toggle_sidebar))
                    .on_drag_move(cx.listener(Self::on_drag_sidebar))
                    .children(sidebar)
                    .child(div().flex_1().h_full().min_w_0().child(main)),
            )
            .children(self.render_ask(Scope::App, cx))
    }
}

#[cfg(all(test, feature = "gui-tests"))]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use commandant_client_core::state::fixtures::{node, output};
    use commandant_client_core::{Settings, Update};
    use commandant_proto::{NodeInfo, OutputStream};
    use gpui::{Entity, TestAppContext, VisualTestContext};

    use super::*;
    use crate::gui::thread::Thread;

    /// A window connected to a core knowing `nodes`, a chat open on the
    /// first, and what sends it updates.
    fn connected(
        nodes: Vec<NodeInfo>,
        cx: &mut TestAppContext,
    ) -> (
        Entity<Connected>,
        &mut VisualTestContext,
        tokio::sync::mpsc::UnboundedSender<Update>,
    ) {
        cx.update(gpui_component::init);
        let open = Intent::OpenSession {
            node: nodes[0].id.clone(),
            session: None,
        };
        let core = Core::offline(nodes, Settings::default());
        let (send, updates) = Updates::channel();
        let (connected, cx) = cx.add_window_view(|window, cx| {
            Connected::new(core, updates, Some(open), 240.0, window, cx)
        });
        (connected, cx, send)
    }

    /// The chat shown, and how many times its thread has been told to draw
    /// again from now on.
    fn watch(
        connected: &Entity<Connected>,
        cx: &mut VisualTestContext,
    ) -> (ChatId, Rc<Cell<usize>>) {
        let (id, thread): (ChatId, Entity<Thread>) = connected.read_with(cx, |c, _| {
            let id = c.shown.expect("a chat is shown");
            (id, c.panes[&id].thread.clone())
        });
        let told = Rc::new(Cell::new(0));
        let counted = told.clone();
        cx.update(|_, cx| {
            cx.observe(&thread, move |_, _| counted.set(counted.get() + 1))
                .detach()
        });
        (id, told)
    }

    #[gpui::test]
    fn a_reply_is_drawn_as_it_streams_in(cx: &mut TestAppContext) {
        let (connected, cx, send) = connected(vec![node("n1", true)], cx);
        let (id, told) = watch(&connected, cx);
        connected.update_in(cx, |c, window, cx| {
            c.act(Intent::Submit(id, "count".into()), window, cx)
        });
        let asked = told.get();
        send.send(Update::Chat(id, output(OutputStream::Stdout, b"one")))
            .unwrap();
        cx.run_until_parked();
        assert!(told.get() > asked, "the thread wasn't told of the reply");
        let asked = told.get();
        send.send(Update::Chat(id, output(OutputStream::Stdout, b" two")))
            .unwrap();
        cx.run_until_parked();
        assert!(told.get() > asked, "nor of more of it");
    }

    #[gpui::test]
    fn an_empty_thread_is_drawn_again_as_the_chat_changes(cx: &mut TestAppContext) {
        let nodes = vec![node("n1", true)];
        let (connected, cx, send) = connected(nodes.clone(), cx);
        let (_, told) = watch(&connected, cx);
        // Anything that comes: its agent, say, which the thread names.
        send.send(Update::Nodes(nodes)).unwrap();
        cx.run_until_parked();
        assert!(told.get() > 0);
    }
}

/// The sidebar was dragged to this width.
pub(super) struct Resized(pub(super) f32);

impl EventEmitter<Resized> for Connected {}
