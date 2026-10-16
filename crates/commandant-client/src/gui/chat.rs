//! A chat in the main pane: its header, its thread and the box prompts are
//! written in.

use std::collections::HashSet;

use commandant_client_core::{Ask, Chat, ChatId, Intent, Scope};
use gpui::{
    AnyElement, AppContext as _, Context, DefiniteLength, Div, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, StyleRefinement, Styled as _, Subscription, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Escape, InputEvent, Textarea, TextareaState};
use gpui_component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_component::{ActiveTheme as _, Disableable as _, IconName, h_flex, v_flex};

use super::connected::Connected;
use super::rows::{Row, Tally};
use super::sidebar::status_dot;
use super::thread::agent_color;

/// The most lines the prompt grows to before it scrolls.
const PROMPT_LINES: usize = 8;

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
    _enter: Subscription,
}

impl Pane {
    pub(super) fn new(id: ChatId, window: &mut Window, cx: &mut Context<Connected>) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, PROMPT_LINES)
                .submit_on_enter(true)
        });
        // Shift+Enter inserts a newline instead.
        let enter = cx.subscribe_in(&composer, window, move |this, _, event, window, cx| {
            if let InputEvent::PressEnter { shift: false, .. } = event {
                this.submit(id, window, cx);
            }
        });
        Self {
            composer,
            scroller: cx.new(|cx| MessageScrollerState::new(0, cx)),
            rows: Vec::new(),
            tally: Tally::default(),
            open: HashSet::new(),
            placeholder: String::new(),
            _enter: enter,
        }
    }
}

impl Connected {
    /// Sends what is typed in the chat's prompt.
    fn submit(&mut self, id: ChatId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.panes.get(&id) else {
            return;
        };
        let text = pane.composer.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        // The core clears the prompt once it has taken it.
        self.act(Intent::Submit(id, text), window, cx);
    }

    /// Esc: says no to the confirm over the window, or else stops what
    /// the chat is at. Other questions aren't shown yet, so it leaves them.
    fn escape(&mut self, id: ChatId, window: &mut Window, cx: &mut Context<Self>) {
        let intent = match self.core.ask(Scope::App) {
            Some(Ask::Confirm { .. }) => Intent::Dismiss(Scope::App),
            _ => Intent::Cancel(id),
        };
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
            .size_full()
            .key_context("Chat")
            .on_action(cx.listener(move |this, _: &Escape, window, cx| this.escape(id, window, cx)))
            .child(
                v_flex()
                    .size_full()
                    .child(header)
                    .child(div().flex_1().min_h_0().child(thread))
                    .child(composer),
            )
    }
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
    h_flex()
        .items_end()
        .gap_2()
        .p_3()
        .border_t_1()
        .border_color(cx.theme().border)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(Textarea::new(&pane.composer)),
        )
        .children(stop)
}

/// The agent prompts go to, by name.
fn agent_name(chat: &Chat) -> &str {
    match chat.agent() {
        "" => "the agent",
        agent => agent,
    }
}

/// `● my-box  build  opencode · linux/x86_64 · v0.1`, then the session
/// and the directory.
fn render_header(chat: &Chat, cx: &Context<Connected>) -> Div {
    let theme = cx.theme();
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
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .truncate()
                .child(format!("{session} · {cwd}")),
        )
}
