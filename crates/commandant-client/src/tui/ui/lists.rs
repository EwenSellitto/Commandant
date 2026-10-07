//! What floats over the rest: the commands completing what is typed, and
//! the picker.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Padding};

use super::{MUTED, PANEL, SELECTED, hints, spinner};
use crate::tui::picker::Picker;

/// The commands completing what is typed, on a panel at the bottom of
/// `area`, just over the prompt.
pub(super) fn draw_suggestions(
    frame: &mut Frame,
    suggestions: &[(String, String)],
    suggested: usize,
    accent: Color,
    area: Rect,
) {
    const SHOWN: usize = 8;
    if suggestions.is_empty() {
        return;
    }
    let height = suggestions.len().min(SHOWN).min(area.height as usize);
    let area = Rect::new(
        area.x,
        area.bottom() - height as u16,
        area.width,
        height as u16,
    );
    frame.render_widget(Clear, area);
    frame.render_widget(Block::new().bg(PANEL), area);
    let width = suggestions
        .iter()
        .map(|(n, _)| n.chars().count())
        .max()
        .unwrap_or(0)
        + 3;
    let items: Vec<ListItem> = suggestions
        .iter()
        .map(|(name, does)| {
            ListItem::new(Line::from(vec![
                Span::raw(format!(" {:<width$}", format!("/{name}"))).fg(accent),
                Span::raw(does.clone()).fg(MUTED),
            ]))
        })
        .collect();
    let list =
        List::new(items).highlight_style(Style::new().bg(SELECTED).add_modifier(Modifier::BOLD));
    let mut state = ListState::default().with_selected(Some(suggested));
    frame.render_stateful_widget(list, area, &mut state);
}

/// The picker: a solid panel centred over everything else.
pub(super) fn draw_picker(frame: &mut Frame, accent: Color, picker: &Picker) {
    let screen = frame.area();
    let width = screen.width.saturating_sub(4).min(80);
    // Title, filter, gap, the list, gap, hints; plus a padding row each end.
    let rows = picker.shown_len().max(1) as u16;
    let height = (rows + 7).min(screen.height.saturating_sub(2));
    let area = screen.centered(Constraint::Length(width), Constraint::Length(height));
    frame.render_widget(Clear, area);
    let panel = Block::new().bg(PANEL).padding(Padding::new(2, 2, 1, 1));
    let inner = panel.inner(area);
    frame.render_widget(panel, area);
    let [title, filter, _, list, _, hints] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    frame.render_widget(Span::raw(picker.title()).bold(), title);
    let count = format!("{} of {}", picker.shown_len(), picker.total());
    let count = match picker.loading() {
        true => format!("{} loading · {count}", spinner()),
        false => count,
    };
    frame.render_widget(Line::from(count).fg(MUTED).right_aligned(), title);
    let query = match picker.filter.as_str() {
        "" => Line::from(vec!["› ".fg(accent), "type to filter".fg(MUTED)]),
        filter => Line::from(vec!["› ".fg(accent), Span::raw(filter.to_string())]),
    };
    frame.render_widget(query, filter);
    let x = filter.x + 2 + picker.filter.chars().count() as u16;
    frame.set_cursor_position((x.min(filter.right().saturating_sub(1)), filter.y));
    frame.render_widget(
        self::hints(&[("↑↓", "move"), ("enter", "choose"), ("esc", "close")]),
        hints,
    );

    if picker.shown_len() == 0 {
        let empty = if picker.loading() {
            "loading…"
        } else {
            "nothing matches"
        };
        frame.render_widget(Line::from(empty).fg(MUTED), list);
        return;
    }
    let items: Vec<ListItem> = picker
        .shown()
        .map(|choice| {
            ListItem::new(Line::from(vec![
                Span::raw(choice.label.clone()),
                Span::raw(format!("  {}", choice.detail)).fg(MUTED),
            ]))
        })
        .collect();
    let list_widget = List::new(items)
        .highlight_symbol(Line::from("▌ ").fg(accent))
        .highlight_style(Style::new().bg(SELECTED).add_modifier(Modifier::BOLD));
    let mut state = ListState::default().with_selected(Some(picker.selected));
    frame.render_stateful_widget(list_widget, list, &mut state);
}
