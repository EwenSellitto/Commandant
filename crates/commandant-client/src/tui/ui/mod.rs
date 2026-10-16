//! Drawing, without boxes. A chat: a status line and the node's chats on top,
//! the thread, the prompt on a solid slab, the settings underneath, and the
//! question asked floating over it all. Or the list of nodes, the list of
//! tasks, or one task's output.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use commandant_client_core::chat::{self, Activity, Chat, Ended, Entry, Role, Run, Unseen};
use commandant_client_core::{Ask, ChatId, Scope, Wanted};
use commandant_common::or;

use super::app::{App, Screen};
use super::chat::ChatView;
use super::help::{BANG, KEYS};
use super::text::{self, MUTED};

mod lists;
mod nodes;
mod tasks;
#[cfg(test)]
mod tests;

use self::lists::{draw_dialog, draw_entry, draw_help, draw_picker, draw_suggestions};
use self::nodes::draw_nodes;
use self::tasks::{draw_output, draw_tasks};

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
    app.sync();
    let area = frame.area().inner(ratatui::layout::Margin::new(1, 0));
    let id = match app.screen {
        Screen::Chat(id) => id,
        Screen::Nodes => return draw_alone(frame, app, |f, app| draw_nodes(f, app, area)),
        Screen::Tasks => return draw_alone(frame, app, |f, app| draw_tasks(f, app, area)),
        Screen::Output(id) => {
            return draw_alone(frame, app, |f, app| {
                if let Some(output) = app.core.output(id) {
                    draw_output(f, output, &mut app.output_scroll, area);
                }
            });
        }
    };
    let [header, tabs, thread, status, input, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .areas(area);
    draw_tabs(frame, app, id, tabs);
    let (Some(chat), Some(view)) = (app.core.chat(id), app.views.get_mut(&id)) else {
        return;
    };
    draw_header(frame, chat, header);
    draw_thread(frame, chat, view, thread);
    draw_status(frame, chat, view.scroll, status);
    draw_input(frame, chat, view, input);
    draw_footer(frame, chat, footer);
    let accent = agent_color(chat, chat.agent());
    draw_suggestions(
        frame,
        &view.suggestions(chat),
        view.suggested,
        accent,
        thread,
    );
    draw_asks(frame, app, accent);
}

/// A screen with no chat on it, then what is asked over it.
fn draw_alone(frame: &mut Frame, app: &mut App, draw: impl FnOnce(&mut Frame, &mut App)) {
    draw(frame, app);
    draw_asks(frame, app, Color::Cyan);
}

/// What is asked on screen, the one on top drawn last, then the help over
/// it all.
fn draw_asks(frame: &mut Frame, app: &App, accent: Color) {
    for scope in app.scopes().into_iter().rev() {
        match (app.pickers.get(&scope), app.core.ask(scope)) {
            (Some(picker), _) => draw_picker(frame, accent, picker),
            // A chat's line is typed in its prompt.
            (None, Some(Ask::Enter { label, wanted, .. })) if scope == Scope::App => {
                let enter = match wanted {
                    Wanted::Lifetime => "make the token",
                    _ => "run",
                };
                draw_entry(frame, accent, label, enter, &app.entry)
            }
            (None, ask) => draw_dialog(frame, accent, ask, app.copied.as_deref()),
        }
    }
    if let Some(help) = &app.help {
        draw_help(frame, accent, help);
    }
}

