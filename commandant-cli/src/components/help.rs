use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::components::{Component, ComponentEffect, HelpContent, RenderContext};

pub(crate) struct Help {
    title: String,
    lines: Vec<String>,
    is_open: bool,
    scroll: u16,
    force_background: bool,
}

impl Help {
    pub(crate) fn new(force_background: bool) -> Self {
        Self {
            title: String::new(),
            lines: Vec::new(),
            is_open: false,
            scroll: 0,
            force_background,
        }
    }

    pub(crate) fn show(&mut self, content: HelpContent) {
        self.title = content.title.to_string();
        self.lines = content
            .lines
            .iter()
            .map(|line| (*line).to_string())
            .collect();
        self.is_open = true;
        self.scroll = 0;
    }

    fn text(&self) -> Text<'_> {
        Text::from(
            self.lines
                .iter()
                .map(|line| Line::from(line.as_str()))
                .collect::<Vec<_>>(),
        )
    }
}

impl Component for Help {
    fn render(&self, frame: &mut Frame, area: Rect, _ctx: RenderContext) {
        if !self.is_open {
            return;
        }

        let backdrop = Block::default().style(Style::default().bg(Color::Rgb(8, 10, 12)));
        let area = centered_rect(area, 70, 70);
        let panel_style = if self.force_background {
            Style::default()
                .bg(Color::Rgb(24, 26, 30))
                .fg(Color::Rgb(228, 229, 231))
        } else {
            Style::default().fg(Color::Rgb(228, 229, 231))
        };
        let help = Paragraph::new(self.text())
            .block(
                Block::default()
                    .title(Line::from(Span::styled(
                        self.title.as_str(),
                        Style::default().add_modifier(Modifier::BOLD),
                    )))
                    .style(panel_style)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Rgb(150, 110, 80))),
            )
            .scroll((self.scroll, 0))
            .wrap(Wrap { trim: false });

        if self.force_background {
            frame.render_widget(backdrop, frame.area());
        }

        frame.render_widget(Clear, area);
        frame.render_widget(help, area);
    }

    fn render_overlay(&self, frame: &mut Frame, area: Rect) {
        self.render(frame, area, RenderContext { focused: true });
    }

    fn captures_input(&self) -> bool {
        self.is_open
    }

    fn handle_key(&mut self, key: KeyEvent) -> ComponentEffect {
        if !self.is_open {
            return ComponentEffect::None;
        }

        match key.code {
            KeyCode::Char('?') | KeyCode::Esc => {
                self.is_open = false;
            }
            KeyCode::Up => {
                self.scroll = self.scroll.saturating_sub(1);
            }
            KeyCode::Down => {
                self.scroll = self.scroll.saturating_add(1);
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(8);
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_add(8);
            }
            KeyCode::Home => {
                self.scroll = 0;
            }
            KeyCode::End => {
                self.scroll = self.lines.len().saturating_sub(1) as u16;
            }
            _ => {}
        }

        ComponentEffect::None
    }

    fn help_content(&self) -> HelpContent {
        HelpContent {
            title: "",
            lines: &[],
        }
    }
}

fn centered_rect(area: Rect, width_percent: u16, height_percent: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - height_percent) / 2),
            Constraint::Percentage(height_percent),
            Constraint::Percentage((100 - height_percent) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_percent) / 2),
            Constraint::Percentage(width_percent),
            Constraint::Percentage((100 - width_percent) / 2),
        ])
        .split(vertical[1])[1]
}
