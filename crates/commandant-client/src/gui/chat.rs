//! A chat in the main pane: its header, its thread, what the node is
//! getting ready, the box prompts are written in with the `/command`s
//! completing what is typed, and the chips that choose where the next
//! prompt goes.

use std::collections::HashSet;

use commandant_client_core::chat::Menu;
use commandant_client_core::{Chat, ChatId, Intent, Scope};
use commandant_common::or;
use gpui::{
    AnyElement, App, AppContext as _, Context, DefiniteLength, Div, Entity,
    InteractiveElement as _, IntoElement, ParentElement as _, ScrollHandle,
    StatefulInteractiveElement as _, StyleRefinement, Styled as _, Subscription, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{
    Escape, IndentInline, InputEvent, MoveDown, MoveUp, OutdentInline, Textarea, TextareaState,
};
use gpui_component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme as _, Disableable as _, IconName, Sizable as _, h_flex, v_flex};

use super::asks::{list_row, panel};
use super::connected::Connected;
use super::rows::{Row, Tally};
use super::sidebar::status_dot;
use super::thread::agent_color;

/// The most lines the prompt grows to before it scrolls.
const PROMPT_LINES: usize = 8;

/// The most completions shown before they scroll, in logical pixels.
const COMPLETIONS_HEIGHT: f32 = 240.0;

/// What the window keeps of a chat: what is typed, where its thread is
/// scrolled, and which of its rows are open.
pub(super) struct Pane {
    /// The prompt being written.
    pub(super) composer: Entity<TextareaState>,
    /// Where the thread is scrolled, and how tall its rows are.
    pub(super) scroller: Entity<MessageScrollerState>,
    /// The thread's rows, as last synced.
    pub(super) rows: Vec<Row>,
    /// What the scroller was told of the rows, and what is new since the
    /// reader scrolled up.
    pub(super) tally: Tally,
    /// What the prompt says while empty, as last set.
    placeholder: String,
    /// The thinking and tool rows opened, by their first entry.
    pub(super) open: HashSet<usize>,
    /// The highlighted completion of a `/command` being typed.
    suggested: usize,
    /// Keeps the highlighted completion in sight.
    completions: ScrollHandle,
    _events: Subscription,
}

impl Pane {
    pub(super) fn new(id: ChatId, window: &mut Window, cx: &mut Context<Connected>) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, PROMPT_LINES)
                .submit_on_enter(true)
        });
        let events = cx.subscribe_in(&composer, window, move |this, _, event, window, cx| {
            match event {
                // Shift+Enter inserts a newline instead.
                InputEvent::PressEnter { shift: false, .. } => this.submit(id, window, cx),
                // What's typed changed: back to the best match.
                InputEvent::Change => {
                    if let Some(pane) = this.panes.get_mut(&id) {
                        pane.suggested = 0;
                    }
                }
                _ => {}
            }
        });
        Self {
            composer,
            scroller: cx.new(|cx| MessageScrollerState::new(0, cx)),
            rows: Vec::new(),
            tally: Tally::default(),
            open: HashSet::new(),
            placeholder: String::new(),
            suggested: 0,
            completions: ScrollHandle::new(),
            _events: events,
        }
    }
}

impl Connected {
    /// Sends what is typed in the chat's prompt.
    fn submit(&mut self, id: ChatId, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(chat), Some(pane)) = (self.core.chat(id), self.panes.get(&id)) else {
            return;
        };
        let text = pane.composer.read(cx).value();
        if text.trim().is_empty() {
            return;
        }
        // A partly typed command runs the one highlighted.
        let text = chat.sent_for(&text, pane.suggested);
        // The core clears the prompt once it has taken it.
        self.act(Intent::Submit(id, text), window, cx);
    }

    /// Fills in the completion `at`, ready for its arguments; `false` when
    /// there is none.
    fn complete(
        &mut self,
        id: ChatId,
        at: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let (Some(chat), Some(pane)) = (self.core.chat(id), self.panes.get_mut(&id)) else {
            return false;
        };
        pane.suggested = 0;
        let Some(text) = chat.completed(&pane.composer.read(cx).value(), at) else {
            return false;
        };
        pane.composer.update(cx, |composer, cx| {
            composer.set_value(text, window, cx);
            composer.focus(window, cx);
        });
        true
    }

    /// ↑↓ through the completions; `false` when there are none, so the
    /// prompt moves its cursor instead.
    fn suggest(&mut self, id: ChatId, step: isize, cx: &mut Context<Self>) -> bool {
        let (Some(chat), Some(pane)) = (self.core.chat(id), self.panes.get_mut(&id)) else {
            return false;
        };
        let count = suggestions(chat, pane, cx).len() as isize;
        if count == 0 {
            return false;
        }
        pane.suggested = (pane.suggested as isize + step).rem_euclid(count) as usize;
        pane.completions.scroll_to_item(pane.suggested);
        cx.notify();
        true
    }

    /// Esc: leaves the question over the window, or else over the chat,
    /// unanswered; with none, stops what the chat is at.
    fn escape(&mut self, id: ChatId, window: &mut Window, cx: &mut Context<Self>) {
        let scope = [Scope::App, Scope::Chat(id)]
            .into_iter()
            .find(|scope| self.core.ask(*scope).is_some());
        let intent = scope.map_or(Intent::Cancel(id), Intent::Dismiss);
        self.act(intent, window, cx);
    }

    /// The chat `id`, if it is open: its header, thread and prompt.
    pub(super) fn render_chat(
        &mut self,
        id: ChatId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (Some(chat), Some(pane)) = (self.core.chat(id), self.panes.get_mut(&id)) else {
            return div();
        };
        let header = render_header(chat, cx);
        let thread = render_thread(chat, pane, cx);
        let composer = render_composer(chat, pane, window, cx);
        div()
            .relative()
            .size_full()
            .key_context("Chat")
            .on_action(cx.listener(move |this, _: &Escape, window, cx| this.escape(id, window, cx)))
            .child(
                v_flex()
                    .size_full()
                    .child(header)
                    .child(div().flex_1().min_h_0().child(thread))
                    .children(render_progress(chat, cx))
                    .child(composer)
                    .child(render_chips(chat, cx)),
            )
            .children(self.render_ask(Scope::Chat(id), cx))
    }
}

