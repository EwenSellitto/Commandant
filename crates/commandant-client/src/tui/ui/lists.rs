//! What floats over the rest: the commands completing what is typed, the
//! picker, a question to confirm or read, and the help.

use commandant_client_core::Ask;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Padding, Paragraph};

use super::{MUTED, PANEL, SELECTED, hints, spinner};
use crate::picker::{Filtered, Picker, Row};
use crate::tui::chat::Input;
use crate::tui::help::Help;
use crate::tui::text;

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
    let title = picker.title();
    draw_list(
        frame,
        accent,
        title,
        picker.loading(),
        &picker.list,
        "choose",
    );
}

/// The help, like a picker.
pub(super) fn draw_help(frame: &mut Frame, accent: Color, help: &Help) {
    draw_list(frame, accent, "Help", false, &help.list, "do it");
}

/// Rows narrowed down by typing on a solid panel centred over everything
/// else, Enter doing what `enter` says.
fn draw_list<R: Row>(
    frame: &mut Frame,
    accent: Color,
    title: &str,
    loading: bool,
    filtered: &Filtered<R>,
    enter: &'static str,
) {
    // Title, filter, gap, the list, gap, hints; plus a padding row each end.
    let rows = filtered.shown_len().max(1) as u16;
    let inner = panel(frame, rows + 7);
    let [title_area, filter, _, list, _, hints] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    frame.render_widget(Span::raw(title).bold(), title_area);
    let count = format!("{} of {}", filtered.shown_len(), filtered.total());
    let count = match loading {
        true => format!("{} loading · {count}", spinner()),
        false => count,
    };
    frame.render_widget(Line::from(count).fg(MUTED).right_aligned(), title_area);
    let query = match filtered.filter.as_str() {
        "" => Line::from(vec!["› ".fg(accent), "type to filter".fg(MUTED)]),
        filter => Line::from(vec!["› ".fg(accent), Span::raw(filter.to_string())]),
    };
    frame.render_widget(query, filter);
    let x = filter.x + 2 + filtered.filter.chars().count() as u16;
    frame.set_cursor_position((x.min(filter.right().saturating_sub(1)), filter.y));
    frame.render_widget(
        self::hints(&[("↑↓", "move"), ("enter", enter), ("esc", "close")]),
        hints,
    );

    if filtered.shown_len() == 0 {
        let empty = if loading {
            "loading…"
        } else {
            "nothing matches"
        };
        frame.render_widget(Line::from(empty).fg(MUTED), list);
        return;
    }
    let items: Vec<ListItem> = filtered
        .shown()
        .map(|row| {
            ListItem::new(Line::from(vec![
                Span::raw(row.label().to_string()),
                Span::raw(format!("  {}", row.detail())).fg(MUTED),
            ]))
        })
        .collect();
    let list_widget = List::new(items)
        .highlight_symbol(Line::from("▌ ").fg(accent))
        .highlight_style(Style::new().bg(SELECTED).add_modifier(Modifier::BOLD));
    let mut state = ListState::default().with_selected(Some(filtered.selected));
    frame.render_stateful_widget(list_widget, list, &mut state);
}

/// A question to confirm or read, if that is what `ask` is: a solid panel
/// centred over everything else. What is to read says once it is `copied`.
pub(super) fn draw_dialog(
    frame: &mut Frame,
    accent: Color,
    ask: Option<&Ask>,
    copied: Option<&str>,
) {
    let (title, said, hints): (&str, &str, &[(&str, &str)]) = match ask {
        Some(Ask::Confirm { text, .. }) => ("", text, &[("enter", "yes"), ("esc", "no")]),
        Some(Ask::Show { title, text }) if copied == Some(text) => {
            (title, text, &[("c", "copied ✓"), ("esc", "close")])
        }
        Some(Ask::Show { title, text }) => (title, text, &[("c", "copy"), ("esc", "close")]),
        _ => return,
    };
    let room = panel_width(frame.area()).saturating_sub(4) as usize;
    let gutter = Span::raw("");
    let lines: Vec<Line> = said
        .lines()
        .flat_map(|line| text::wrap(Line::from(line.to_string()), room, &gutter, &gutter))
        .collect();
    let rows = lines.len() as u16;
    // Title and gap, the text, gap, hints; plus a padding row each end.
    let titled = if title.is_empty() { 0 } else { 2 };
    let inner = panel(frame, titled + rows + 4);
    let [heading, _, body, _, keys] = Layout::vertical([
        Constraint::Length(titled.min(1)),
        Constraint::Length(titled.saturating_sub(1)),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    frame.render_widget(Span::raw(title).bold().fg(accent), heading);
    frame.render_widget(Paragraph::new(lines), body);
    frame.render_widget(self::hints(hints), keys);
}

/// The line the app asks for, typed in a box of its own: a solid panel
/// centred over everything else, Enter doing what `enter` says.
pub(super) fn draw_entry(
    frame: &mut Frame,
    accent: Color,
    label: &str,
    enter: &'static str,
    input: &Input,
) {
    // Label and gap, the line, gap, hints; plus a padding row each end.
    let inner = panel(frame, 7);
    let [heading, _, line, _, keys] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    let mut label = label.chars();
    let first = label.next().map(|c| c.to_uppercase().collect::<String>());
    let label = format!("{}{}", first.unwrap_or_default(), label.as_str());
    frame.render_widget(Span::raw(label).bold().fg(accent), heading);
    draw_line(frame, accent, input, line);
    frame.render_widget(self::hints(&[("enter", enter), ("esc", "cancel")]), keys);
}

/// How wide a panel is on `screen`.
fn panel_width(screen: Rect) -> u16 {
    screen.width.saturating_sub(4).min(80)
}

/// A solid panel `height` rows tall, padding included, centred over
/// everything else; where what it holds goes.
pub(super) fn panel(frame: &mut Frame, height: u16) -> Rect {
    let screen = frame.area();
    let height = height.min(screen.height.saturating_sub(2));
    let area = screen.centered(
        Constraint::Length(panel_width(screen)),
        Constraint::Length(height),
    );
    frame.render_widget(Clear, area);
    let panel = Block::new().bg(PANEL).padding(Padding::new(2, 2, 1, 1));
    let inner = panel.inner(area);
    frame.render_widget(panel, area);
    inner
}

/// A line being typed, behind `›`, with the cursor in it.
pub(super) fn draw_line(frame: &mut Frame, accent: Color, input: &Input, area: Rect) {
    // Scroll sideways so the cursor stays visible.
    let room = (area.width as usize).saturating_sub(3);
    let offset = input.cursor.saturating_sub(room);
    let visible: String = input.text.chars().skip(offset).take(room).collect();
    frame.render_widget(Line::from(vec!["› ".fg(accent), Span::raw(visible)]), area);
    let x = area.x + 2 + (input.cursor - offset) as u16;
    frame.set_cursor_position((x, area.y));
}
