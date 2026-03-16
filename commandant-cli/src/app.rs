use commandant_core::browser::WorkspaceBrowserData;
use crossterm::event::{KeyCode, KeyEvent};

use crate::components::{
    Component, ComponentEffect, browser::Browser, container::Container, help::Help,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FocusedComponent {
    Browser,
    Container,
}

pub(crate) struct App {
    browser: Browser,
    container: Container,
    help: Help,
    force_background: bool,
    focused: FocusedComponent,
}

impl App {
    pub(crate) fn new(data: WorkspaceBrowserData, force_background: bool) -> Self {
        let browser = Browser::new(data, force_background);
        let mut container = Container::new(force_background);
        let help = Help::new(force_background);
        let (title, body) = browser.selected_content();
        container.set_content(title, body);

        Self {
            browser,
            container,
            help,
            force_background,
            focused: FocusedComponent::Browser,
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> bool {
        if matches!(key.code, KeyCode::Char('q')) {
            return true;
        }
        let effect = if self.help.captures_input() {
            self.help.handle_key(key)
        } else if matches!(key.code, KeyCode::Char('?')) {
            self.open_help();
            ComponentEffect::None
        } else if self.is_focus_shortcut(key.code) {
            self.switch_focus(key.code);
            ComponentEffect::None
        } else {
            match self.focused {
                FocusedComponent::Browser => self.browser.handle_key(key),
                FocusedComponent::Container => self.container.handle_key(key),
            }
        };

        let (title, body) = self.browser.selected_content();
        self.container.set_content(title, body);

        matches!(effect, ComponentEffect::Quit)
    }

    fn open_help(&mut self) {
        let content = match self.focused {
            FocusedComponent::Browser => self.browser.help_content(),
            FocusedComponent::Container => self.container.help_content(),
        };

        self.help.show(content);
    }

    fn is_focus_shortcut(&self, key: KeyCode) -> bool {
        matches!(key, KeyCode::Left | KeyCode::Right)
    }

    fn switch_focus(&mut self, key: KeyCode) {
        self.focused = match key {
            KeyCode::Left => FocusedComponent::Browser,
            KeyCode::Right => FocusedComponent::Container,
            _ => self.focused,
        };
    }

    pub(crate) fn browser(&self) -> &Browser {
        &self.browser
    }

    pub(crate) fn container(&self) -> &Container {
        &self.container
    }

    pub(crate) fn help(&self) -> &Help {
        &self.help
    }

    pub(crate) fn force_background(&self) -> bool {
        self.force_background
    }

    pub(crate) fn focused(&self) -> FocusedComponent {
        self.focused
    }
}
