//! Drawing, without boxes. A chat: a status line and the node's chats on top,
//! the thread, the prompt on a solid slab, the settings underneath, and the
//! picker floating over it all. Or the list of nodes.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Padding, Paragraph};

use super::app::{self, App, Screen};
use super::chat::{self, Activity, Auth, Chat, Role, Unseen, or};
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
    let Screen::Chat(id) = app.screen else {
        draw_nodes(frame, app, area);
        if let Some(picker) = &app.picker {
            draw_picker(frame, Color::Cyan, picker);
        }
        return;
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
    let Some(chat) = app.chats.iter_mut().find(|c| c.id == id) else {
        return;
    };
    draw_header(frame, chat, header);
    draw_thread(frame, chat, thread);
    draw_status(frame, chat, status);
    draw_input(frame, chat, input);
    draw_footer(frame, chat, footer);
    let accent = agent_color(chat, chat.agent());
    draw_suggestions(frame, chat, accent, thread);
    if let Some(picker) = &chat.picker {
        draw_picker(frame, accent, picker);
    }
    if let Some(picker) = &app.picker {
        draw_picker(frame, accent, picker);
    }
}

/// The node list: what each runs, and how many of its chats are open here.
fn draw_nodes(frame: &mut Frame, app: &App, area: Rect) {
    let [header, _, list, notice, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let online = app.nodes.iter().filter(|n| n.online).count();
    frame.render_widget(
        Line::from(vec![
            "Commandant".bold(),
            format!("  {} nodes · {online} online", app.nodes.len()).fg(MUTED),
        ]),
        header,
    );
    if app.nodes.is_empty() {
        let empty = "No nodes yet. Add a worker with `commandant worker <link>`.";
        frame.render_widget(Line::from(empty).fg(MUTED), list);
    }
    let name_width = app
        .nodes
        .iter()
        .map(|n| n.name.chars().count())
        .max()
        .unwrap_or(0);
    let items: Vec<ListItem> = app
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
            if app.starting.contains(&node.id) {
                spans.push(format!("   {} starting its agent…", spinner()).yellow());
            } else if node.online && node.harnesses.is_empty() && !node.can_host.is_empty() {
                spans.push("   enter to start an agent".fg(MUTED));
            }
            let chats = app.chats_on(&node.id).count();
            if chats > 0 {
                let working = app
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
    let mut state = ListState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(list_widget, list, &mut state);
    frame.render_widget(Line::from(app.notice.clone()).yellow(), notice);
    frame.render_widget(
        hints(&[("↑↓", "move"), ("enter", "open"), ("q", "quit")]),
        footer,
    );
}

/// The node's chats, the shown one highlighted; the others say whether they
/// are working, or finished while out of sight.
fn draw_tabs(frame: &mut Frame, app: &App, shown: app::ChatId, area: Rect) {
    let Some(node) = app.chat().map(|c| c.node.id.clone()) else {
        return;
    };
    let tabs: Vec<Vec<Span<'static>>> = app
        .chats_on(&node)
        .enumerate()
        .map(|(i, chat)| tab(i + 1, chat, chat.id == shown))
        .collect();
    let at = app.chats_on(&node).position(|c| c.id == shown).unwrap_or(0);
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
    let mark = match (&chat.activity, chat.unseen) {
        (Activity::Working { .. }, _) => Some(Span::raw(format!(" {}", spinner())).cyan()),
        (Activity::Idle, Some(Unseen::Done)) => Some(" ●".green()),
        (Activity::Idle, Some(Unseen::Failed)) => Some(" ✗".red()),
        (Activity::Idle, None) => None,
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

fn draw_thread(frame: &mut Frame, chat: &mut Chat, area: Rect) {
    let rows = thread_rows(chat, area.width as usize);
    // Follow the bottom unless the user has scrolled up.
    let height = area.height as usize;
    let bottom = rows.len().saturating_sub(height);
    chat.scroll = chat.scroll.min(bottom as u16);
    let top = bottom - chat.scroll as usize;
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
    let keys = chat::KEYS.iter().map(|&(k, d)| (k.to_string(), d));
    let commands = chat::COMMANDS[1..8]
        .iter()
        .map(|&(n, d)| (format!("/{n}"), d));
    for (key, does) in keys.take(4).chain(commands) {
        lines.push(Line::from(vec![
            Span::raw(format!("{INDENT}{key:<13}")).bold(),
            does.fg(MUTED),
        ]));
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::raw(INDENT),
        "/help".bold(),
        " for every command and key".fg(MUTED),
    ]));
    lines
}

/// What the agent is doing, or what is still loading, and where the thread
/// is scrolled.
fn draw_status(frame: &mut Frame, chat: &Chat, area: Rect) {
    let loading = |what: String| {
        Line::from(vec![
            Span::raw(format!("{} ", spinner())).fg(MUTED),
            what.fg(MUTED),
        ])
    };
    let left = match &chat.activity {
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
        _ if let Some(Auth::Waiting { doing, .. }) = &chat.auth => Line::from(vec![
            Span::raw(format!("{} ", spinner())).yellow(),
            Span::raw(doing.clone()).yellow(),
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
    if chat.scroll > 0 {
        let more = Line::from(format!("↓ {} more lines  ", chat.scroll)).fg(MUTED);
        frame.render_widget(more.right_aligned(), area);
    }
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
fn draw_input(frame: &mut Frame, chat: &Chat, area: Rect) {
    let color = agent_color(chat, chat.agent());
    frame.render_widget(Block::new().bg(SURFACE), area);
    for y in area.top()..area.bottom() {
        let edge = Rect::new(area.x, y, 1, 1);
        frame.render_widget(Span::raw("▌").fg(color), edge);
    }
    let line = Rect::new(area.x + 2, area.y + 1, area.width.saturating_sub(3), 1);

    let input = &chat.input;
    let secret = matches!(chat.auth, Some(Auth::Key { .. }));
    if input.text.is_empty() {
        let hint = match (&chat.auth, &chat.activity) {
            (Some(Auth::Key { .. }), _) => {
                "Paste the API key (hidden), Enter to save, Esc to cancel".into()
            }
            (Some(Auth::Code { .. }), _) => "Paste the code the page shows, Esc to cancel".into(),
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
        false => chat.command_len().saturating_sub(offset),
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

/// The commands completing what is typed, on a panel at the bottom of
/// `area`, just over the prompt.
fn draw_suggestions(frame: &mut Frame, chat: &Chat, accent: Color, area: Rect) {
    const SHOWN: usize = 8;
    let suggestions = chat.suggestions();
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
    let mut state = ListState::default().with_selected(Some(chat.suggested));
    frame.render_stateful_widget(list, area, &mut state);
}

/// The picker: a solid panel centred over everything else.
fn draw_picker<T: Clone + PartialEq>(frame: &mut Frame, accent: Color, picker: &Picker<T>) {
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

    frame.render_widget(Span::raw(picker.title).bold(), title);
    let count = format!("{} of {}", picker.shown_len(), picker.total());
    let count = match picker.loading {
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
        let empty = if picker.loading {
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

/// The color of the agent named `name`, from where the node lists it.
fn agent_color(chat: &Chat, name: &str) -> Color {
    let at = chat
        .options
        .as_ref()
        .and_then(|o| o.agents.iter().position(|a| a.name == name))
        .unwrap_or(0);
    AGENT_COLORS[at % AGENT_COLORS.len()]
}

/// `● `: green when the node is online, red when not.
fn status_dot(online: bool) -> Span<'static> {
    if online { "● ".green() } else { "● ".red() }
}

/// `opencode`, `linux/x86_64`: what a node runs, and on what.
fn facts(node: &commandant_proto::NodeInfo) -> Vec<String> {
    let harness = node
        .harnesses
        .first()
        .map_or("no agent harness", String::as_str);
    vec![harness.to_string(), format!("{}/{}", node.os, node.arch)]
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

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    use super::*;
    use crate::tui::app::Update;
    use crate::tui::app::tests::app;
    use crate::tui::chat::tests::output;
    use commandant_proto::OutputStream;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    /// Where `text` isn't.
    fn absent(buf: &Buffer, text: &str) -> bool {
        let area = buf.area;
        (0..area.height).all(|y| {
            let row: String = (0..area.width).map(|x| buf[(x, y)].symbol()).collect();
            !row.contains(text)
        })
    }

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
        app.on_key(KeyEvent::from(KeyCode::Enter));
        let id = app.chats[0].id;
        app.on_update(Update::Chat(
            id,
            output(OutputStream::Reasoning, b"pondering"),
        ));
        let buf = render(&mut app);

        let hint = &buf[find(&buf, "Message")];
        assert_eq!((hint.fg, hint.bg), (HINT, SURFACE));
        assert_eq!(buf[find(&buf, "pondering")].fg, THOUGHT);
        assert_eq!(buf[find(&buf, "opencode")].fg, MUTED);
        // Some palettes make dark gray unreadable, so nothing uses it.
        assert!(buf.content.iter().all(|cell| cell.fg != Color::DarkGray));
    }

    #[test]
    fn nodes_list_their_open_chats() {
        let mut app = app();
        let buf = render(&mut app);
        assert_eq!(buf[find(&buf, "box-n1")].modifier, Modifier::BOLD);
        assert!(
            find(&buf, "seen ").1 > find(&buf, "box-n2").1,
            "offline says when last seen"
        );
        assert!(absent(&buf, "1 open"));

        app.on_key(KeyEvent::from(KeyCode::Enter));
        app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
        let buf = render(&mut app);
        // Both chats have a tab; the shown one is the second.
        let (_, row) = find(&buf, " 1 new session");
        assert_eq!(find(&buf, " 2 new session").1, row);
        assert_eq!(buf[find(&buf, " 2 new session")].bg, SELECTED);
        assert_ne!(buf[find(&buf, " 1 new session")].bg, SELECTED);

        app.on_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        let buf = render(&mut app);
        assert_eq!(find(&buf, "2 open").1, find(&buf, "box-n1").1);
    }

    #[test]
    fn many_tabs_scroll_round_the_shown_one() {
        let mut app = app();
        app.on_key(KeyEvent::from(KeyCode::Enter));
        for _ in 0..7 {
            app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
        }
        // The last of eight is shown; earlier ones are out of sight to the left.
        let buf = render(&mut app);
        let (_, row) = find(&buf, " 8 new session");
        assert_eq!(buf[find(&buf, " 8 new session")].bg, SELECTED);
        assert_eq!(find(&buf, "‹").1, row);
        assert!(absent(&buf, " 1 new session"));
        assert!(absent(&buf, "›"));
        assert!(absent(&buf, "ctrl-o"), "no room left for the hint");

        // In the middle, both sides are cut.
        for _ in 0..4 {
            app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
        }
        let buf = render(&mut app);
        assert_eq!(buf[find(&buf, " 4 new session")].bg, SELECTED);
        assert_eq!(find(&buf, "‹").1, row);
        assert_eq!(find(&buf, "›").1, row);

        // A long title is cut short rather than pushing the others out.
        let long = "a".repeat(80);
        let id = app.chat().unwrap().id;
        let chat = app.chats.iter_mut().find(|c| c.id == id).unwrap();
        chat.title = long;
        let buf = render(&mut app);
        let (x, _) = find(&buf, " 4 aaaa");
        let row_text: String = (0..buf.area.width)
            .map(|c| buf[(c, row)].symbol())
            .collect();
        assert!(row_text.contains('…'), "{row_text}");
        assert!(x < 30);
    }

    #[test]
    fn the_status_line_says_what_is_loading() {
        let mut app = app();
        app.on_key(KeyEvent::from(KeyCode::Enter));
        let buf = render(&mut app);
        find(&buf, "asking box-n1 what its agent offers…");

        let options = commandant_proto::AgentOptions {
            loading: true,
            ..Default::default()
        };
        app.on_update(Update::Options("n1".into(), Ok(options)));
        let buf = render(&mut app);
        find(&buf, "connecting MCP servers");
        assert!(app.busy(), "its spinner turns");

        let mcp = |name: &str, status: &str| commandant_proto::McpServer {
            name: name.into(),
            status: status.into(),
            ..Default::default()
        };
        let options = commandant_proto::AgentOptions {
            mcp_servers: vec![mcp("docs", "connected"), mcp("x", "failed")],
            ..Default::default()
        };
        app.on_update(Update::Options("n1".into(), Ok(options)));
        let buf = render(&mut app);
        find(&buf, "mcp 1/2 connected · 1 failed");
        assert!(!app.busy());

        app.on_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        let buf = render(&mut app);
        find(&buf, "loading · ");
    }

    #[test]
    fn only_a_known_commands_name_is_colored() {
        let mut app = app();
        app.on_key(KeyEvent::from(KeyCode::Enter));
        let typed = |app: &mut App, text: &str| {
            let id = app.chat().unwrap().id;
            let chat = app.chats.iter_mut().find(|c| c.id == id).unwrap();
            chat.input = Default::default();
            for c in text.chars() {
                app.on_key(KeyEvent::from(KeyCode::Char(c)));
            }
            render(app)
        };
        let accent = Color::Cyan;
        let buf = typed(&mut app, "/model smart");
        assert_eq!(buf[find(&buf, "/model smart")].fg, accent);
        let (x, y) = find(&buf, " smart");
        assert_eq!(buf[(x + 1, y)].fg, Color::Reset, "its argument isn't");
        let buf = typed(&mut app, "/nope x");
        assert_eq!(buf[find(&buf, "/nope x")].fg, Color::Reset, "unknown");
    }

    #[test]
    fn completions_float_over_the_prompt() {
        let mut app = app();
        app.on_key(KeyEvent::from(KeyCode::Enter));
        for c in "/prov".chars() {
            app.on_key(KeyEvent::from(KeyCode::Char(c)));
        }
        let buf = render(&mut app);
        // The empty chat's welcome lists it too: the completion is the lowest.
        let row = (0..buf.area.height)
            .rev()
            .find(|&y| {
                let text: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
                text.contains("/providers ")
            })
            .unwrap();
        assert_eq!(buf[(2, row)].bg, SELECTED);
        let input = (0..buf.area.height)
            .find(|&y| buf[(1, y)].bg == SURFACE)
            .unwrap();
        assert_eq!(row + 2, input, "just over the prompt, past the status line");
    }

    #[test]
    fn an_api_key_is_masked_while_typed() {
        let mut app = app();
        app.on_key(KeyEvent::from(KeyCode::Enter));
        let id = app.chats[0].id;
        let chat = app.chats.iter_mut().find(|c| c.id == id).unwrap();
        chat.auth = Some(Auth::Key {
            provider: "acme".into(),
        });
        for c in "sk-abc".chars() {
            app.on_key(KeyEvent::from(KeyCode::Char(c)));
        }
        let buf = render(&mut app);
        find(&buf, "••••••");
        assert!(absent(&buf, "sk-abc"));
    }

    #[test]
    fn the_harness_picker_floats_over_the_nodes() {
        let bare = crate::tui::app::tests::bare("n1");
        let mut app = App::new(vec![bare], Default::default());
        let buf = render(&mut app);
        find(&buf, "enter to start an agent");
        app.on_key(KeyEvent::from(KeyCode::Enter));
        let buf = render(&mut app);
        find(&buf, "Start an agent on this node");
        find(&buf, "opencode");
        app.on_key(KeyEvent::from(KeyCode::Enter));
        let buf = render(&mut app);
        assert!(absent(&buf, "Start an agent on this node"));
        find(&buf, "starting its agent…");
    }
}