/// The node's chats, the shown one highlighted; the others say whether they
/// are working, or finished while out of sight.
fn draw_tabs(frame: &mut Frame, app: &App, shown: ChatId, area: Rect) {
    let Some(node) = app.chat().map(|c| c.node.id.clone()) else {
        return;
    };
    let tabs: Vec<Vec<Span<'static>>> = app
        .core
        .chats_on(&node)
        .enumerate()
        .map(|(i, chat)| tab(i + 1, chat, chat.id == shown))
        .collect();
    let at = app
        .core
        .chats_on(&node)
        .position(|c| c.id == shown)
        .unwrap_or(0);
    let hint = hints(&[("ctrl-n", "new"), ("ctrl-o", "sessions")]);
    let width = |spans: &[Span]| spans.iter().map(Span::width).sum::<usize>();
    // Room for the tabs, and the arrows that say some are out of sight.
    let room = (area.width as usize).saturating_sub(4);
    // Widen round the shown tab while the others fit.
    let (mut from, mut to) = (at, at + 1);
    let mut used = width(&tabs[at]);
    loop {
        let mut grew = false;
        if to < tabs.len() && used + width(&tabs[to]) <= room {
            used += width(&tabs[to]);
            to += 1;
            grew = true;
        }
        if from > 0 && used + width(&tabs[from - 1]) <= room {
            from -= 1;
            used += width(&tabs[from]);
            grew = true;
        }
        if !grew {
            break;
        }
    }
    let mut spans = Vec::new();
    if from > 0 {
        spans.push("‹ ".fg(MUTED));
    }
    for tab in &tabs[from..to] {
        spans.extend(tab.iter().cloned());
    }
    if to < tabs.len() {
        spans.push(" ›".fg(MUTED));
    }
    let line = Line::from(spans);
    if line.width() + hint.width() + 2 <= area.width as usize {
        frame.render_widget(hint.right_aligned(), area);
    }
    frame.render_widget(line, area);
}

/// ` 2 fix the parser ⠋ `
fn tab(number: usize, chat: &Chat, shown: bool) -> Vec<Span<'static>> {
    let title: String = shorten(&chat.title(), 22);
    let style = match shown {
        true => Style::new().bg(SELECTED).bold(),
        false => Style::new().fg(MUTED),
    };
    let mut spans = vec![Span::styled(format!(" {number} {title}"), style)];
    let mark = match (chat.at_work(), chat.unseen) {
        (true, _) => Some(Span::raw(format!(" {}", spinner())).cyan()),
        (false, Some(Unseen::Done)) => Some(" ●".green()),
        (false, Some(Unseen::Failed)) => Some(" ✗".red()),
        (false, None) => None,
    };
    spans.extend(mark.map(|m| m.patch_style(Style::new().bg(style.bg.unwrap_or_default()))));
    spans.push(Span::styled(" ", style));
    spans.push(Span::raw(" "));
    spans
}

/// `● my-box  opencode · linux/x86_64 · 0.1.0          ses_1f3a… · ~/src/app`
fn draw_header(frame: &mut Frame, chat: &Chat, area: Rect) {
    let node = &chat.node;
    let mut left = vec![
        status_dot(node.online),
        Span::raw(node.name.clone()).bold(),
        Span::raw("  "),
    ];
    let mut facts = facts(node);
    facts.push(format!("v{}", node.version));
    left.extend(dotted(facts.into_iter().map(|f| f.fg(MUTED))));

    let settings = &chat.settings;
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

fn draw_thread(frame: &mut Frame, chat: &Chat, view: &mut ChatView, area: Rect) {
    let rows = thread_rows(chat, area.width as usize);
    // Follow the bottom unless the user has scrolled up.
    let height = area.height as usize;
    let bottom = rows.len().saturating_sub(height);
    view.scroll = view.scroll.min(bottom as u16);
    let top = bottom - view.scroll as usize;
    let visible: Vec<Line> = rows.into_iter().skip(top).take(height).collect();
    frame.render_widget(Paragraph::new(visible), area);
}

/// The thread, wrapped to `width`, each entry behind its gutter.
fn thread_rows(chat: &Chat, width: usize) -> Vec<Line<'static>> {
    if chat.thread.is_empty() {
        return welcome(chat);
    }
    let mut rows = Vec::new();
    let mut previous = None;
    for entry in &chat.thread {
        let role = entry.role;
        // Space out turns, but keep the pieces of a reply together.
        let joined = match (previous, role) {
            (Some(Role::Tool), Role::Tool) => true,
            (Some(previous), Role::Summary) => previous != Role::Command,
            _ => false,
        };
        if previous.is_some() && !joined {
            rows.push(Line::default());
        }
        previous = Some(role);

        let text = entry.text.trim_matches('\n');
        let (lines, first, rest): (Vec<Line<'static>>, Span<'static>, Span<'static>) = match role {
            Role::User => (
                plain_lines(text, Style::new().fg(USER)),
                Span::styled(BAR, Style::new().fg(agent_color(chat, &entry.agent))),
                Span::styled(BAR, Style::new().fg(agent_color(chat, &entry.agent))),
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
            Role::Command => {
                rows.extend(command_rows(entry, width));
                continue;
            }
        };
        for (i, line) in lines.into_iter().enumerate() {
            let lead = if i == 0 { &first } else { &rest };
            rows.extend(text::wrap(line, width, lead, &rest));
        }
    }
    rows
}

/// A command's block: `$ <command>`, what it printed with its error output
/// tinted, then how it ended.
fn command_rows(entry: &Entry, width: usize) -> Vec<Line<'static>> {
    let command = Line::from(Span::raw(entry.text.clone()).bold());
    let mut rows = text::wrap(command, width, &"$ ".fg(MUTED), &INDENT.into());
    let Some(run) = &entry.run else {
        return rows;
    };
    rows.extend(output_rows(run, width));
    rows.extend(text::wrap(
        end_line(run),
        width,
        &"└ ".fg(MUTED),
        &INDENT.into(),
    ));
    rows
}

