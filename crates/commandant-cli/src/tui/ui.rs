//! Drawing: the agent's details on top, the thread in the middle, the prompt
//! at the bottom, and the picker floating over them.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Wrap};

use super::app::{Activity, App, Role};
use super::picker::Picker;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const PROMPT: &str = "› ";

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [header, thread, input] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(3),
        Constraint::Length(3),
    ])
    .areas(frame.area());
    draw_header(frame, app, header);
    draw_thread(frame, app, thread);
    draw_input(frame, app, input);
    if let Some(picker) = &app.picker {
        draw_picker(frame, picker);
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let node = &app.node;
    let title = match app.harness() {
        Some(harness) => format!(" {harness} on {} ", node.name),
        None => format!(" {} (no agent harness) ", node.name),
    };
    let block = Block::bordered().title(title.bold());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let online = match node.online {
        true => Span::styled("● online", Style::new().fg(Color::Green)),
        false => Span::styled("● offline", Style::new().fg(Color::Red)),
    };
    let mut machine = vec![online];
    field(&mut machine, "host", &node.hostname);
    field(&mut machine, "os", &format!("{}/{}", node.os, node.arch));
    field(&mut machine, "worker", &node.version);
    let settings = &app.settings;
    let mut conversation = Vec::new();
    field(
        &mut conversation,
        "session",
        or(&settings.session_id, "new"),
    );
    field(&mut conversation, "cwd", or(&settings.cwd, "worker's"));
    let (machine, conversation) = (Line::from(machine), Line::from(conversation));
    frame.render_widget(Paragraph::new(vec![machine, conversation]), inner);
    frame.render_widget(Paragraph::new(activity(app)).right_aligned(), inner);
}

/// Appends `name value`, with the name dimmed.
fn field(line: &mut Vec<Span<'static>>, name: &str, value: &str) {
    let gap = if line.is_empty() { "" } else { "   " };
    line.push(Span::raw(format!("{gap}{name} ")).dark_gray());
    line.push(Span::raw(value.to_string()));
}

fn or<'a>(value: &'a str, default: &'a str) -> &'a str {
    if value.is_empty() { default } else { value }
}

fn activity(app: &App) -> Line<'static> {
    match &app.activity {
        Activity::Idle => Line::from("idle").dark_gray(),
        Activity::Working {
            cancelling: true, ..
        } => Line::from("cancelling…").yellow(),
        Activity::Working { since, .. } => {
            let elapsed = since.elapsed();
            let frame = (elapsed.as_millis() / 100) as usize % SPINNER.len();
            Line::from(format!("{} working {}s", SPINNER[frame], elapsed.as_secs())).cyan()
        }
    }
}

fn draw_thread(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::bordered().title(" Thread ");
    let inner = block.inner(area);

    let lines = thread_lines(app);
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    // Follow the bottom unless the user has scrolled up.
    let total = paragraph.line_count(inner.width) as u16;
    let bottom = total.saturating_sub(inner.height);
    app.scroll = app.scroll.min(bottom);
    let block = match app.scroll {
        0 => block,
        up => block.title_bottom(Line::from(format!(" ↓ {up} more lines ")).right_aligned()),
    };
    frame.render_widget(
        paragraph.block(block).scroll((bottom - app.scroll, 0)),
        area,
    );
}

fn thread_lines(app: &App) -> Vec<Line<'static>> {
    if app.thread.is_empty() {
        return vec![Line::from("Ask the agent something below.").dark_gray()];
    }
    let mut lines = Vec::new();
    let mut previous = None;
    for entry in &app.thread {
        // Separate turns, but keep a run of tool calls together.
        if previous.is_some() && !(previous == Some(Role::Tool) && entry.role == Role::Tool) {
            lines.push(Line::default());
        }
        previous = Some(entry.role);
        let text = entry.text.trim_matches('\n');
        match entry.role {
            Role::User => {
                for (i, line) in text.lines().enumerate() {
                    let lead = if i == 0 { PROMPT } else { "  " };
                    lines.push(Line::from(vec![
                        Span::raw(lead).cyan().bold(),
                        Span::raw(line.to_string()).bold(),
                    ]));
                }
            }
            Role::Agent => lines.extend(text.lines().map(|l| Line::from(l.to_string()))),
            Role::Tool => lines.push(Line::from(format!("  ⚙ {text}")).dark_gray()),
            Role::Error => lines.push(Line::from(format!("✗ {text}")).red()),
            Role::Info => lines.push(Line::from(format!("· {text}")).dark_gray().italic()),
        }
    }
    lines
}

fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    let hints = match app.activity {
        Activity::Idle => {
            " Enter send · Tab agent · /model · /effort · /new · PgUp/PgDn scroll · Ctrl-C quit "
        }
        Activity::Working { .. } => " Esc cancel · PgUp/PgDn scroll · Ctrl-C quit ",
    };
    let block = Block::bordered()
        .title(choices(app))
        .title_bottom(Line::from(hints).dark_gray().right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let input = &app.input;
    // Scroll sideways so the cursor stays visible.
    let width = (inner.width as usize).saturating_sub(PROMPT.chars().count() + 1);
    let offset = input.cursor.saturating_sub(width);
    let visible: String = input.text.chars().skip(offset).take(width).collect();
    let line = Line::from(vec![Span::raw(PROMPT).cyan().bold(), Span::raw(visible)]);
    frame.render_widget(Paragraph::new(line), inner);
    let x = inner.x + (PROMPT.chars().count() + input.cursor - offset) as u16;
    frame.set_cursor_position((x, inner.y));
}

/// The agent, model and effort the next prompt goes to, as the prompt's title.
fn choices(app: &App) -> Line<'static> {
    let settings = &app.settings;
    let model = match (settings.model.as_str(), app.model()) {
        ("", "") => "default model".to_string(),
        ("", known) => format!("{known} (default)"),
        (chosen, _) => chosen.to_string(),
    };
    let effort = match settings.effort.as_str() {
        "" => "default effort".to_string(),
        effort => format!("{effort} effort"),
    };
    Line::from(vec![
        Span::raw(" "),
        Span::raw(or(app.agent(), "default agent").to_string())
            .cyan()
            .bold(),
        Span::raw(" · ").dark_gray(),
        Span::raw(model),
        Span::raw(" · ").dark_gray(),
        Span::raw(effort).magenta(),
        Span::raw(" "),
    ])
}

/// The picker, centred over everything else.
fn draw_picker(frame: &mut Frame, picker: &Picker) {
    let screen = frame.area();
    let width = screen.width.saturating_sub(4).min(90);
    // Border, filter line, and the list.
    let height = (picker.shown_len() as u16 + 3).clamp(6, screen.height.saturating_sub(2));
    let area = screen.centered(Constraint::Length(width), Constraint::Length(height));
    frame.render_widget(Clear, area);

    let block = Block::bordered()
        .title(picker.pick.title().bold())
        .title_bottom(
            Line::from(" ↑↓ move · type to filter · Enter choose · Esc close ")
                .dark_gray()
                .right_aligned(),
        )
        .border_style(Style::new().cyan());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [filter, list] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);

    let prompt = Line::from(vec![
        Span::raw(PROMPT).cyan().bold(),
        Span::raw(picker.filter.clone()),
    ]);
    frame.render_widget(Paragraph::new(prompt), filter);
    let x = filter.x + (PROMPT.chars().count() + picker.filter.chars().count()) as u16;
    frame.set_cursor_position((x.min(filter.right().saturating_sub(1)), filter.y));

    if picker.shown_len() == 0 {
        frame.render_widget(
            Paragraph::new(Line::from("nothing matches").dark_gray()),
            list,
        );
        return;
    }
    let items: Vec<ListItem> = picker
        .shown()
        .map(|choice| {
            ListItem::new(Line::from(vec![
                Span::raw(choice.label.clone()),
                Span::raw(format!("  {}", choice.detail)).dark_gray(),
            ]))
        })
        .collect();
    let list_widget = List::new(items)
        .highlight_symbol("› ")
        .highlight_style(Style::new().bg(Color::DarkGray).bold());
    let mut state = ListState::default().with_selected(Some(picker.selected));
    frame.render_stateful_widget(list_widget, list, &mut state);
}
