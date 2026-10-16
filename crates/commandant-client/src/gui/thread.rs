//! A chat's thread, row by row: what the core keeps as raw text and a role,
//! drawn as each role is.

use std::ops::Range;

use commandant_client_core::chat::{Ended, Entry, Role, Run};
use commandant_client_core::{Chat, ChatId};
use gpui::{
    AnyElement, App, Context, Div, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_component::clipboard::Clipboard;
use gpui_component::text::TextView;
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};

use super::connected::Connected;
use super::rows::{Row, running};

impl Connected {
    pub(super) fn render_row(
        &mut self,
        id: ChatId,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (Some(chat), Some(pane)) = (self.core.chat(id), self.panes.get(&id)) else {
            return div().into_any_element();
        };
        let Some(row) = pane.rows.get(index).cloned() else {
            return div().into_any_element();
        };
        let open = pane.open.contains(&row.first());
        match row {
            Row::Tools(range) => self.render_tools(chat, range, open, cx),
            Row::Entry(at) => match chat.thread.get(at) {
                Some(entry) => self.render_entry(chat, at, entry, open, cx),
                None => div().into_any_element(),
            },
        }
    }

    /// Opens or folds the row starting at the entry `first`.
    fn toggle(&mut self, id: ChatId, first: usize, cx: &mut Context<Self>) {
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        if !pane.open.remove(&first) {
            pane.open.insert(first);
        }
        if let Some(index) = pane.rows.iter().position(|r| r.first() == first) {
            pane.scroller.update(cx, |scroller, cx| {
                scroller.remeasure_items(index..index + 1, cx)
            });
        }
        cx.notify();
    }

    fn render_entry(
        &self,
        chat: &Chat,
        at: usize,
        entry: &Entry,
        open: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let text = entry.text.trim_matches('\n').to_string();
        match entry.role {
            Role::User => h_flex()
                .items_stretch()
                .gap_3()
                .child(div().w(px(3.0)).flex_none().rounded_sm().bg(agent_color(
                    chat,
                    &entry.agent,
                    cx,
                )))
                .child(div().flex_1().min_w_0().py_1().child(text))
                .into_any_element(),
            Role::Agent => TextView::markdown(("reply", at), text)
                .selectable(true)
                .code_block_actions(|block, _, _| Clipboard::new("copy").value(block.code()))
                .into_any_element(),
            Role::Thinking => {
                let thinking = div().italic().text_color(theme.muted_foreground);
                if running(chat, at) {
                    return thinking.child(text).into_any_element();
                }
                // Folded to its first line once the turn has ended.
                let id = chat.id;
                let (icon, body) = match open {
                    true => (IconName::ChevronDown, thinking.child(text)),
                    false => (
                        IconName::ChevronRight,
                        thinking.truncate().child(first_line(&text).to_string()),
                    ),
                };
                h_flex()
                    .id(("thinking", at))
                    .items_start()
                    .gap_1()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(id, at, cx)))
                    .child(muted_icon(icon, theme.muted_foreground))
                    .child(body.flex_1().min_w_0())
                    .into_any_element()
            }
            Role::Error => div()
                .text_color(theme.danger)
                .child(text)
                .into_any_element(),
            Role::Info => div()
                .italic()
                .text_color(theme.muted_foreground)
                .child(text)
                .into_any_element(),
            Role::Summary => div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(text)
                .into_any_element(),
            Role::Command => render_command(entry, cx).into_any_element(),
            Role::Tool => self.render_tools(chat, at..at + 1, open, cx),
        }
    }

    /// Tool calls in a row: the last one on a line, or each once opened.
    fn render_tools(
        &self,
        chat: &Chat,
        range: Range<usize>,
        open: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let tools = &chat.thread[range.clone()];
        let (id, first) = (chat.id, range.start);
        let head = h_flex()
            .id(("tools", first))
            .gap_1()
            .cursor_pointer()
            .text_sm()
            .text_color(muted)
            .on_click(cx.listener(move |this, _, _, cx| this.toggle(id, first, cx)));
        if open {
            let count = match tools.len() {
                1 => "1 tool call".to_string(),
                n => format!("{n} tool calls"),
            };
            let lines = tools.iter().map(|tool| {
                div()
                    .pl_5()
                    .text_sm()
                    .text_color(muted)
                    .child(tool.text.trim().to_string())
            });
            return v_flex()
                .gap_0p5()
                .child(
                    head.child(muted_icon(IconName::ChevronDown, muted))
                        .child(count),
                )
                .children(lines)
                .into_any_element();
        }
        let last = tools.last().map_or("", |t| first_line(&t.text));
        let more = (tools.len() > 1).then(|| format!("+{} more", tools.len() - 1));
        head.child(muted_icon(IconName::ChevronRight, muted))
            .child(div().flex_1().min_w_0().truncate().child(last.to_string()))
            .children(more.map(|more| div().flex_none().child(more)))
            .into_any_element()
    }
}

/// A command's block: `$ <command>`, what it printed with its error output
/// tinted, then how it ended.
fn render_command(entry: &Entry, cx: &App) -> Div {
    let theme = cx.theme();
    let block = v_flex()
        .p_2()
        .gap_1()
        .rounded_md()
        .bg(theme.muted)
        .font_family(theme.mono_font_family.clone())
        .text_sm()
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(format!("$ {}", entry.text)),
        );
    let Some(run) = &entry.run else {
        return block;
    };
    let output = run.output.iter().map(|printed| {
        let color = match printed.stderr {
            true => theme.danger,
            false => theme.foreground,
        };
        div()
            .text_color(color)
            .child(printed.text.trim_end_matches('\n').to_string())
    });
    let dropped = run.dropped.then(|| {
        div()
            .italic()
            .text_color(theme.muted_foreground)
            .child("… its earlier output wasn't kept")
    });
    let (end, color) = ended(run, cx);
    block
        .children(dropped)
        .children(output)
        .child(div().text_xs().text_color(color).child(end))
}

/// How a command ended, or that it still runs.
fn ended(run: &Run, cx: &App) -> (String, Hsla) {
    let theme = cx.theme();
    match &run.ended {
        None if run.stopping() => ("stopping…".into(), theme.warning),
        None => ("running…".into(), theme.info),
        Some(ended) if ended.succeeded() => ("exit 0".into(), theme.muted_foreground),
        Some(Ended::Exited(code)) => (format!("exit {code}"), theme.danger),
        Some(Ended::Cancelled) => ("cancelled".into(), theme.warning),
        Some(Ended::Failed(why)) => (why.clone(), theme.danger),
    }
}

fn first_line(text: &str) -> &str {
    text.trim().lines().next().unwrap_or_default()
}

fn muted_icon(icon: IconName, color: Hsla) -> Icon {
    Icon::new(icon).xsmall().text_color(color)
}

/// The color of the agent named `name`, from where the node lists it.
pub(super) fn agent_color(chat: &Chat, name: &str, cx: &App) -> Hsla {
    let theme = cx.theme();
    let colors = [
        theme.cyan,
        theme.yellow,
        theme.magenta,
        theme.green,
        theme.blue,
        theme.red,
    ];
    let at = chat
        .options
        .as_ref()
        .and_then(|o| o.agents.iter().position(|a| a.name == name))
        .unwrap_or(0);
    colors[at % colors.len()]
}
