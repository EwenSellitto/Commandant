//! The TUI's key chords, doing the same here: a new chat, the sessions,
//! closing, the nodes, the tasks, the chat before or after, the next
//! effort; and Ctrl-B, which folds the sidebar away.

use commandant_client_core::{ChatId, Intent};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{App, Context, Div, InteractiveElement as _, KeyBinding, Window};

use super::connected::Connected;

gpui_kit::actions!(
    chord,
    [
        NewChat,
        Sessions,
        CloseChat,
        ShowNodes,
        ShowTasks,
        PreviousChat,
        NextChat,
        NextEffort,
        ToggleSidebar,
    ]
);

/// Where the chords work: the sidebar and the views. A question takes the
/// keys instead, as the TUI's does: one over the whole window is outside
/// it, and one over the chat shown leaves only Ctrl-B.
pub(super) const KEYS: &str = "Connected";

/// Binds the chords where they work.
pub(super) fn bind_keys(cx: &mut App) {
    // Over the prompt's own bindings too: Ctrl-H replaces in a text box,
    // and Alt-←/→ moves by word on macOS.
    let over_input = "Connected > Input";
    cx.bind_keys([
        KeyBinding::new("ctrl-n", NewChat, Some(KEYS)),
        KeyBinding::new("ctrl-o", Sessions, Some(KEYS)),
        KeyBinding::new("ctrl-w", CloseChat, Some(KEYS)),
        KeyBinding::new("ctrl-g", ShowNodes, Some(KEYS)),
        KeyBinding::new("ctrl-h", ShowTasks, Some(KEYS)),
        KeyBinding::new("ctrl-h", ShowTasks, Some(over_input)),
        KeyBinding::new("alt-left", PreviousChat, Some(KEYS)),
        KeyBinding::new("alt-left", PreviousChat, Some(over_input)),
        KeyBinding::new("alt-right", NextChat, Some(KEYS)),
        KeyBinding::new("alt-right", NextChat, Some(over_input)),
        KeyBinding::new("ctrl-t", NextEffort, Some(KEYS)),
        KeyBinding::new("ctrl-b", ToggleSidebar, Some(KEYS)),
    ]);
}

impl Connected {
    /// Has `keys` do the chords: only Ctrl-B while a question is `asked`
    /// over the chat shown.
    pub(super) fn on_chords(&self, keys: Div, asked: bool, cx: &mut Context<Self>) -> Div {
        keys.when(!asked, |keys| {
            keys.on_action(cx.listener(|this, _: &NewChat, window, cx| {
                this.for_shown(Intent::NewChat, window, cx);
            }))
            .on_action(cx.listener(|this, _: &Sessions, window, cx| {
                this.for_shown(Intent::Sessions, window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseChat, window, cx| {
                this.for_shown(Intent::Close, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowNodes, window, cx| {
                this.show_nodes(None, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowTasks, window, cx| {
                this.show_tasks(window, cx);
            }))
            .on_action(cx.listener(|this, _: &PreviousChat, window, cx| {
                this.show_beside(-1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &NextChat, window, cx| {
                this.show_beside(1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &NextEffort, window, cx| {
                this.for_shown(Intent::CycleEffort, window, cx);
            }))
        })
        .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
            this.sidebar_open = !this.sidebar_open;
            cx.notify();
        }))
    }

    /// Asks the core for something about the chat shown, if one is.
    fn for_shown(
        &mut self,
        intent: impl FnOnce(ChatId) -> Intent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.shown {
            self.act(intent(id), window, cx);
        }
    }

    /// Shows the tasks: back from an output, kept as filtered if already
    /// listed, else listed anew.
    pub(super) fn show_tasks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.output.is_some() && self.core.tasks().is_some() {
            self.close_output(window, cx);
        } else if self.core.tasks().is_none() {
            self.act(Intent::ListTasks { node: None }, window, cx);
        }
    }

    /// Shows the chat `step` away from the one shown, among its node's.
    fn show_beside(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let beside = self.shown.and_then(|id| self.core.chat_beside(id, step));
        if let Some(id) = beside {
            self.show(id, window, cx);
            cx.notify();
        }
    }
}
