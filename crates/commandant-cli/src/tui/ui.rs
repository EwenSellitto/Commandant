//! Drawing: the agent's details on top, the thread in the middle, the prompt
//! at the bottom.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use super::app::{Activity, App, Role};

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
    field(&mut conversation, "model", or(&settings.model, "default"));
    field(&mut conversation, "agent", or(&settings.agent, "default"));
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
        Activity::Idle => " Enter send · /new new session · PgUp/PgDn scroll · Ctrl-C quit ",
        Activity::Working { .. } => " Esc cancel · PgUp/PgDn scroll · Ctrl-C quit ",
    };
    let block = Block::bordered()
        .title(" Prompt ")
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
