//! The sidebar: every node, live from the core's node poll, with its open
//! chats beneath it. Its edge is dragged to widen it.

use commandant_client_core::chat::Unseen;
use commandant_client_core::{Chat, Intent};
use commandant_proto::NodeInfo;
use gpui::{
    App, AppContext as _, Context, Div, DragMoveEvent, Empty, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Stateful, StatefulInteractiveElement as _, Styled as _, Window,
    div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};

use super::connected::{Connected, Resized};
use super::placement::sidebar_within;

/// How wide the sidebar's edge is to grab, in logical pixels.
const EDGE: f32 = 4.0;

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
        let mut tree = Vec::new();
        for (at, node) in nodes.iter().enumerate() {
            tree.push(self.node_row(at, node, cx));
            for chat in self.core.chats_on(&node.id) {
                tree.push(self.chat_row(chat, cx));
            }
        }
        let edge = div()
            .id("sidebar-edge")
            .absolute()
            .top_0()
            .right_0()
            .w(px(EDGE))
            .h_full()
            .cursor_col_resize()
            .hover(|s| s.bg(theme.border))
            .on_drag(DraggedEdge, |edge, _, _, cx| cx.new(|_| *edge));
        v_flex()
            .relative()
            .w(px(self.sidebar))
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
            .child(self.nodes_row(cx))
            .child(self.tasks_row(cx))
            .child(self.footer(cx))
            .child(edge)
    }

    /// Widens or narrows the sidebar to where its edge is dragged.
    pub(super) fn on_drag_sidebar(
        &mut self,
        drag: &DragMoveEvent<DraggedEdge>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let width = sidebar_within((drag.event.position.x - drag.bounds.left()).into());
        if width != self.sidebar {
            self.sidebar = width;
            cx.emit(Resized(width));
            cx.notify();
        }
    }

    /// Opens the nodes; marked while they, or an output run from them, are
    /// shown.
    fn nodes_row(&self, cx: &Context<Self>) -> Stateful<Div> {
        let shown = self.nodes_shown && self.core.tasks().is_none();
        entry("nodes-entry", IconName::Network, "Nodes", shown, cx)
            .on_click(cx.listener(|this, _, window, cx| this.show_nodes(None, window, cx)))
    }

    /// Opens the tasks; marked while they, or an output, are shown.
    fn tasks_row(&self, cx: &Context<Self>) -> Stateful<Div> {
        let shown = self.shown.is_none()
            && (self.core.tasks().is_some() || (self.output.is_some() && !self.nodes_shown));
        entry("tasks-entry", IconName::SquareTerminal, "Tasks", shown, cx)
            .on_click(cx.listener(|this, _, window, cx| this.show_tasks(window, cx)))
    }

    /// Shows the link to copy, or logs out once confirmed.
    fn footer(&self, cx: &Context<Self>) -> Div {
        let button = |id: &'static str, label: &'static str, intent: Intent| {
            Button::new(id).ghost().xsmall().label(label).on_click(
                cx.listener(move |this, _, window, cx| this.act(intent.clone(), window, cx)),
            )
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
                    // An item asking the core for `intent`.
                    let item = |label: &'static str, intent: &Intent| {
                        let (connected, intent) = (connected.clone(), intent.clone());
                        PopupMenuItem::new(label).on_click(move |_, window, cx| {
                            let intent = intent.clone();
                            let _ = connected.update(cx, |this, cx| this.act(intent, window, cx));
                        })
                    };
                    menu.item(item("New chat", &new_chat))
                        .item(item("Sessions…", &sessions))
                })
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

/// A view's entry under the nodes, marked while `shown`.
fn entry(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    shown: bool,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    let row = h_flex()
        .id(id)
        .gap_2()
        .px_2()
        .h(px(28.0))
        .rounded_md()
        .cursor_pointer()
        .hover(|s| s.bg(theme.sidebar_accent))
        .child(Icon::new(icon).small())
        .child(label);
    if shown {
        row.bg(theme.sidebar_accent)
            .text_color(theme.sidebar_accent_foreground)
    } else {
        row
    }
}

/// The sidebar's edge, while it is dragged.
#[derive(Clone, Copy)]
pub(super) struct DraggedEdge;

/// Nothing follows the pointer: the sidebar's edge is what moves.
impl Render for DraggedEdge {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}
