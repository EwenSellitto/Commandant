//! The first-run screen: a panel to paste the server's link on.

use ratatui::Frame;
use ratatui::layout::Constraint::{Length, Min};
use ratatui::layout::Layout;
use ratatui::style::{Color, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

use super::lists::{draw_line, panel};
use super::{MUTED, hints};
use crate::tui::login::LogIn;

const ACCENT: Color = Color::Cyan;

pub fn draw_log_in(frame: &mut Frame, screen: &LogIn) {
    // The title, the hint (2 rows), the line, what is going on (2 rows) and
    // the keys, a row apart; plus a padding row each end.
    let inner = panel(frame, 13);
    let [title, hint, line, news, keys] =
        Layout::vertical([Length(1), Length(2), Length(1), Min(1), Length(1)])
            .spacing(1)
            .areas(inner);
    frame.render_widget("Connect to your server".bold().fg(ACCENT), title);
    let paste = "Paste the connection link `commandant-server serve` printed.".fg(MUTED);
    frame.render_widget(Paragraph::new(paste).wrap(Wrap { trim: true }), hint);
    draw_line(frame, ACCENT, &screen.link, line);
    let said = if screen.connecting {
        Line::from("connecting…".fg(MUTED))
    } else {
        Line::from(screen.error.as_deref().unwrap_or_default()).fg(Color::Red)
    };
    frame.render_widget(Paragraph::new(said).wrap(Wrap { trim: true }), news);
    let keys_shown: &[(&str, &str)] = if screen.connecting {
        &[("esc", "give up")]
    } else {
        &[("enter", "connect"), ("esc", "quit")]
    };
    frame.render_widget(hints(keys_shown), keys);
}
