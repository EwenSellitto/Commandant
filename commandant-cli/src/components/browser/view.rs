use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState},
    Frame,
};

use crate::components::{browser::Browser, RenderContext};

pub(crate) fn render_browser(frame: &mut Frame, browser: &Browser, area: Rect, ctx: RenderContext) {
    let items: Vec<ListItem<'_>> = browser
        .rows()
        .iter()
        .map(|row| ListItem::new(Line::from(Span::raw(row.label.as_str()))))
        .collect();

    let mut state = ListState::default().with_selected(Some(browser.selected_index()));
    let list_style = if browser.force_background() {
        Style::default()
            .bg(Color::Rgb(30, 39, 34))
            .fg(Color::Rgb(225, 230, 222))
    } else {
        Style::default().fg(Color::Rgb(225, 230, 222))
    };
    let block_style = if browser.force_background() {
        Style::default().bg(Color::Rgb(30, 39, 34))
    } else {
        Style::default()
    };
    let border_color = if ctx.focused {
        Color::Rgb(200, 214, 170)
    } else {
        Color::Rgb(110, 140, 120)
    };

    let list = List::new(items)
        .style(list_style)
        .block(
            Block::default()
                .title(" Projects ")
                .borders(Borders::ALL)
                .style(block_style)
                .border_style(Style::default().fg(border_color)),
        )
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Rgb(200, 214, 170))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("");

    frame.render_stateful_widget(list, area, &mut state);
}
