//! The sidebar: every node, live from the core's node poll, with its open
//! chats beneath it. Its edge is dragged to widen it.

use commandant_client_core::chat::Unseen;
use commandant_client_core::{Chat, Intent};
use commandant_proto::NodeInfo;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::DropdownMenu as _;
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Div, ElementId, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Stateful, StatefulInteractiveElement as _, Styled as _, div, px,
};

use super::connected::{Connected, Resized, act_item, act_on, hint};
use super::placement::{SIDEBAR_MAX, SIDEBAR_MIN};

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
    /// Every node and its chats, then the Nodes and Tasks entries, and the
    /// link and logging out.
    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let nodes = self.core.nodes();
        let empty = nodes.is_empty().then(|| {
            hint(
                "No nodes yet. Add a worker with `commandant-server worker <link>`.",
                cx,
            )
        });
        let mut tree = Vec::new();
        for (at, node) in nodes.iter().enumerate() {
            tree.push(self.node_row(at, node, cx));
            for chat in self.core.chats_on(&node.id) {
                tree.push(self.chat_row(chat, cx));
            }
        }
        v_flex()
            .size_full()
            .p_2()
            .gap_1()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .child(
                v_flex()
                    .id("nodes")
                    .flex_1()
                    .gap_1()
                    .overflow_y_scroll()
                    .children(tree)
                    .children(empty),
            )
            .child(self.nodes_row(cx))
            .child(self.tasks_row(cx))
            .child(self.footer(cx))
    }

    /// `main`, beside the sidebar unless it is folded away; the edge
    /// between them is dragged to resize the sidebar, which keeps its width
    /// as the window resizes.
    pub(super) fn beside_sidebar(&self, main: Div, cx: &mut Context<Self>) -> AnyElement {
        if !self.sidebar_open {
            return main.into_any_element();
        }
        h_resizable("sidebar")
            .on_resize(cx.listener(|this, state: &Entity<ResizableState>, _, cx| {
                let Some(&width) = state.read(cx).sizes().first() else {
                    return;
                };
                // A click on the edge ends a drag too, moving nothing.
                let width: f32 = width.into();
                if width != this.sidebar {
                    this.sidebar = width;
                    cx.emit(Resized(width));
                }
            }))
            .child(
                resizable_panel()
                    .size(px(self.sidebar))
                    .size_range(px(SIDEBAR_MIN)..px(SIDEBAR_MAX))
                    .fixed()
                    .child(self.render_sidebar(cx)),
            )
            .child(resizable_panel().child(main))
            .into_any_element()
    }

    /// Opens the nodes; marked while they, or an output run from them, are
    /// shown.
    fn nodes_row(&self, cx: &Context<Self>) -> Stateful<Div> {
        let shown = self.nodes_shown && self.core.tasks().is_none();
        row("nodes-entry", shown, cx)
            .child(Icon::new(IconName::Network).small())
            .child("Nodes")
            .on_click(cx.listener(|this, _, window, cx| this.show_nodes(None, window, cx)))
    }

    /// Opens the tasks; marked while they, or an output, are shown.
    fn tasks_row(&self, cx: &Context<Self>) -> Stateful<Div> {
        let shown = self.shown.is_none()
            && (self.core.tasks().is_some() || (self.output.is_some() && !self.nodes_shown));
        row("tasks-entry", shown, cx)
            .child(Icon::new(IconName::SquareTerminal).small())
            .child("Tasks")
            .on_click(cx.listener(|this, _, window, cx| this.show_tasks(window, cx)))
    }

    /// Shows the link to copy, or logs out once confirmed.
    fn footer(&self, cx: &Context<Self>) -> Div {
        let button = |id: &'static str, label: &'static str, intent: Intent| {
            Button::new(id)
                .ghost()
                .xsmall()
                .label(label)
                .on_click(act_on(intent, cx))
        };
        h_flex()
            .pt_1()
            .border_t_1()
            .border_color(cx.theme().sidebar_border)
            .child(button("link", "Link…", Intent::ShowLink))
            .child(button("log-out", "Log out", Intent::LogOut))
    }

    /// A node's status dot and name, dimmed while it is offline and turning
    /// while it starts its agent; a click opens its last chat, `[+]` a new
    /// one or its sessions.
    fn node_row(&self, at: usize, node: &NodeInfo, cx: &Context<Self>) -> Stateful<Div> {
        let theme = cx.theme();
        let (id, online) = (node.id.clone(), node.online);
        let open = cx.listener(move |this, _, window, cx| {
            // Nothing to open on it: its row in the nodes says why.
            if !online && this.core.chats_on(&id).next().is_none() {
                return this.show_nodes(Some(id.clone()), window, cx);
            }
            let chat = this.last.get(&id).copied();
            let node = id.clone();
            this.act(Intent::Open { node, chat }, window, cx);
        });
        let connected = cx.entity().downgrade();
        let new_chat = Intent::OpenSession {
            node: node.id.clone(),
            session: None,
        };
        let sessions = Intent::NodeSessions {
            node: node.id.clone(),
            chat: self.last.get(&node.id).copied(),
        };
        let new = node.online.then(|| {
            Button::new(("new-chat", at))
                .ghost()
                .xsmall()
                .icon(IconName::Plus)
                .tooltip("New chat or session")
                .dropdown_menu(move |menu, _, _| {
                    menu.item(act_item(&connected, "New chat", new_chat.clone()))
                        .item(act_item(&connected, "Sessions…", sessions.clone()))
                })
        });
        row(("node", at), false, cx)
            .on_click(open)
            .when(!node.online, |row| row.text_color(theme.muted_foreground))
            .child(status_dot(node.online, cx))
            .child(div().flex_1().truncate().child(node.name.clone()))
            .children(
                self.core
                    .starting(&node.id)
                    .then(|| Spinner::new().xsmall().color(theme.info)),
            )
            .children(new)
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
            .group_hover("chat-row", gpui_kit::Styled::visible)
            .child(
                Button::new(("close", u64::from(id)))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Close)
                    .tooltip("Close")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.act(Intent::Close(id), window, cx);
                    })),
            );
        row(("chat", u64::from(id)), self.shown == Some(id), cx)
            .group("chat-row")
            .gap_1()
            .pl_4()
            .pr_1()
            .h(px(26.0))
            .text_sm()
            .on_click(cx.listener(move |this, _, window, cx| {
                this.show(id, window, cx);
                cx.notify();
            }))
            .child(mark)
            .child(div().flex_1().min_w_0().truncate().child(chat.title()))
            .child(close)
    }
}

/// A row of the sidebar, marked while what it opens is `shown`.
fn row(id: impl Into<ElementId>, shown: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    h_flex()
        .id(id)
        .gap_2()
        .px_2()
        .h(px(28.0))
        .rounded_md()
        .cursor_pointer()
        .hover(|s| s.bg(theme.sidebar_accent))
        .when(shown, |row| {
            row.bg(theme.sidebar_accent)
                .text_color(theme.sidebar_accent_foreground)
        })
}
