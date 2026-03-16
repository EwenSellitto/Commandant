pub(crate) mod browser;
pub(crate) mod container;
pub(crate) mod help;

use crossterm::event::KeyEvent;
use ratatui::{Frame, layout::Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RenderContext {
    pub(crate) focused: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComponentEffect {
    None,
    Quit,
}

#[derive(Clone, Copy)]
pub(crate) struct HelpContent {
    pub(crate) title: &'static str,
    pub(crate) lines: &'static [&'static str],
}

pub(crate) trait Component {
    fn render(&self, frame: &mut Frame, area: Rect, ctx: RenderContext);

    fn render_overlay(&self, _frame: &mut Frame, _area: Rect) {}

    fn captures_input(&self) -> bool {
        false
    }

    fn handle_key(&mut self, _key: KeyEvent) -> ComponentEffect {
        ComponentEffect::None
    }

    fn help_content(&self) -> HelpContent;
}
