use crossterm::event::KeyEvent;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::components::{Component, ComponentEffect, HelpContent, RenderContext};

pub(crate) struct Container {
    title: String,
    body: String,
    force_background: bool,
}

impl Container {
    pub(crate) fn new(force_background: bool) -> Self {
        Self {
            title: String::new(),
            body: String::new(),
            force_background,
        }
    }

    pub(crate) fn set_content(&mut self, title: String, body: String) {
        self.title = title;
        self.body = body;
    }
}

impl Component for Container {
    fn render(&self, frame: &mut Frame, area: Rect, ctx: RenderContext) {
        let block_style = if self.force_background {
            Style::default().bg(Color::Rgb(39, 30, 27))
        } else {
            Style::default()
        };
        let content_style = if self.force_background {
            Style::default()
                .bg(Color::Rgb(39, 30, 27))
                .fg(Color::Rgb(232, 222, 215))
        } else {
            Style::default().fg(Color::Rgb(232, 222, 215))
        };
        let border_color = if ctx.focused {
            Color::Rgb(217, 181, 141)
        } else {
            Color::Rgb(70, 70, 70)
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .style(block_style)
            .border_style(Style::default().fg(border_color));

        let content_area = area.inner(Margin {
            vertical: 1,
            horizontal: 2,
        });
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(content_area);

        let title = Paragraph::new(Line::from(Span::styled(
            self.title.as_str(),
            Style::default()
                .fg(Color::Rgb(150, 110, 80))
                .add_modifier(Modifier::BOLD),
        )))
        .alignment(Alignment::Center)
        .style(block_style);
        let body = Paragraph::new(self.body.as_str())
            .wrap(Wrap { trim: false })
            .style(content_style);

        frame.render_widget(block, area);
        frame.render_widget(title, layout[0]);
        frame.render_widget(body, layout[1]);
    }

    fn handle_key(&mut self, _key: KeyEvent) -> ComponentEffect {
        ComponentEffect::None
    }

    fn help_content(&self) -> HelpContent {
        HelpContent {
            title: " Help ",
            lines: &[" Hello world"],
        }
    }
}
