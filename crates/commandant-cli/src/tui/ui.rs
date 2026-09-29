//! Drawing, without boxes: a status line on top, the thread, the prompt on a
//! solid slab, the settings underneath, and the picker floating over it all.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Padding, Paragraph};

use super::app::{self, Activity, App, Role};
use super::picker::Picker;
use super::text::{self, MUTED};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// The prompt's slab.
const SURFACE: Color = Color::Indexed(236);
/// The picker's panel, and its selected row.
const PANEL: Color = Color::Indexed(235);
const SELECTED: Color = Color::Indexed(238);
const USER: Color = Color::Indexed(252);
/// Fixed greys, a notch brighter than `MUTED`: the model's thinking, and the
/// empty prompt's hint on its slab.
const THOUGHT: Color = Color::Indexed(247);
const HINT: Color = Color::Indexed(246);
/// Agents take these colors in the order the node lists them.
const AGENT_COLORS: [Color; 6] = [
    Color::Cyan,
    Color::Yellow,
    Color::Magenta,
    Color::Green,
    Color::Blue,
    Color::LightRed,
];
const BAR: &str = "▌ ";
const INDENT: &str = "  ";

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area().inner(ratatui::layout::Margin::new(1, 0));
    let [header, _, thread, status, input, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .areas(area);
    draw_header(frame, app, header);
    draw_thread(frame, app, thread);
    draw_status(frame, app, status);
    draw_input(frame, app, input);
    draw_footer(frame, app, footer);
    if let Some(picker) = &app.picker {
        draw_picker(frame, app, picker);
    }
}

/// `● my-box  opencode · linux/x86_64 · 0.1.0          ses_1f3a… · ~/src/app`
fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let node = &app.node;
    let dot = match node.online {
        true => "● ".green(),
        false => "● ".red(),
    };
    let mut left = vec![dot, Span::raw(node.name.clone()).bold(), Span::raw("  ")];
    let facts = [
        app.harness().unwrap_or("no agent harness").to_string(),
        format!("{}/{}", node.os, node.arch),
        format!("v{}", node.version),
    ];
    left.extend(dotted(facts.into_iter().map(|f| f.fg(MUTED))));

    let settings = &app.settings;
    let session = match settings.session_id.as_str() {
        "" => "new session".to_string(),
        id => shorten(id, 16),
    };
    let left = Line::from(left);
    // What's left once the session is shown, less a gap and a dot.
    let room = (area.width as usize).saturating_sub(left.width() + session.chars().count() + 5);
    let cwd = match settings.cwd.as_str() {
        "" => "worker's directory".to_string(),
        cwd => shorten_left(cwd, room),
    };
    // Give up the directory, then the session, rather than overlap.
    let right = if cwd.chars().count() <= room && room >= 12 {
        Line::from(dotted([session.fg(MUTED), cwd.fg(MUTED)]))
    } else if session.chars().count() + 2 + left.width() <= area.width as usize {
        Line::from(session.fg(MUTED))
    } else {
        Line::default()
    };
    frame.render_widget(left, area);
    frame.render_widget(right.right_aligned(), area);
}

fn draw_thread(frame: &mut Frame, app: &mut App, area: Rect) {
    let rows = thread_rows(app, area.width as usize);
    // Follow the bottom unless the user has scrolled up.
    let height = area.height as usize;
    let bottom = rows.len().saturating_sub(height);
    app.scroll = app.scroll.min(bottom as u16);
    let top = bottom - app.scroll as usize;
    let visible: Vec<Line> = rows.into_iter().skip(top).take(height).collect();
    frame.render_widget(Paragraph::new(visible), area);
}

/// The thread, wrapped to `width`, each entry behind its gutter.
fn thread_rows(app: &App, width: usize) -> Vec<Line<'static>> {
    if app.thread.is_empty() {
        return welcome(app);
    }
    let mut rows = Vec::new();
    let mut previous = None;
    for entry in &app.thread {
        let role = entry.role;
        // Space out turns, but keep the pieces of a reply together.
        let joined = matches!(
            (previous, role),
            (Some(Role::Tool), Role::Tool) | (Some(_), Role::Summary)
        );
        if previous.is_some() && !joined {
            rows.push(Line::default());
        }
        previous = Some(role);

        let text = entry.text.trim_matches('\n');
        let (lines, first, rest): (Vec<Line<'static>>, Span<'static>, Span<'static>) = match role {
            Role::User => (
                plain_lines(text, Style::new().fg(USER)),
                Span::styled(BAR, Style::new().fg(agent_color(app, &entry.agent))),
                Span::styled(BAR, Style::new().fg(agent_color(app, &entry.agent))),
            ),
            Role::Thinking => (
                plain_lines(text, Style::new().fg(THOUGHT).italic()),
                "┊ ".fg(MUTED),
                "┊ ".fg(MUTED),
            ),
            Role::Agent => (text::markdown(text), INDENT.into(), INDENT.into()),
            Role::Tool => (
                plain_lines(text, Style::new().fg(MUTED)),
                "⚙ ".fg(MUTED),
                INDENT.into(),
            ),
            Role::Error => (
                plain_lines(text, Style::new().red()),
                "✗ ".red(),
                INDENT.into(),
            ),
            Role::Info => (
                plain_lines(text, Style::new().fg(MUTED).italic()),
                "· ".fg(MUTED),
                INDENT.into(),
            ),
            Role::Summary => (
                plain_lines(text, Style::new().fg(MUTED)),
                "◆ ".fg(MUTED),
                INDENT.into(),
            ),
        };
        for (i, line) in lines.into_iter().enumerate() {
            let lead = if i == 0 { &first } else { &rest };
            rows.extend(text::wrap(line, width, lead, &rest));
        }
    }
    rows
}