/// What a command printed, behind its gutter, wrapped to `width`.
fn output_rows(run: &Run, width: usize) -> Vec<Line<'static>> {
    let gutter = "│ ".fg(MUTED);
    let dropped = run
        .dropped
        .then(|| Line::from("… its earlier output wasn't kept".fg(MUTED).italic()));
    let lines = dropped.into_iter().chain(output_lines(run));
    lines
        .flat_map(|line| text::wrap(line, width, &gutter, &gutter))
        .collect()
}

/// How a command ended, or that it still runs.
fn end_line(run: &Run) -> Line<'static> {
    match &run.ended {
        None if run.stopping() => stopping(),
        None => Line::from(format!("{} running", spinner())).cyan(),
        Some(ended) if ended.succeeded() => Line::from("exit 0").fg(MUTED),
        Some(Ended::Exited(code)) => Line::from(format!("exit {code}")).red(),
        Some(Ended::Cancelled) => Line::from("cancelled").yellow(),
        Some(Ended::Failed(why)) => Line::from(why.clone()).red(),
    }
}

/// What a command printed, line by line; a line can hold pieces of both
/// streams, its error output red.
fn output_lines(run: &Run) -> Vec<Line<'static>> {
    let mut lines = vec![Line::default()];
    for printed in &run.output {
        let style = match printed.stderr {
            true => Style::new().red(),
            false => Style::new(),
        };
        for (i, part) in printed.text.split('\n').enumerate() {
            if i > 0 {
                lines.push(Line::default());
            }
            let part = part.trim_end_matches('\r').replace('\t', "    ");
            if let Some(line) = lines.last_mut().filter(|_| !part.is_empty()) {
                line.push_span(Span::styled(part, style));
            }
        }
    }
    // What ends with a newline has no line after it.
    if lines.last().is_some_and(|l| l.spans.is_empty()) {
        lines.pop();
    }
    lines
}

fn plain_lines(text: &str, style: Style) -> Vec<Line<'static>> {
    text.lines()
        .map(|l| Line::from(Span::styled(l.to_string(), style)))
        .collect()
}

/// What an empty thread shows: where prompts go, and the keys.
fn welcome(chat: &Chat) -> Vec<Line<'static>> {
    let agent = chat.agent();
    let mut lines = vec![
        Line::from(vec![
            Span::raw(INDENT),
            "Ask ".fg(MUTED),
            Span::raw(or(agent, "the agent").to_string()).fg(agent_color(chat, agent)),
            format!(" on {} anything.", chat.node.name).fg(MUTED),
        ]),
        Line::default(),
    ];
    let keys = KEYS.iter().map(|&(k, d, _)| (k.to_string(), d));
    let commands = chat::COMMANDS[1..8]
        .iter()
        .map(|&(n, d)| (format!("/{n}"), d));
    let bang = std::iter::once((BANG.0.to_string(), BANG.1));
    for (key, does) in keys.take(4).chain(commands).chain(bang) {
        lines.push(Line::from(vec![
            Span::raw(format!("{INDENT}{key:<13}")).bold(),
            does.fg(MUTED),
        ]));
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::raw(INDENT),
        "F1".bold(),
        " or ".fg(MUTED),
        "/help".bold(),
        " to find any command or key".fg(MUTED),
    ]));
    lines
}

