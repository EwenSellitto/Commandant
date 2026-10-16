//! The sidebar: every node, live from the core's node poll, with its open
//! chats beneath it.

use commandant_client_core::chat::Unseen;
use commandant_client_core::{Chat, Intent};
use commandant_proto::NodeInfo;
use gpui::{
    App, Context, Div, InteractiveElement as _, IntoElement, ParentElement as _, Stateful,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};

use super::connected::Connected;

/// The sidebar's width, in logical pixels.
const WIDTH: f32 = 240.0;

/// Room for a chat's mark, so titles line up.
const MARK: f32 = 14.0;

/// A node's dot: filled while it is online, hollow while not.
pub(super) fn status_dot(online: bool, cx: &App) -> Div {
    let theme = cx.theme();
    let dot = div().size(px(8.0)).flex_none().rounded_full();
    match online {
        true => dot.bg(theme.success),
        false => dot.border_1().border_color(theme.muted_foreground),
    }
}

impl Connected {
    /// Every node and its chats, then the core's notice.
    pub(super) fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let nodes = self.core.nodes();
        let empty = nodes.is_empty().then(|| {
            div()
                .px_2()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("No nodes yet. Add a worker with `commandant-server worker <link>`.")
        });
        let notice = (!self.core.notice().is_empty()).then(|| {
            div()
                .px_2()
                .text_sm()
                .text_color(theme.warning)
                .child(self.core.notice().to_string())
        });
        let mut tree = Vec::new();
        for (at, node) in nodes.iter().enumerate() {
            tree.push(self.node_row(at, node, cx));
            for chat in self.core.chats_on(&node.id) {
                tree.push(self.chat_row(chat, cx));
            }
        }
        v_flex()
            .w(px(WIDTH))
            .h_full()
            .flex_none()
            .p_2()
            .gap_1()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                v_flex()
                    .id("nodes")
                    .flex_1()
                    .gap_1()
                    .overflow_y_scroll()
                    .children(tree)
                    .children(empty),
            )
            .children(notice)
    }

    /// A node's status dot and name, dimmed while it is offline and turning
    /// while it starts its agent; a click opens its last chat, `[+]` a new
    /// one.
    fn node_row(&self, at: usize, node: &NodeInfo, cx: &Context<Self>) -> Stateful<Div> {
        let theme = cx.theme();
        let id = node.id.clone();
        let open = cx.listener(move |this, _, window, cx| {
            let chat = this.last.get(&id).copied();
            let node = id.clone();
            this.act(Intent::Open { node, chat }, window, cx);
        });
        let id = node.id.clone();
        let new = node.online.then(|| {
            Button::new(("new-chat", at))
                .ghost()
                .xsmall()
                .icon(IconName::Plus)
                .tooltip("New chat")
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    let node = id.clone();
                    this.act(
                        Intent::OpenSession {
                            node,
                            session: None,
                        },
                        window,
                        cx,
                    );
                }))
        });
        let row = h_flex()
            .id(("node", at))
            .gap_2()
            .px_2()
            .h(px(28.0))
            .rounded_md()
            .cursor_pointer()
            .hover(|s| s.bg(theme.sidebar_accent))
            .on_click(open)
            .child(status_dot(node.online, cx))
            .child(div().flex_1().truncate().child(node.name.clone()))
            .children(
                self.core
                    .starting(&node.id)
                    .then(|| Spinner::new().xsmall().color(theme.info)),
            )
            .children(new);
        if node.online {
            row
        } else {
            row.text_color(theme.muted_foreground)
        }
    }

    /// A chat's title and what it is at, how it ended out of sight, or that
    /// it needs input; a hover × closes it.
    fn chat_row(&self, chat: &Chat, cx: &Context<Self>) -> Stateful<Div> {
        let theme = cx.theme();
        let id = chat.id;
        let mark = div()
            .w(px(MARK))
            .flex_none()
            .flex()
            .items_center()
            .justify_center();
        let mark = match (chat.at_work(), chat.unseen) {
            // A question waits for an answer, whatever else goes on.
            _ if chat.ask().is_some() => {
                mark.child(Icon::new(IconName::Info).xsmall().text_color(theme.warning))
            }
            (true, _) => mark.child(Spinner::new().xsmall().color(theme.info)),
            (false, Some(Unseen::Done)) => {
                mark.child(div().size(px(7.0)).rounded_full().bg(theme.success))
            }
            (false, Some(Unseen::Failed)) => {
                mark.child(Icon::new(IconName::Close).xsmall().text_color(theme.danger))
            }
            (false, None) => mark,
        };
        let close = div()
            .invisible()
            .group_hover("chat-row", |s| s.visible())
            .child(
                Button::new(("close", id))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Close)
                    .tooltip("Close")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.act(Intent::Close(id), window, cx);
                    })),
            );
        let shown = self.shown == Some(id);
        let row = h_flex()
            .id(("chat", id))
            .group("chat-row")
            .gap_1()
            .pl_4()
            .pr_1()
            .h(px(26.0))
            .rounded_md()
            .text_sm()
            .cursor_pointer()
            .hover(|s| s.bg(theme.sidebar_accent))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.show(id, window, cx);
                cx.notify();
            }))
            .child(mark)
            .child(div().flex_1().min_w_0().truncate().child(chat.title()))
            .child(close);
        if shown {
            row.bg(theme.sidebar_accent)
                .text_color(theme.sidebar_accent_foreground)
        } else {
            row
        }
    }
}
