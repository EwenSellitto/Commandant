//! The first-run screen: a panel to paste the server's link on.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Padding, Paragraph, Wrap};

use super::lists::draw_line;
use super::{MUTED, PANEL, hints};
use crate::tui::login::LogIn;

const ACCENT: Color = Color::Cyan;

pub fn draw_log_in(frame: &mut Frame, screen: &LogIn) {
    let screen_area = frame.area();
    let width = screen_area.width.saturating_sub(4).min(80);
    // Title and gap, the hint and gap, the line and gap, what is going on
    // and gap, hints; plus a padding row each end.
    let height = 13.min(screen_area.height.saturating_sub(2));
    let area = screen_area.centered(Constraint::Length(width), Constraint::Length(height));
    let panel = Block::new().bg(PANEL).padding(Padding::new(2, 2, 1, 1));
    let inner = panel.inner(area);
    frame.render_widget(panel, area);
    let [title, _, hint, _, line, _, news, _, keys] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    let heading = Span::raw("Connect to your server").bold().fg(ACCENT);
    frame.render_widget(heading, title);
    let paste = "Paste the connection link `commandant-server serve` printed.".fg(MUTED);
    frame.render_widget(Paragraph::new(paste).wrap(Wrap { trim: true }), hint);
    draw_line(frame, ACCENT, &screen.link, line);
    let said = match (&screen.error, screen.connecting) {
        (_, true) => Paragraph::new("connecting…".fg(MUTED)),
        (Some(error), false) => {
            Paragraph::new(Line::from(error.as_str()).fg(Color::Red)).wrap(Wrap { trim: true })
        }
        (None, false) => Paragraph::default(),
    };
    frame.render_widget(said, news);
    let keys_shown: &[(&str, &str)] = match screen.connecting {
        true => &[("esc", "give up")],
        false => &[("enter", "connect"), ("esc", "quit")],
    };
    frame.render_widget(hints(keys_shown), keys);
}