fn plain_lines(text: &str, style: Style) -> Vec<Line<'static>> {
    text.lines()
        .map(|l| Line::from(Span::styled(l.to_string(), style)))
        .collect()
}

/// What an empty thread shows: where prompts go, and the keys.
fn welcome(app: &App) -> Vec<Line<'static>> {
    let agent = app.agent();
    let mut lines = vec![
        Line::from(vec![
            Span::raw(INDENT),
            "Ask ".fg(MUTED),
            Span::raw(or(agent, "the agent").to_string()).fg(agent_color(app, agent)),
            format!(" on {} anything.", app.node.name).fg(MUTED),
        ]),
        Line::default(),
    ];
    let keys = [
        ("Tab", "switch agent"),
        ("/model", "choose a model"),
        ("/effort  Ctrl-T", "thinking effort"),
        ("/skills", "the agent's commands and skills"),
        ("/mcp", "connect MCP servers"),
        ("/new", "new session"),
        ("Esc", "cancel a turn"),
        ("PgUp PgDn", "scroll"),
    ];
    for (key, does) in keys {
        lines.push(Line::from(vec![
            Span::raw(format!("{INDENT}{key:<17}")).bold(),
            does.fg(MUTED),
        ]));
    }
    lines
}

/// What the agent is doing, and where the thread is scrolled.
fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let left = match &app.activity {
        Activity::Idle => Line::default(),
        Activity::Working {
            cancelling: true, ..
        } => Line::from("  cancelling…").yellow(),
        Activity::Working { since, .. } => {
            let elapsed = since.elapsed();
            let frame = (elapsed.as_millis() / 100) as usize % SPINNER.len();
            Line::from(vec![
                Span::raw(format!("{} ", SPINNER[frame])).cyan(),
                Span::raw(app.doing()).cyan(),
                format!(" {}s", elapsed.as_secs()).fg(MUTED),
                "  esc to cancel".fg(MUTED),
            ])
        }
    };
    frame.render_widget(left, area);
    if app.scroll > 0 {
        let more = Line::from(format!("↓ {} more lines  ", app.scroll)).fg(MUTED);
        frame.render_widget(more.right_aligned(), area);
    }
}

/// The prompt, on a solid slab with the agent's color down its edge.
fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    let color = agent_color(app, app.agent());
    frame.render_widget(Block::new().bg(SURFACE), area);
    for y in area.top()..area.bottom() {
        let edge = Rect::new(area.x, y, 1, 1);
        frame.render_widget(Span::raw("▌").fg(color), edge);
    }
    let line = Rect::new(area.x + 2, area.y + 1, area.width.saturating_sub(3), 1);

    let input = &app.input;
    if input.text.is_empty() {
        let hint = match app.activity {
            Activity::Idle => format!("Message {}…", or(app.agent(), "the agent")),
            Activity::Working { .. } => "Type the next prompt…".into(),
        };
        frame.render_widget(Span::raw(hint).fg(HINT), line);
        frame.set_cursor_position((line.x, line.y));
        return;
    }
    // Scroll sideways so the cursor stays visible.
    let width = (line.width as usize).saturating_sub(1);
    let offset = input.cursor.saturating_sub(width);
    let visible: String = input.text.chars().skip(offset).take(width).collect();
    let style = match input.text.starts_with('/') {
        true => Style::new().fg(color),
        false => Style::new(),
    };
    frame.render_widget(Span::styled(visible, style), line);
    let x = line.x + (input.cursor - offset) as u16;
    frame.set_cursor_position((x, line.y));
}

