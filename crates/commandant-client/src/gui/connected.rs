//! The window once connected: the sidebar, the chat shown beside it, and
//! what the core asks to confirm over both.

use std::collections::HashMap;

use commandant_client_core::{Ask, ChatId, Core, Edit, Go, Intent, Next, Scope, Updates};
use gpui::{Context, IntoElement, ParentElement as _, Render, Styled as _, Task, Window, div, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

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
                    this.sync(cx);
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
        self.sync(cx);
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
            // Quitting, the help and the nodes view come with the views
            // that go there.
            Some(Go::Quit | Go::Help | Go::Ready(_)) | None => {}
        }
    }

    /// Shows a chat, which its turn's end is then no news to.
    pub(super) fn show(&mut self, id: ChatId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = self.core.chat(id) else {
            return;
        };
        self.last.insert(chat.node.id.clone(), id);
        self.shown = Some(id);
        self.core.act(Intent::Seen(id));
        let composer = self.pane(id, window, cx).composer.clone();
        composer.update(cx, |composer, cx| composer.focus(window, cx));
        // A pane new to the window learns its thread.
        self.sync(cx);
    }

    /// What the window keeps of the chat `id`, kept from now on.
    fn pane(&mut self, id: ChatId, window: &mut Window, cx: &mut Context<Self>) -> &mut Pane {
        self.panes
            .entry(id)
            .or_insert_with(|| Pane::new(id, window, cx))
    }

    /// Forgets closed chats, and tells each thread's scroller how its
    /// thread has changed.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let core = &self.core;
        self.panes.retain(|id, _| core.chat(*id).is_some());
        for (id, pane) in &mut self.panes {
            let Some(chat) = core.chat(*id) else {
                continue;
            };
            pane.rows = rows(&chat.thread);
            let count = pane.rows.len();
            let scrolled_up = pane.scroller.read(cx).is_scrolled_up();
            let change = pane.tally.change(chat, count, scrolled_up);
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

    /// Asks the core what it asks to confirm, if anything, over the window.
    fn render_confirm(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let Some(Ask::Confirm { text, .. }) = self.core.ask(Scope::App) else {
            return None;
        };
        let theme = cx.theme();
        let card = v_flex()
            .w(px(360.0))
            .gap_4()
            .p_5()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .child(text.clone())
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(Button::new("no").ghost().label("No").on_click(cx.listener(
                        |this, _, window, cx| this.act(Intent::Dismiss(Scope::App), window, cx),
                    )))
                    .child(
                        Button::new("yes")
                            .primary()
                            .label("Yes")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.act(Intent::Confirm(Scope::App), window, cx)
                            })),
                    ),
            );
        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme.overlay)
                .child(card),
        )
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
            .children(self.render_confirm(cx))
    }
}
