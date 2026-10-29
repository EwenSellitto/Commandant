//! The nodes: what each runs, and what can be done with it, from opening a
//! chat to removing it; and adding one.

use commandant_client_core::{Intent, lacks_agent};
use commandant_common::time::ago;
use commandant_proto::NodeInfo;
use gpui::{
    Context, Div, FontWeight, InteractiveElement as _, ParentElement as _, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::spinner::Spinner;
use gpui_component::{ActiveTheme as _, IconName, Sizable as _, h_flex, v_flex};

use super::connected::Connected;
use super::sidebar::status_dot;

/// How wide a node's name is, in logical pixels.
const NAME: f32 = 160.0;

impl Connected {
    /// Every node, the one marked highlighted, under Add node.
    pub(super) fn render_nodes(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let nodes = self.core.nodes();
        let online = nodes.iter().filter(|n| n.online).count();
        let add = Button::new("add-node")
            .small()
            .icon(IconName::Plus)
            .label("Add node")
            .on_click(cx.listener(|this, _, window, cx| this.act(Intent::AddNode, window, cx)));
        let header = h_flex()
            .px_4()
            .py_2()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Nodes"))
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(format!("{} nodes · {online} online", nodes.len())),
            )
            .child(add);
        let empty = nodes.is_empty().then(|| {
            div()
                .px_2()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("No nodes yet. Add node gives the command that enrols one.")
        });
        let rows: Vec<_> = nodes.iter().map(|node| self.node_entry(node, cx)).collect();
        v_flex().size_full().child(header).child(
            v_flex()
                .id("node-list")
                .flex_1()
                .min_h_0()
                .p_2()
                .gap_0p5()
                .overflow_y_scroll()
                .children(rows)
                .children(empty),
        )
    }

    /// A node's dot, name and facts, and what can be done with it now: open
    /// a chat, start an agent, run a command, remove it. A click marks it.
    fn node_entry(&self, node: &NodeInfo, cx: &Context<Self>) -> Stateful<Div> {
        let theme = cx.theme();
        let starting = self.core.starting(&node.id);
        let chats = self.core.chats_on(&node.id).count();
        // A button acting on this node, by its id.
        let action = |name: &str, act: fn(&mut Self, String) -> Intent| {
            let id = node.id.clone();
            Button::new(SharedString::from(format!("{name}-{}", node.id)))
                .ghost()
                .xsmall()
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.marked = Some(id.clone());
                    let intent = act(this, id.clone());
                    this.act(intent, window, cx);
                }))
        };
        let open = (node.online || chats > 0).then(|| {
            action("open", |this, node| {
                let chat = this.last.get(&node).copied();
                Intent::Open { node, chat }
            })
            .label("Open chat")
        });
        let start = (lacks_agent(node) && !node.can_host.is_empty() && !starting).then(|| {
            action("start", |_, node| Intent::StartAgent(node))
                .icon(IconName::Bot)
                .label("Start agent")
        });
        let run = node.online.then(|| {
            action("run", |_, node| Intent::Run {
                node,
                line: String::new(),
            })
            .icon(IconName::SquareTerminal)
            .label("Run command…")
        });
        let remove = action("remove", |_, node| Intent::RemoveNode(node))
            .icon(IconName::Delete)
            .tooltip("Remove");
        let id = node.id.clone();
        let row = h_flex()
            .id(SharedString::from(format!("nodes-{}", node.id)))
            .gap_2()
            .px_2()
            .h(px(32.0))
            .rounded_md()
            .text_sm()
            .hover(|s| s.bg(theme.accent))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.marked = Some(id.clone());
                cx.notify();
            }))
            .child(status_dot(node.online, cx))
            .child(
                div()
                    .w(px(NAME))
                    .flex_none()
                    .truncate()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(node.name.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.muted_foreground)
                    .child(facts(node)),
            )
            .children(starting.then(|| {
                h_flex()
                    .gap_1()
                    .text_color(theme.info)
                    .child(Spinner::new().xsmall().color(theme.info))
                    .child("starting its agent…")
            }))
            .children(open)
            .children(start)
            .children(run)
            .child(remove);
        if self.marked.as_ref() == Some(&node.id) {
            row.bg(theme.accent)
        } else {
            row
        }
    }
}

/// `linux/x86_64 · v0.4.0 · opencode`, and when it was last seen while
/// offline.
fn facts(node: &NodeInfo) -> String {
    let mut facts = vec![format!("{}/{}", node.os, node.arch)];
    if !node.version.is_empty() {
        facts.push(format!("v{}", node.version));
    }
    facts.push(match node.harnesses.join(", ") {
        none if none.is_empty() => "no agent".into(),
        agents => agents,
    });
    if !node.online {
        facts.push(format!("seen {}", ago(node.last_seen)));
    }
    facts.join(" · ")
}

#[cfg(test)]
mod tests {
    use commandant_common::time::now;

    use super::*;

    #[test]
    fn a_node_says_what_it_runs_on_and_hosts() {
        let node = NodeInfo {
            os: "linux".into(),
            arch: "x86_64".into(),
            version: "0.4.0".into(),
            online: true,
            harnesses: vec!["opencode".into(), "claude-code".into()],
            ..Default::default()
        };
        assert_eq!(
            facts(&node),
            "linux/x86_64 · v0.4.0 · opencode, claude-code"
        );
    }

    #[test]
    fn an_offline_node_says_when_it_was_last_seen() {
        let node = NodeInfo {
            os: "linux".into(),
            arch: "aarch64".into(),
            last_seen: now() - 180,
            ..Default::default()
        };
        assert_eq!(facts(&node), "linux/aarch64 · no agent · seen 3m ago");
    }
}