/// The agent, model and effort the next prompt goes to; what the session
/// has used; and the keys to change them.
fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let agent = app.agent();
    let model = match (app.settings.model.as_str(), app.model()) {
        ("", "") => "default model".to_string(),
        (_, id) => app.model_name(id).to_string(),
    };
    let effort = match app.settings.effort.as_str() {
        "" => "default effort".to_string(),
        effort => format!("{effort} effort"),
    };
    let left = Line::from(dotted([
        Span::raw(or(agent, "default agent").to_string())
            .fg(agent_color(app, agent))
            .bold(),
        Span::raw(model),
        Span::raw(effort).magenta(),
    ]));

    let mut usage = Vec::new();
    if app.context > 0 {
        let window = app.model_choice(app.model()).map_or(0, |m| m.context);
        usage.push(match window {
            0 => format!("{} tokens", app::count(app.context)),
            window => format!(
                "{}/{} {}%",
                app::count(app.context),
                app::count(window),
                app.context * 100 / window
            ),
        });
    }
    if app.spent > 0.0 {
        usage.push(app::dollars(app.spent));
    }
    let usage = Line::from(dotted(usage.into_iter().map(|u| u.fg(MUTED))));
    let keys = Line::from(vec![
        "tab".bold().fg(MUTED),
        " agent  ".fg(MUTED),
        "/model".bold().fg(MUTED),
        "  ".into(),
        "ctrl-t".bold().fg(MUTED),
        " effort".fg(MUTED),
    ]);

    // Drop the keys, then the usage, when there's no room.
    let room = (area.width as usize).saturating_sub(left.width() + 2);
    let gap = if usage.width() > 0 { 3 } else { 0 };
    let right = if usage.width() + gap + keys.width() <= room {
        let mut spans = usage.spans;
        if gap > 0 {
            spans.push(Span::raw("   "));
        }
        spans.extend(keys.spans);
        Line::from(spans)
    } else if usage.width() <= room {
        usage
    } else {
        Line::default()
    };
    frame.render_widget(left, area);
    frame.render_widget(right.right_aligned(), area);
}

/// The picker: a solid panel centred over everything else.
fn draw_picker(frame: &mut Frame, app: &App, picker: &Picker) {
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

    let accent = agent_color(app, app.agent());
    frame.render_widget(Span::raw(picker.pick.title()).bold(), title);
    frame.render_widget(
        Line::from(format!("{} of {}", picker.shown_len(), picker.total()))
            .fg(MUTED)
            .right_aligned(),
        title,
    );
    let query = match picker.filter.as_str() {
        "" => Line::from(vec!["› ".fg(accent), "type to filter".fg(MUTED)]),
        filter => Line::from(vec!["› ".fg(accent), Span::raw(filter.to_string())]),
    };
    frame.render_widget(query, filter);
    let x = filter.x + 2 + picker.filter.chars().count() as u16;
    frame.set_cursor_position((x.min(filter.right().saturating_sub(1)), filter.y));
    frame.render_widget(
        Line::from(vec![
            "↑↓".bold().fg(MUTED),
            " move  ".fg(MUTED),
            "enter".bold().fg(MUTED),
            " choose  ".fg(MUTED),
            "esc".bold().fg(MUTED),
            " close".fg(MUTED),
        ]),
        hints,
    );

    if picker.shown_len() == 0 {
        frame.render_widget(Line::from("nothing matches").fg(MUTED), list);
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

/// The color of the agent named `name`, from where the node lists it.
fn agent_color(app: &App, name: &str) -> Color {
    let at = app
        .options
        .as_ref()
        .and_then(|o| o.agents.iter().position(|a| a.name == name))
        .unwrap_or(0);
    AGENT_COLORS[at % AGENT_COLORS.len()]
}

/// Spans separated by dim dots.
fn dotted(spans: impl IntoIterator<Item = Span<'static>>) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    for span in spans {
        if !out.is_empty() {
            out.push(" · ".fg(MUTED));
        }
        out.push(span);
    }
    out
}

fn or<'a>(value: &'a str, default: &'a str) -> &'a str {
    if value.is_empty() { default } else { value }
}

/// `ses_f15c0068…`: the start of an id.
fn shorten(id: &str, keep: usize) -> String {
    match id.char_indices().nth(keep) {
        Some((at, _)) => format!("{}…", &id[..at]),
        None => id.to_string(),
    }
}

/// `…/tmp/tui/proj`: the end of a path, to fit `room` columns.
fn shorten_left(path: &str, room: usize) -> String {
    let len = path.chars().count();
    if len <= room || room < 2 {
        return path.to_string();
    }
    let tail: String = path.chars().skip(len - (room - 1)).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    use super::*;
    use crate::tui::app::tests::{app, output};
    use commandant_proto::OutputStream;

    fn render(app: &mut App) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// Where `text` starts on screen.
    fn find(buf: &Buffer, text: &str) -> (u16, u16) {
        let area = buf.area;
        for y in 0..area.height {
            let row: String = (0..area.width).map(|x| buf[(x, y)].symbol()).collect();
            if let Some(at) = row.find(text) {
                let x = row[..at].chars().count() as u16;
                return (x, y);
            }
        }
        panic!("{text:?} isn't on screen");
    }

    #[test]
    fn secondary_text_stays_readable() {
        let mut app = app();
        app.on_message(output(OutputStream::Reasoning, b"pondering"));
        let buf = render(&mut app);

        let hint = &buf[find(&buf, "Message")];
        assert_eq!((hint.fg, hint.bg), (HINT, SURFACE));
        assert_eq!(buf[find(&buf, "pondering")].fg, THOUGHT);
        assert_eq!(buf[find(&buf, "opencode")].fg, MUTED);
        // Some palettes make dark gray unreadable, so nothing uses it.
        assert!(buf.content.iter().all(|cell| cell.fg != Color::DarkGray));
    }
}
