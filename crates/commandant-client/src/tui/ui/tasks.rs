//! The tasks: everything the nodes have run, newest first; and one task's
//! output, followed while it runs.

use std::time::Duration;

use commandant_client_core::chat::elapsed;
use commandant_client_core::{Output, what_ran};
use commandant_common::time::ago;
use commandant_proto::TaskInfo;
use ratatui::Frame;
use ratatui::layout::Constraint::{Length, Min};
use ratatui::layout::{Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState};

use super::{INDENT, MUTED, SELECTED, dotted, draw_more, draw_scrolled, hints, run_rows, spinner};
use crate::tui::app::App;
use crate::tui::text;

/// The task list: node, kind, how it ended, when, and what ran.
pub(super) fn draw_tasks(frame: &mut Frame, app: &App, area: Rect) {
    let [header, _, list, notice, footer] =
        Layout::vertical([Length(1), Length(1), Min(1), Length(1), Length(1)]).areas(area);
    let core = &app.core;
    let Some(tasks) = core.tasks() else {
        return;
    };
    let whose = match &tasks.node {
        None => "every node".to_string(),
        Some(id) => core
            .nodes()
            .iter()
            .find(|n| &n.id == id)
            .map_or_else(|| id.clone(), |n| n.name.clone()),
    };
    let mut title = vec!["Tasks".bold(), format!("  {whose}").fg(MUTED)];
    if tasks.loading {
        title.push(format!("  {} listing…", spinner()).fg(MUTED));
    }
    frame.render_widget(Line::from(title), header);
    if tasks.tasks.is_empty() && !tasks.loading {
        let empty = "Nothing has run yet. Run a command with `r` on the nodes, or `!` in a chat.";
        frame.render_widget(Line::from(empty).fg(MUTED), list);
    }
    let width = |of: fn(&TaskInfo) -> usize| tasks.tasks.iter().map(of).max().unwrap_or(0);
    let node_width = width(|t| t.node_name.chars().count());
    let items: Vec<ListItem> = tasks
        .tasks
        .iter()
        .map(|task| {
            let kind = commandant_common::or(&task.kind, "command");
            let ran = what_ran(task).replace(['\r', '\n'], " ");
            let ran = match task.output_pruned {
                true => Span::raw(ran).fg(MUTED),
                false => Span::raw(ran),
            };
            ListItem::new(Line::from(vec![
                Span::raw(format!("{:<node_width$}  ", task.node_name)).bold(),
                format!("{kind:<7}  ").fg(MUTED),
                status(task),
                format!("{:>8}  ", ago(task.created_at)).fg(MUTED),
                ran,
            ]))
        })
        .collect();
    let list_widget = List::new(items)
        .highlight_symbol(Line::from("▌ ").cyan())
        .highlight_style(Style::new().bg(SELECTED));
    let mut list_state = ListState::default().with_selected(Some(app.selected_task));
    frame.render_stateful_widget(list_widget, list, &mut list_state);
    let said = match &tasks.error {
        Some(e) => format!("couldn't list the tasks: {e}"),
        None => core.notice().to_string(),
    };
    frame.render_widget(Line::from(said).yellow(), notice);
    frame.render_widget(
        hints(&[
            ("↑↓", "move"),
            ("enter", "open"),
            ("f", "node"),
            ("x", "cancel"),
            ("esc", "back"),
        ]),
        footer,
    );
}

/// `running`, `exit 0`, `exit 3`, `cancelled`, padded to line up.
fn status(task: &TaskInfo) -> Span<'static> {
    let (text, style) = match (task.status.as_str(), task.exit_code) {
        ("running", _) => (format!("{} running", spinner()), Style::new().cyan()),
        ("succeeded", _) => ("exit 0".to_string(), Style::new().fg(MUTED)),
        ("failed", Some(code)) => (format!("exit {code}"), Style::new().red()),
        ("cancelled", _) => ("cancelled".to_string(), Style::new().yellow()),
        (status, _) => (status.to_string(), Style::new().red()),
    };
    Span::styled(format!("{text:<10}  "), style)
}

/// A task's output filling the screen: what ran and where, then what it
/// printed, following the end unless scrolled up.
pub(super) fn draw_output(frame: &mut Frame, output: &Output, scroll: &mut u16, area: Rect) {
    let [command, facts, _, body, footer] =
        Layout::vertical([Length(1), Length(1), Length(1), Min(1), Length(1)]).areas(area);
    let width = area.width as usize;
    let line = Line::from(Span::raw(output.line.replace(['\r', '\n'], " ")).bold());
    let line = text::wrap(line, width, &"$ ".fg(MUTED), &INDENT.into());
    frame.render_widget(line.into_iter().next().unwrap_or_default(), command);
    let cwd = commandant_common::or(&output.cwd, "worker's directory").to_string();
    let took = output.finished.unwrap_or_else(commandant_common::time::now) - output.started;
    let took = elapsed(Duration::from_secs(took.max(0) as u64));
    let said = [output.node.clone(), cwd, took].map(|s| s.fg(MUTED));
    frame.render_widget(Line::from(dotted(said)), facts);

    let pruned = output.pruned.then(|| {
        Line::from(
            "output pruned: the server no longer keeps it"
                .fg(MUTED)
                .italic(),
        )
    });
    let rows = pruned
        .into_iter()
        .chain(run_rows(&output.run, width))
        .collect();
    draw_scrolled(frame, rows, scroll, body);

    let mut keys = vec![("↑↓", "scroll"), ("esc", "back")];
    if output.following() {
        keys.insert(1, ("x", "stop"));
    }
    frame.render_widget(hints(&keys), footer);
    draw_more(frame, *scroll, footer);
}