/// The commands completing what is typed in the chat's prompt, with what
/// they do.
fn suggestions(chat: &Chat, pane: &Pane, cx: &App) -> Vec<(String, String)> {
    chat.completions(&pane.composer.read(cx).value())
}

/// The agent, model and effort the next prompt goes to, each choosing its
/// own; what the session has used on the right.
fn render_chips(chat: &Chat, cx: &mut Context<Connected>) -> Div {
    let id = chat.id;
    let chip = |name: &'static str, label: String, menu: Menu| {
        Button::new(name).ghost().xsmall().label(label).on_click(
            cx.listener(move |this, _, window, cx| this.act(Intent::Menu(id, menu), window, cx)),
        )
    };
    let chips = h_flex()
        .gap_1()
        .child(chip(
            "agent",
            or(chat.agent(), "default agent").to_string(),
            Menu::Agent,
        ))
        .child(chip("model", chat.model_label(), Menu::Model))
        .child(chip("effort", chat.effort_label(), Menu::Effort));
    h_flex()
        .justify_between()
        .gap_2()
        .px_3()
        .pb_2()
        .child(chips)
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(chat.usage().join(" · ")),
        )
}

/// The thread, following its end unless scrolled up; or, while it is
/// empty, where prompts go.
fn render_thread(chat: &Chat, pane: &mut Pane, cx: &mut Context<Connected>) -> AnyElement {
    if chat.thread.is_empty() {
        return div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(cx.theme().muted_foreground)
            .child(format!(
                "Ask {} on {} anything.",
                agent_name(chat),
                chat.node.name
            ))
            .into_any_element();
    }
    if !pane.scroller.read(cx).is_scrolled_up() {
        pane.tally.read();
    }
    let label = match pane.tally.unread() {
        0 => "Latest".to_string(),
        new => format!("{new} new"),
    };
    let mut gap = StyleRefinement::default();
    gap.padding.bottom = Some(DefiniteLength::from(px(12.0)));
    let id = chat.id;
    let renderer =
        cx.processor(move |this, index: usize, window, cx| this.render_row(id, index, window, cx));
    MessageScroller::new(("thread", id), pane.scroller.clone(), renderer)
        .with_row_style(gap)
        .with_jump_button_renderer(move |button| button.label(label))
        .into_any_element()
}

