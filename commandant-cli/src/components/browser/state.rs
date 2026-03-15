use commandant_core::browser::{Project, WorkspaceBrowserData};
use crossterm::event::{KeyCode, KeyEvent};

use crate::components::{Component, ComponentEffect, HelpContent, RenderContext};

#[derive(Clone, Copy)]
enum RowKind {
    Project {
        project_index: usize,
    },
    Conversation {
        project_index: usize,
        conversation_index: usize,
    },
}

#[derive(Clone)]
pub(crate) struct VisibleRow {
    pub(crate) label: String,
    kind: RowKind,
}

pub(crate) struct Browser {
    data: WorkspaceBrowserData,
    expanded: Vec<bool>,
    rows: Vec<VisibleRow>,
    selected_index: usize,
    force_background: bool,
}

impl Browser {
    pub(crate) fn new(data: WorkspaceBrowserData, force_background: bool) -> Self {
        let expanded = vec![true; data.projects.len()];
        let mut browser = Self {
            data,
            expanded,
            rows: Vec::new(),
            selected_index: 0,
            force_background,
        };
        browser.refresh_rows();
        browser.ensure_valid_selection();
        browser
    }

    pub(crate) fn rows(&self) -> &[VisibleRow] {
        &self.rows
    }

    pub(crate) fn selected_index(&self) -> usize {
        self.selected_index
    }

    pub(crate) fn selected_content(&self) -> (String, String) {
        let Some(row) = self.rows.get(self.selected_index) else {
            return (
                " Details ".to_string(),
                "No projects available yet.".to_string(),
            );
        };

        match row.kind {
            RowKind::Project { project_index } => {
                let project = &self.data.projects[project_index];
                let state = if self.expanded[project_index] {
                    "open"
                } else {
                    "closed"
                };

                (
                    format!(" Project: {} ", project.name),
                    format!(
                        "Drawer state: {state}\n\nConversations: {}\n\nSelect a conversation to read its text in this pane.",
                        project.conversations.len()
                    ),
                )
            }
            RowKind::Conversation {
                project_index,
                conversation_index,
            } => {
                let project: &Project = &self.data.projects[project_index];
                let conversation = &project.conversations[conversation_index];
                (
                    format!(" {} / {} ", project.name, conversation.name),
                    conversation.text.clone(),
                )
            }
        }
    }

    pub(crate) fn force_background(&self) -> bool {
        self.force_background
    }

    fn move_selection_up(&mut self) {
        if self.rows.is_empty() {
            return;
        }

        self.selected_index = self.selected_index.saturating_sub(1);
    }

    fn move_selection_down(&mut self) {
        if self.rows.is_empty() {
            return;
        }

        let last_index = self.rows.len().saturating_sub(1);
        self.selected_index = (self.selected_index + 1).min(last_index);
    }

    fn toggle_current_drawer(&mut self) {
        if let Some(row) = self.rows.get(self.selected_index) {
            match row.kind {
                RowKind::Project { project_index } => {
                    if let Some(expanded) = self.expanded.get_mut(project_index) {
                        *expanded = !*expanded;
                        self.refresh_rows();
                        self.select_project(project_index);
                    }
                }
                RowKind::Conversation { .. } => {}
            }
        }
    }

    fn refresh_rows(&mut self) {
        self.rows.clear();

        for (project_index, project) in self.data.projects.iter().enumerate() {
            let drawer = if self.expanded[project_index] {
                "[-]"
            } else {
                "[+]"
            };
            self.rows.push(VisibleRow {
                kind: RowKind::Project { project_index },
                label: format!("{drawer} {}", project.name),
            });

            if self.expanded[project_index] {
                for (conversation_index, conversation) in project.conversations.iter().enumerate() {
                    self.rows.push(VisibleRow {
                        kind: RowKind::Conversation {
                            project_index,
                            conversation_index,
                        },
                        label: format!("  {}", conversation.name),
                    });
                }
            }
        }

        self.ensure_valid_selection();
    }

    fn ensure_valid_selection(&mut self) {
        if self.rows.is_empty() {
            self.selected_index = 0;
            return;
        }

        self.selected_index = self.selected_index.min(self.rows.len() - 1);
    }

    fn select_project(&mut self, project_index: usize) {
        if let Some((row_index, _)) = self.rows.iter().enumerate().find(
            |(_, row)| matches!(row.kind, RowKind::Project { project_index: idx } if idx == project_index),
        ) {
            self.selected_index = row_index;
        }
    }
}

impl Component for Browser {
    fn render(&self, frame: &mut ratatui::Frame, area: ratatui::layout::Rect, ctx: RenderContext) {
        crate::components::browser::view::render_browser(frame, self, area, ctx);
    }

    fn handle_key(&mut self, key: KeyEvent) -> ComponentEffect {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => ComponentEffect::Quit,
            KeyCode::Up => {
                self.move_selection_up();
                ComponentEffect::None
            }
            KeyCode::Down => {
                self.move_selection_down();
                ComponentEffect::None
            }
            KeyCode::Home => {
                self.selected_index = 0;
                ComponentEffect::None
            }
            KeyCode::End => {
                if let Some(last_index) = self.rows.len().checked_sub(1) {
                    self.selected_index = last_index;
                }
                ComponentEffect::None
            }
            KeyCode::Enter => {
                self.toggle_current_drawer();
                ComponentEffect::None
            }
            _ => ComponentEffect::None,
        }
    }

    fn help_content(&self) -> HelpContent {
        HelpContent {
            title: " Help ",
            lines: &[
                "Bindings",
                "",
                "Navigation",
                "  Left        Focus the browser pane",
                "  Right       Focus the details pane",
                "  Up          Move selection up",
                "  Down        Move selection down",
                "  Home        Jump to the first row",
                "  End         Jump to the last row",
                "",
                "Drawers",
                "  Enter       Toggle the selected project drawer",
                "",
                "Help",
                "  ?           Open or close this modal",
                "  Up/Down     Scroll help while the modal is open",
                "  PageUp      Scroll help by a page",
                "  PageDown    Scroll help by a page",
                "  Home/End    Jump to the top or bottom of help",
                "  Esc         Close the modal",
                "",
                "Quit",
                "  q           Quit the app when help is closed",
            ],
        }
    }
}
