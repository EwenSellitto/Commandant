//! The sidebar: every node, live from the core's node poll.

use commandant_client_core::Core;
use commandant_proto::NodeInfo;
use gpui::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, StatefulInteractiveElement as _,
    Styled as _, div, px,
};
use gpui_component::{ActiveTheme as _, h_flex, v_flex};

/// The sidebar's width, in logical pixels.
const WIDTH: f32 = 240.0;

pub(super) fn sidebar(core: &Core, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let nodes = core.nodes();
    let empty = nodes.is_empty().then(|| {
        div()
            .px_2()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child("No nodes yet. Add a worker with `commandant-server worker <link>`.")
    });
    let notice = (!core.notice().is_empty()).then(|| {
        div()
            .px_2()
            .text_sm()
            .text_color(theme.warning)
            .child(core.notice().to_string())
    });
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
                .children(nodes.iter().map(|node| node_row(node, cx)))
                .children(empty),
        )
        .children(notice)
}

/// A node's status dot and name, dimmed while it is offline.
fn node_row(node: &NodeInfo, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let dot = div().size(px(8.0)).flex_none().rounded_full();
    let dot = if node.online {
        dot.bg(theme.success)
    } else {
        dot.border_1().border_color(theme.muted_foreground)
    };
    let row = h_flex()
        .gap_2()
        .px_2()
        .py_1()
        .rounded_md()
        .child(dot)
        .child(div().flex_1().truncate().child(node.name.clone()));
    if node.online {
        row
    } else {
        row.text_color(theme.muted_foreground)
    }
}