/// What the agent is doing, or what is still loading, and where the thread
/// is scrolled.
fn draw_status(frame: &mut Frame, chat: &Chat, scroll: u16, area: Rect) {
    let loading = |what: String| {
        Line::from(vec![
            Span::raw(format!("{} ", spinner())).fg(MUTED),
            what.fg(MUTED),
        ])
    };
    let left = match &chat.activity {
        // Esc stops them first.
        _ if let Some(running) = commands_status(chat) => running,
        Activity::Working {
            cancelling: true, ..
        } => Line::from(format!("{} cancelling…", spinner())).yellow(),
        Activity::Working { since, .. } => Line::from(vec![
            Span::raw(format!("{} ", spinner())).cyan(),
            Span::raw(chat.doing()).cyan(),
            format!(" {}", chat::elapsed(since.elapsed())).fg(MUTED),
            "  esc to cancel".fg(MUTED),
        ]),
        Activity::Idle if !chat.node.online => {
            Line::from(format!("● {} is offline", chat.node.name)).red()
        }
        _ if let Some(doing) = chat.signing_in() => Line::from(vec![
            Span::raw(format!("{} ", spinner())).yellow(),
            Span::raw(doing.to_string()).yellow(),
        ]),
        Activity::Idle if let Some(repository) = &chat.preparing => {
            loading(format!("getting {repository} ready on {}…", chat.node.name))
        }
        Activity::Idle if chat.listing_projects => {
            loading(format!("listing {}'s projects…", chat.node.name))
        }
        Activity::Idle if chat.listing_providers.is_some() => {
            loading(format!("listing {}'s model providers…", chat.node.name))
        }
        Activity::Idle if chat.loading_history => {
            loading("loading the session's earlier messages…".into())
        }
        Activity::Idle if chat.options.is_none() && chat.fetching_options => {
            loading(format!("asking {} what its agent offers…", chat.node.name))
        }
        Activity::Idle if chat.loading() => {
            loading("connecting MCP servers; commands follow…".into())
        }
        Activity::Idle => mcp_summary(chat),
    };
    frame.render_widget(left, area);
    if scroll > 0 {
        let more = Line::from(format!("↓ {scroll} more lines  ")).fg(MUTED);
        frame.render_widget(more.right_aligned(), area);
    }
}

/// Which commands run, and how to stop them; `None` when none does.
fn commands_status(chat: &Chat) -> Option<Line<'static>> {
    let running: Vec<(&str, &Run)> = chat.commands().filter(|(_, r)| r.running()).collect();
    let (line, _) = running.last()?;
    if running.iter().all(|(_, r)| r.stopping()) {
        return Some(stopping());
    }
    let what = match running.len() {
        1 => shorten(line, 40),
        n => format!("{n} commands"),
    };
    Some(Line::from(vec![
        Span::raw(format!("{} running ", spinner())).cyan(),
        Span::raw(what).cyan(),
        "  esc to stop".fg(MUTED),
    ]))
}

/// Commands asked to stop, and not stopped yet.
fn stopping() -> Line<'static> {
    Line::from(format!("{} stopping…", spinner())).yellow()
}

/// `mcp 1/2 connected · 1 failed`, when the agent has MCP servers.
fn mcp_summary(chat: &Chat) -> Line<'static> {
    let Some(options) = chat.options.as_ref().filter(|o| !o.mcp_servers.is_empty()) else {
        return Line::default();
    };
    let servers = &options.mcp_servers;
    let connected = servers.iter().filter(|m| m.status == "connected").count();
    let failed = servers.iter().filter(|m| m.status == "failed").count();
    let mut spans = vec![format!("  mcp {connected}/{} connected", servers.len()).fg(MUTED)];
    if failed > 0 {
        spans.push(" · ".fg(MUTED));
        spans.push(format!("{failed} failed").red());
    }
    Line::from(spans)
}