/// The prompt, and the Stop button while the chat is at work.
fn render_composer(
    chat: &Chat,
    pane: &mut Pane,
    window: &mut Window,
    cx: &mut Context<Connected>,
) -> Div {
    let id = chat.id;
    let stopping = chat.stopping();
    let stop = chat.at_work().then(|| {
        Button::new("stop")
            .danger()
            .icon(IconName::Square)
            .label(if stopping { "Stopping…" } else { "Stop" })
            .disabled(stopping)
            .on_click(
                cx.listener(move |this, _, window, cx| this.act(Intent::Cancel(id), window, cx)),
            )
    });
    // Only when it changes: setting it redraws.
    let placeholder = format!("Message {}…", agent_name(chat));
    if pane.placeholder != placeholder {
        pane.placeholder = placeholder.clone();
        pane.composer.update(cx, |composer, cx| {
            composer.set_placeholder(placeholder, window, cx)
        });
    }
    let completions = render_completions(chat, pane, cx);
    h_flex()
        .relative()
        .items_end()
        .gap_2()
        .p_3()
        .border_t_1()
        .border_color(cx.theme().border)
        .capture_action(cx.listener(move |this, _: &MoveUp, _, cx| {
            if this.suggest(id, -1, cx) {
                cx.stop_propagation();
            }
        }))
        .capture_action(cx.listener(move |this, _: &MoveDown, _, cx| {
            if this.suggest(id, 1, cx) {
                cx.stop_propagation();
            }
        }))
        // Tab completes the highlighted command, or else switches agent;
        // Shift+Tab switches back.
        .capture_action(cx.listener(move |this, _: &IndentInline, window, cx| {
            cx.stop_propagation();
            let at = this.panes.get(&id).map_or(0, |pane| pane.suggested);
            if !this.complete(id, at, window, cx) {
                this.act(Intent::CycleAgent(id, 1), window, cx);
            }
        }))
        .capture_action(cx.listener(move |this, _: &OutdentInline, window, cx| {
            cx.stop_propagation();
            this.act(Intent::CycleAgent(id, -1), window, cx);
        }))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(Textarea::new(&pane.composer)),
        )
        .children(stop)
        .children(completions)
}

/// The commands completing what is typed, floating over the thread; a
/// click fills one in.
fn render_completions(chat: &Chat, pane: &Pane, cx: &mut Context<Connected>) -> Option<Div> {
    let suggestions = suggestions(chat, pane, cx);
    if suggestions.is_empty() {
        return None;
    }
    let id = chat.id;
    let color = agent_color(chat, chat.agent(), cx);
    let rows = suggestions.iter().enumerate().map(|(at, (name, about))| {
        let name = div().text_color(color).child(format!("/{name}"));
        list_row(("completion", at), name, about, at == pane.suggested, cx).on_click(cx.listener(
            move |this, _, window, cx| {
                this.complete(id, at, window, cx);
            },
        ))
    });
    Some(
        div().absolute().bottom_full().left_3().right_3().child(
            panel(cx).p_1().child(
                v_flex()
                    .id("completions")
                    .max_h(px(COMPLETIONS_HEIGHT))
                    .overflow_y_scroll()
                    .track_scroll(&pane.completions)
                    .children(rows),
            ),
        ),
    )
}

/// The agent prompts go to, by name.
fn agent_name(chat: &Chat) -> &str {
    match chat.agent() {
        "" => "the agent",
        agent => agent,
    }
}

/// What the node is getting ready for the chat, with a spinner: a copy of
/// a project, the list of them, or a resumed session's earlier turns.
fn render_progress(chat: &Chat, cx: &App) -> Option<Div> {
    let node = &chat.node.name;
    let doing = match &chat.preparing {
        Some(repository) => format!("getting {repository} ready on {node}…"),
        None if chat.listing_projects => format!("listing {node}'s projects…"),
        None if chat.loading_history => "loading the session's earlier messages…".into(),
        None => return None,
    };
    let theme = cx.theme();
    Some(
        h_flex()
            .gap_2()
            .px_4()
            .py_1()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.muted)
            .text_sm()
            .text_color(theme.muted_foreground)
            .child(Spinner::new().xsmall().color(theme.info))
            .child(doing),
    )
}

/// `● my-box  build  opencode · linux/x86_64 · v0.1`, then the session
/// and the directory, which a click offers the node's projects in.
fn render_header(chat: &Chat, cx: &Context<Connected>) -> Div {
    let theme = cx.theme();
    let id = chat.id;
    let node = &chat.node;
    let agent = (!chat.agent().is_empty()).then(|| {
        div()
            .text_sm()
            .text_color(agent_color(chat, chat.agent(), cx))
            .child(chat.agent().to_string())
    });
    let harness = node.harnesses.first().map_or("no agent", String::as_str);
    let mut facts = vec![harness.to_string(), format!("{}/{}", node.os, node.arch)];
    if !node.version.is_empty() {
        facts.push(format!("v{}", node.version));
    }
    let session = match chat.settings.session_id.as_str() {
        "" => "new session",
        id => id,
    };
    let cwd = match chat.settings.cwd.as_str() {
        "" => "worker's directory",
        cwd => cwd,
    };
    v_flex()
        .px_4()
        .py_2()
        .gap_0p5()
        .border_b_1()
        .border_color(theme.border)
        .child(
            h_flex()
                .gap_2()
                .child(status_dot(node.online, cx))
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(node.name.clone()),
                )
                .children(agent)
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .truncate()
                        .child(facts.join(" · ")),
                ),
        )
        .child(
            h_flex()
                .gap_1()
                .min_w_0()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(div().flex_none().child(format!("{session} ·")))
                .child(
                    div()
                        .id("cwd")
                        .min_w_0()
                        .truncate()
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme.foreground))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.act(Intent::Projects(id), window, cx)
                        }))
                        .child(cwd.to_string()),
                ),
        )
}
