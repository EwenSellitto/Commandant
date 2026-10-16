//! What the core asks, drawn over what it is about: a chat's question over
//! its pane, which stays there while other chats are shown, and the app's
//! over the whole window.

use commandant_client_core::{Ask, Intent, Scope};
use gpui::{
    App, AppContext as _, Context, Div, ElementId, Entity, FocusHandle, FontWeight,
    InteractiveElement as _, IntoElement, KeyBinding, ParentElement as _, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::clipboard::Clipboard;
use gpui_component::input::{Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use gpui_component::list::ListItem;
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use super::connected::Connected;
use crate::picker::Picker;

/// Where the keys of a question go.
const KEYS: &str = "Ask";

// Enter and Esc on a question with nothing to type.
gpui::actions!(ask, [Yes, No]);

/// The tallest a question's choices or text grow before they scroll, in
/// logical pixels.
const MAX_HEIGHT: f32 = 320.0;

/// Enter and Esc for a question with nothing to type: yes (or done), and no.
pub(super) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("enter", Yes, Some(KEYS)),
        KeyBinding::new("escape", No, Some(KEYS)),
    ]);
}

/// What the window keeps of a question while it is open: where its keys
/// go, and what is typed for one that takes typing.
pub(super) struct Question {
    /// Takes the keys of a question with nothing to type; tells questions
    /// apart.
    pub(super) focus: FocusHandle,
    /// The line typed: the filter of the choices, or what is asked for.
    input: Option<Entity<InputState>>,
    /// The choices the filter narrows down, if there are any.
    picker: Option<Picker>,
    /// Keeps the highlighted choice in sight.
    scroll: ScrollHandle,
    _events: Option<Subscription>,
}

impl Question {
    /// One that takes typing: `picker`'s filter, or else a line, masked
    /// if `secret`.
    fn typed(
        scope: Scope,
        picker: Option<Picker>,
        secret: bool,
        window: &mut Window,
        cx: &mut Context<Connected>,
    ) -> Self {
        let filter = picker.as_ref().map_or("", |p| &p.list.filter).to_string();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(secret)
                .default_value(filter)
        });
        let events =
            cx.subscribe_in(
                &input,
                window,
                move |this, input, event, window, cx| match event {
                    InputEvent::Change => {
                        let filter = input.read(cx).value();
                        if let Some(picker) = this.picker(scope) {
                            picker.list.set_filter(&filter);
                        }
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => this.answer(scope, window, cx),
                    _ => {}
                },
            );
        Self {
            input: Some(input),
            picker,
            _events: Some(events),
            ..Self::untyped(cx)
        }
    }

    /// One to say yes to, or read.
    fn untyped(cx: &mut Context<Connected>) -> Self {
        Self {
            focus: cx.focus_handle(),
            input: None,
            picker: None,
            scroll: ScrollHandle::new(),
            _events: None,
        }
    }

    /// The question to ask `ask` in `scope` with, given the one `shown`:
    /// the same while the question is, its choices kept up to date.
    pub(super) fn sync(
        ask: Option<&Ask>,
        shown: Option<Self>,
        scope: Scope,
        window: &mut Window,
        cx: &mut Context<Connected>,
    ) -> Option<Self> {
        Some(match (ask?, shown) {
            (Ask::Choose(choices), Some(mut shown))
                if shown.picker.as_ref().is_some_and(|p| p.id() == choices.id) =>
            {
                shown.picker = Picker::sync(shown.picker.take(), Some(choices));
                shown
            }
            (Ask::Enter { .. }, Some(shown)) if shown.input.is_some() && shown.picker.is_none() => {
                shown
            }
            (Ask::Confirm { .. } | Ask::Show { .. }, Some(shown)) if shown.input.is_none() => shown,
            (Ask::Choose(choices), _) => {
                Self::typed(scope, Picker::sync(None, Some(choices)), false, window, cx)
            }
            (Ask::Enter { secret, .. }, _) => Self::typed(scope, None, *secret, window, cx),
            (Ask::Confirm { .. } | Ask::Show { .. }, _) => Self::untyped(cx),
        })
    }

    /// Gives it the keys.
    pub(super) fn focus(&self, window: &mut Window, cx: &mut Context<Connected>) {
        match &self.input {
            Some(input) => input.update(cx, |input, cx| input.focus(window, cx)),
            None => window.focus(&self.focus, cx),
        }
    }
}

impl Connected {
    fn picker(&mut self, scope: Scope) -> Option<&mut Picker> {
        self.questions.get_mut(&scope)?.picker.as_mut()
    }

    /// Moves the highlighted choice in `scope`, keeping it in sight.
    fn step(&mut self, scope: Scope, rows: isize, cx: &mut Context<Self>) {
        let Some(question) = self.questions.get_mut(&scope) else {
            return;
        };
        if let Some(picker) = &mut question.picker {
            picker.list.step(rows);
            question.scroll.scroll_to_item(picker.list.selected);
            cx.notify();
        }
    }

    /// Enter in a question's field: the highlighted choice, or the line.
    fn answer(&mut self, scope: Scope, window: &mut Window, cx: &mut Context<Self>) {
        let Some(question) = self.questions.get(&scope) else {
            return;
        };
        let intent = match (&question.picker, &question.input) {
            (Some(picker), _) => match picker.list.picked() {
                Some(choice) => Intent::Choose(scope, choice.value.clone()),
                None => return,
            },
            (None, Some(input)) => Intent::Enter(scope, input.read(cx).value().to_string()),
            (None, None) => return,
        };
        self.act(intent, window, cx);
    }

