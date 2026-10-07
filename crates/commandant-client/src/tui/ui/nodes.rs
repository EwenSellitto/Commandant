//! The node list: what each node runs, and how many of its chats are open.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState};

use super::{MUTED, SELECTED, dotted, facts, hints, spinner, status_dot};
use crate::state::chat::Activity;
use crate::tui::app::App;

/// The node list: what each runs, and how many of its chats are open here.
pub(super) fn draw_nodes(frame: &mut Frame, app: &App, area: Rect) {
    let [header, _, list, notice, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let state = &app.state;
    let online = state.nodes.iter().filter(|n| n.online).count();
    frame.render_widget(
        Line::from(vec![
            "Commandant".bold(),
            format!("  {} nodes · {online} online", state.nodes.len()).fg(MUTED),
        ]),
        header,
    );
    if state.nodes.is_empty() {
        let empty = "No nodes yet. Add a worker with `commandant-server worker <link>`.";
        frame.render_widget(Line::from(empty).fg(MUTED), list);
    }
    let name_width = state
        .nodes
        .iter()
        .map(|n| n.name.chars().count())
        .max()
        .unwrap_or(0);
    let items: Vec<ListItem> = state
        .nodes
        .iter()
        .map(|node| {
            let dot = status_dot(node.online);
            let mut facts = facts(node);
            if !node.online {
                facts.push(format!(
                    "seen {}",
                    commandant_common::time::ago(node.last_seen)
                ));
            }
            let mut spans = vec![
                dot,
                Span::raw(format!("{:<name_width$}  ", node.name)).bold(),
            ];
            spans.extend(dotted(facts.into_iter().map(|f| f.fg(MUTED))));
            if state.starting.contains(&node.id) {
                spans.push(format!("   {} starting its agent…", spinner()).yellow());
            } else if node.online && node.harnesses.is_empty() && !node.can_host.is_empty() {
                spans.push("   enter to start an agent".fg(MUTED));
            }
            let chats = state.chats_on(&node.id).count();
            if chats > 0 {
                let working = state
                    .chats_on(&node.id)
                    .filter(|c| matches!(c.activity, Activity::Working { .. }))
                    .count();
                let mut open = format!("   {chats} open");
                if working > 0 {
                    open.push_str(&format!(" · {working} working"));
                }
                spans.push(open.cyan());
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list_widget = List::new(items)
        .highlight_symbol(Line::from("▌ ").cyan())
        .highlight_style(Style::new().bg(SELECTED));
    let mut list_state = ListState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(list_widget, list, &mut list_state);
    frame.render_widget(Line::from(state.notice.clone()).yellow(), notice);
    frame.render_widget(
        hints(&[("↑↓", "move"), ("enter", "open"), ("q", "quit")]),
        footer,
    );
}
