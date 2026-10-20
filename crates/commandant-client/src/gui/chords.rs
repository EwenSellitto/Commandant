//! The TUI's key chords, doing the same here: a new chat, the sessions,
//! closing, the nodes, the tasks, the chat before or after, the next
//! effort; and Ctrl-B, which folds the sidebar away.

use commandant_client_core::{ChatId, Go, Intent, Next, cycle};
use gpui::{App, Context, KeyBinding, Window};

use super::connected::Connected;

gpui::actions!(
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

/// Where the chords work: the sidebar and the views, not under a question
/// over the whole window, which takes the keys as the TUI's does.
pub(super) const KEYS: &str = "Connected";

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
    /// What the chat shown asks for, if one is.
    fn in_shown(
        &mut self,
        intent: impl FnOnce(ChatId) -> Intent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.shown {
            self.act(intent(id), window, cx);
        }
    }

    pub(super) fn on_new_chat(&mut self, _: &NewChat, window: &mut Window, cx: &mut Context<Self>) {
        self.in_shown(Intent::NewChat, window, cx);
    }

    pub(super) fn on_sessions(
        &mut self,
        _: &Sessions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.in_shown(Intent::Sessions, window, cx);
    }

    pub(super) fn on_close_chat(
        &mut self,
        _: &CloseChat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.in_shown(Intent::Close, window, cx);
    }

    pub(super) fn on_show_nodes(
        &mut self,
        _: &ShowNodes,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let next = Next {
            go: Some(Go::Nodes),
            prompt: None,
            sent: None,
        };
        self.apply(next, window, cx);
        cx.notify();
    }

    /// Nothing yet: the tasks come with their view, and listing them now
    /// would keep them refreshing with nobody to see them.
    pub(super) fn on_show_tasks(&mut self, _: &ShowTasks, _: &mut Window, _: &mut Context<Self>) {}

    pub(super) fn on_previous_chat(
        &mut self,
        _: &PreviousChat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_chat(-1, window, cx);
    }

    pub(super) fn on_next_chat(
        &mut self,
        _: &NextChat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_chat(1, window, cx);
    }

    /// Shows the chat `step` away from the one shown, among its node's.
    fn cycle_chat(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = self.shown.and_then(|id| self.core.chat(id)) else {
            return;
        };
        let ids: Vec<ChatId> = self.core.chats_on(&chat.node.id).map(|c| c.id).collect();
        if let Some(next) = cycle(&ids, chat.id, step) {
            self.show(next, window, cx);
            cx.notify();
        }
    }

    pub(super) fn on_next_effort(
        &mut self,
        _: &NextEffort,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.in_shown(Intent::CycleEffort, window, cx);
    }

    pub(super) fn on_toggle_sidebar(
        &mut self,
        _: &ToggleSidebar,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar_open = !self.sidebar_open;
        cx.notify();
    }
}