    /// What is asked in `scope`, over what it is about, if anything.
    pub(super) fn render_ask(&self, scope: Scope, cx: &mut Context<Self>) -> Option<Div> {
        let (ask, question) = (self.core.ask(scope)?, self.questions.get(&scope)?);
        let theme = cx.theme();
        let (muted, muted_bg, overlay) = (theme.muted_foreground, theme.muted, theme.overlay);
        let mono = theme.mono_font_family.clone();
        let card = panel(cx).w(px(440.0)).gap_3().p_4().occlude();
        let card = match ask {
            Ask::Choose(_) => {
                let (picker, input) = (question.picker.as_ref()?, question.input.as_ref()?);
                let loading = picker
                    .loading()
                    .then(|| div().text_sm().text_color(muted).child("loading…"));
                card.child(title(picker.title()))
                    .child(Input::new(input))
                    .child(render_choices(scope, question, picker, cx))
                    .children(loading)
            }
            Ask::Enter { label, secret, .. } => {
                let hint = match secret {
                    true => "Enter saves it, hidden · Esc cancels",
                    false => "Enter saves it · Esc cancels",
                };
                card.child(format!("Enter {label}"))
                    .child(Input::new(question.input.as_ref()?))
                    .child(div().text_xs().text_color(muted).child(hint))
            }
            Ask::Confirm { text, .. } => card
                .track_focus(&question.focus)
                .on_action(act_on::<Yes>(Intent::Confirm(scope), cx))
                .child(text.clone())
                .child(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("no")
                                .ghost()
                                .label("No")
                                .on_click(act_on(Intent::Dismiss(scope), cx)),
                        )
                        .child(
                            Button::new("yes")
                                .primary()
                                .label("Yes")
                                .on_click(act_on(Intent::Confirm(scope), cx)),
                        ),
                ),
            Ask::Show { title: name, text } => card
                .track_focus(&question.focus)
                .on_action(act_on::<Yes>(Intent::Dismiss(scope), cx))
                .child(title(name.clone()))
                .child(
                    div()
                        .id("shown")
                        .max_h(px(MAX_HEIGHT))
                        .overflow_y_scroll()
                        .p_2()
                        .rounded_md()
                        .bg(muted_bg)
                        .font_family(mono)
                        .text_sm()
                        .child(text.clone()),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(Clipboard::new("copy").tooltip("Copy").value(text.clone()))
                        .child(
                            Button::new("close")
                                .ghost()
                                .label("Close")
                                .on_click(act_on(Intent::Dismiss(scope), cx)),
                        ),
                ),
        };
        let over = div()
            .absolute()
            .key_context(KEYS)
            .flex()
            .justify_center()
            .on_action(act_on::<Escape>(Intent::Dismiss(scope), cx))
            .on_action(act_on::<No>(Intent::Dismiss(scope), cx))
            .capture_action(cx.listener(move |this, _: &MoveUp, _, cx| {
                this.step(scope, -1, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(move |this, _: &MoveDown, _, cx| {
                this.step(scope, 1, cx);
                cx.stop_propagation();
            }));
        let over = match ask {
            // What the line is for, a page to get a code from say, is in the
            // thread, which stays usable.
            Ask::Enter { .. } => over.bottom_0().left_0().right_0().p_3(),
            _ => over.inset_0().occlude().items_center().bg(overlay),
        };
        Some(over.child(card))
    }
}

/// Does `intent` on an `E`: a click, a key.
fn act_on<E: 'static>(
    intent: Intent,
    cx: &Context<Connected>,
) -> impl Fn(&E, &mut Window, &mut App) + 'static {
    cx.listener(move |this, _: &E, window, cx| this.act(intent.clone(), window, cx))
}

fn title(text: impl Into<SharedString>) -> Div {
    div().font_weight(FontWeight::SEMIBOLD).child(text.into())
}

/// What floats over the window: a question, or the completions.
pub(super) fn panel(cx: &App) -> Div {
    let theme = cx.theme();
    v_flex()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .shadow_lg()
}

/// One of a list to pick from: `label`, then what it is.
pub(super) fn list_row(
    id: impl Into<ElementId>,
    label: impl IntoElement,
    detail: &str,
    selected: bool,
    cx: &App,
) -> ListItem {
    let detail = div()
        .flex_1()
        .min_w_0()
        .truncate()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(detail.to_string());
    ListItem::new(id).selected(selected).child(
        h_flex()
            .gap_2()
            .child(div().flex_none().child(label))
            .child(detail),
    )
}

/// The choices the filter leaves, the highlighted one marked; a click
/// chooses one.
fn render_choices(
    scope: Scope,
    question: &Question,
    picker: &Picker,
    cx: &mut Context<Connected>,
) -> impl IntoElement {
    let rows = picker.list.shown().enumerate().map(|(at, choice)| {
        let selected = at == picker.list.selected;
        let choose = Intent::Choose(scope, choice.value.clone());
        list_row(
            ("choice", at),
            choice.label.clone(),
            &choice.detail,
            selected,
            cx,
        )
        .on_click(act_on(choose, cx))
    });
    let empty = (picker.list.shown_len() == 0).then(|| {
        div()
            .px_2()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("Nothing matches")
    });
    v_flex()
        .id("choices")
        .max_h(px(MAX_HEIGHT))
        .overflow_y_scroll()
        .track_scroll(&question.scroll)
        .children(rows)
        .children(empty)
}
