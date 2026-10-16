//! What the core asks, drawn over what it is about: a chat's question over
//! its pane, which stays there while other chats are shown, and the app's
//! over the whole window.

use commandant_client_core::{Ask, Intent, Scope};
use gpui::{
    App, AppContext as _, Context, Div, ElementId, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, ScrollHandle, Stateful, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::clipboard::Clipboard;
use gpui_component::input::{Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

use super::connected::Connected;
use crate::tui::picker::Picker;

/// The most choices shown before the list scrolls, in logical pixels.
const LIST_HEIGHT: f32 = 320.0;

/// Where a question that takes typing is typed: the filter of what there is
/// to choose from, or the line asked for.
pub(super) struct Field {
    pub(super) input: Entity<InputState>,
    /// The choices the filter narrows down, if there are any.
    picker: Option<Picker>,
    /// Keeps the highlighted choice in sight.
    scroll: ScrollHandle,
    _events: Subscription,
}

impl Field {
    fn new(
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
            input,
            picker,
            scroll: ScrollHandle::new(),
            _events: events,
        }
    }

    /// The field to ask `ask` in `scope` with, given the one `shown`: the
    /// same while the question is, its choices kept up to date.
    pub(super) fn sync(
        ask: Option<&Ask>,
        shown: Option<Self>,
        scope: Scope,
        window: &mut Window,
        cx: &mut Context<Connected>,
    ) -> Option<Self> {
        match (ask?, shown) {
            (Ask::Choose(choices), Some(mut shown))
                if shown.picker.as_ref().is_some_and(|p| p.id() == choices.id) =>
            {
                shown.picker = Picker::sync(shown.picker.take(), Some(choices));
                Some(shown)
            }
            (Ask::Enter { .. }, Some(shown)) if shown.picker.is_none() => Some(shown),
            (Ask::Choose(choices), _) => {
                let picker = Picker::sync(None, Some(choices));
                Some(Self::new(scope, picker, false, window, cx))
            }
            (Ask::Enter { secret, .. }, _) => Some(Self::new(scope, None, *secret, window, cx)),
            _ => None,
        }
    }

    pub(super) fn focus(&self, window: &mut Window, cx: &mut Context<Connected>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }
}

impl Connected {
    fn picker(&mut self, scope: Scope) -> Option<&mut Picker> {
        self.fields.get_mut(&scope)?.picker.as_mut()
    }

    /// Moves the highlighted choice in `scope`, keeping it in sight.
    fn step(&mut self, scope: Scope, rows: isize, cx: &mut Context<Self>) {
        let Some(field) = self.fields.get_mut(&scope) else {
            return;
        };
        if let Some(picker) = &mut field.picker {
            picker.list.step(rows);
            field.scroll.scroll_to_item(picker.list.selected);
            cx.notify();
        }
    }

    /// Enter in a question's field: the highlighted choice, or the line.
    fn answer(&mut self, scope: Scope, window: &mut Window, cx: &mut Context<Self>) {
        let Some(field) = self.fields.get(&scope) else {
            return;
        };
        let intent = match &field.picker {
            Some(picker) => match picker.list.picked() {
                Some(choice) => Intent::Choose(scope, choice.value.clone()),
                None => return,
            },
            None => Intent::Enter(scope, field.input.read(cx).value().to_string()),
        };
        self.act(intent, window, cx);
    }

    /// What is asked in `scope`, over what it is about, if anything.
    pub(super) fn render_ask(&self, scope: Scope, cx: &mut Context<Self>) -> Option<Div> {
        let ask = self.core.ask(scope)?;
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let (overlay, muted_bg, mono) =
            (theme.overlay, theme.muted, theme.mono_font_family.clone());
        let card = panel(cx).w(px(440.0)).gap_3().p_4();
        let card =
            match ask {
                Ask::Choose(_) => {
                    let field = self.fields.get(&scope)?;
                    let picker = field.picker.as_ref()?;
                    let loading = picker
                        .loading()
                        .then(|| div().text_sm().text_color(muted).child("loading…"));
                    card.child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(picker.title()),
                    )
                    .child(Input::new(&field.input))
                    .child(render_choices(scope, field, picker, cx))
                    .children(loading)
                }
                Ask::Enter { label, secret, .. } => {
                    let field = self.fields.get(&scope)?;
                    let hint = match secret {
                        true => "Enter saves it, hidden · Esc cancels",
                        false => "Enter saves it · Esc cancels",
                    };
                    card.child(format!("Paste {label}"))
                        .child(Input::new(&field.input))
                        .child(div().text_xs().text_color(muted).child(hint))
                }
                Ask::Confirm { text, .. } => card.child(text.clone()).child(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(Button::new("no").ghost().label("No").on_click(cx.listener(
                            move |this, _, window, cx| this.act(Intent::Dismiss(scope), window, cx),
                        )))
                        .child(
                            Button::new("yes")
                                .primary()
                                .label("Yes")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.act(Intent::Confirm(scope), window, cx)
                                })),
                        ),
                ),
                Ask::Show { title, text } => card
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(title.clone()),
                    )
                    .child(
                        div()
                            .id("shown")
                            .max_h(px(LIST_HEIGHT))
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
                            .child(Button::new("close").ghost().label("Close").on_click(
                                cx.listener(move |this, _, window, cx| {
                                    this.act(Intent::Dismiss(scope), window, cx)
                                }),
                            )),
                    ),
            };
        Some(
            div()
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(overlay)
                .on_action(cx.listener(move |this, _: &Escape, window, cx| {
                    this.act(Intent::Dismiss(scope), window, cx)
                }))
                .capture_action(cx.listener(move |this, _: &MoveUp, _, cx| {
                    this.step(scope, -1, cx);
                    cx.stop_propagation();
                }))
                .capture_action(cx.listener(move |this, _: &MoveDown, _, cx| {
                    this.step(scope, 1, cx);
                    cx.stop_propagation();
                }))
                .child(card),
        )
    }
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

/// One of a list to pick from: `label`, then what it is, marked while
/// `selected`.
pub(super) fn list_row(
    id: impl Into<ElementId>,
    label: impl IntoElement,
    detail: &str,
    selected: bool,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    let row = h_flex()
        .id(id)
        .gap_2()
        .px_2()
        .py_0p5()
        .rounded_md()
        .cursor_pointer()
        .hover(|s| s.bg(theme.accent))
        .child(div().flex_none().child(label))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(detail.to_string()),
        );
    match selected {
        true => row.bg(theme.accent).text_color(theme.accent_foreground),
        false => row,
    }
}

/// The choices the filter leaves, the highlighted one marked; a click
/// chooses one.
fn render_choices(
    scope: Scope,
    field: &Field,
    picker: &Picker,
    cx: &mut Context<Connected>,
) -> impl IntoElement {
    let rows = picker.list.shown().enumerate().map(|(at, choice)| {
        let value = choice.value.clone();
        let selected = at == picker.list.selected;
        list_row(
            ("choice", at),
            choice.label.clone(),
            &choice.detail,
            selected,
            cx,
        )
        .on_click(cx.listener(move |this, _, window, cx| {
            this.act(Intent::Choose(scope, value.clone()), window, cx)
        }))
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
        .max_h(px(LIST_HEIGHT))
        .overflow_y_scroll()
        .track_scroll(&field.scroll)
        .children(rows)
        .children(empty)
}