/// The prompt, on a solid slab with the agent's color down its edge.
fn draw_input(frame: &mut Frame, chat: &Chat, view: &ChatView, area: Rect) {
    let color = agent_color(chat, chat.agent());
    frame.render_widget(Block::new().bg(SURFACE), area);
    for y in area.top()..area.bottom() {
        let edge = Rect::new(area.x, y, 1, 1);
        frame.render_widget(Span::raw("▌").fg(color), edge);
    }
    let line = Rect::new(area.x + 2, area.y + 1, area.width.saturating_sub(3), 1);

    let input = &view.input;
    // A line asked for is typed here.
    let secret = chat.ask().is_some_and(Ask::secret);
    if input.text.is_empty() {
        let hint = match (chat.ask(), &chat.activity) {
            (Some(Ask::Enter { label, secret, .. }), _) => match secret {
                true => format!("Paste {label} (hidden), Enter to save, Esc to cancel"),
                false => format!("Paste {label}, Esc to cancel"),
            },
            (_, Activity::Idle) => format!("Message {}…", or(chat.agent(), "the agent")),
            (_, Activity::Working { .. }) => "Type the next prompt…".into(),
        };
        frame.render_widget(Span::raw(hint).fg(HINT), line);
        frame.set_cursor_position((line.x, line.y));
        return;
    }
    // Scroll sideways so the cursor stays visible.
    let width = (line.width as usize).saturating_sub(1);
    let offset = input.cursor.saturating_sub(width);
    let visible: String = match secret {
        true => "•".repeat(input.text.chars().count().saturating_sub(offset).min(width)),
        false => input.text.chars().skip(offset).take(width).collect(),
    };
    // Only a known command's name is colored, not what follows it.
    let colored = match secret {
        true => 0,
        false => chat.command_len(&input.text).saturating_sub(offset),
    };
    let at = visible
        .char_indices()
        .nth(colored)
        .map_or(visible.len(), |(i, _)| i);
    let (command, rest) = visible.split_at(at);
    let spans = vec![
        Span::raw(command.to_string()).fg(color),
        Span::raw(rest.to_string()),
    ];
    frame.render_widget(Line::from(spans), line);
    let x = line.x + (input.cursor - offset) as u16;
    frame.set_cursor_position((x, line.y));
}

/// The agent, model and effort the next prompt goes to; what the session
/// has used; and the keys to change them.
fn draw_footer(frame: &mut Frame, chat: &Chat, area: Rect) {
    let agent = chat.agent();
    let model = match (chat.settings.model.as_str(), chat.model()) {
        ("", "") => "default model".to_string(),
        (_, id) => chat.model_name(id).to_string(),
    };
    let effort = match chat.settings.effort.as_str() {
        "" => "default effort".to_string(),
        effort => format!("{effort} effort"),
    };
    let left = Line::from(dotted([
        Span::raw(or(agent, "default agent").to_string())
            .fg(agent_color(chat, agent))
            .bold(),
        Span::raw(model),
        Span::raw(effort).magenta(),
    ]));

    let mut usage = Vec::new();
    if chat.context > 0 {
        let window = chat.model_choice(chat.model()).map_or(0, |m| m.context);
        usage.push(match window {
            0 => format!("{} tokens", chat::count(chat.context)),
            window => format!(
                "{}/{} {}%",
                chat::count(chat.context),
                chat::count(window),
                chat.context * 100 / window
            ),
        });
    }
    if chat.spent > 0.0 {
        usage.push(chat::dollars(chat.spent));
    }
    let usage = Line::from(dotted(usage.into_iter().map(|u| u.fg(MUTED))));
    let keys = hints(&[("tab", "agent"), ("/model", ""), ("ctrl-t", "effort")]);

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

/// The color of the agent named `name`, from where the node lists it.
fn agent_color(chat: &Chat, name: &str) -> Color {
    AGENT_COLORS[chat.agent_index(name) % AGENT_COLORS.len()]
}

/// `● `: green when the node is online, red when not.
fn status_dot(online: bool) -> Span<'static> {
    if online { "● ".green() } else { "● ".red() }
}

/// `opencode`, `linux/x86_64`: what a node runs, and on what.
fn facts(node: &commandant_proto::NodeInfo) -> Vec<String> {
    let agents = match node.harnesses.join(", ") {
        none if none.is_empty() => "no agent".to_string(),
        agents => agents,
    };
    vec![agents, format!("{}/{}", node.os, node.arch)]
}

/// The spinner's frame now; every spinner on screen turns together.
fn spinner() -> &'static str {
    let now = std::time::UNIX_EPOCH.elapsed().unwrap_or_default();
    SPINNER[(now.as_millis() / 100) as usize % SPINNER.len()]
}

/// `key does  key does`: the keys in bold, all of it dimmed.
fn hints(keys: &[(&'static str, &'static str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (key, does) in keys {
        if !spans.is_empty() {
            spans.push("  ".into());
        }
        spans.push(key.bold().fg(MUTED));
        if !does.is_empty() {
            spans.push(format!(" {does}").fg(MUTED));
        }
    }
    Line::from(spans)
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
