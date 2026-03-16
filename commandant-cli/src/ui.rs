use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::Block,
};

use crate::{
    app::{App, FocusedComponent},
    components::{Component, RenderContext},
};

pub(crate) fn render(frame: &mut Frame, app: &App) {
    let full_area = frame.area();
    let background = Block::default().style(Style::default().bg(Color::Rgb(18, 22, 24)));
    let sections = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
        .split(full_area);

    if app.force_background() {
        frame.render_widget(background, full_area);
    }
    app.browser().render(
        frame,
        sections[0],
        RenderContext {
            focused: app.focused() == FocusedComponent::Browser,
        },
    );
    app.container().render(
        frame,
        sections[1],
        RenderContext {
            focused: app.focused() == FocusedComponent::Container,
        },
    );
    app.help().render_overlay(frame, full_area);
}
