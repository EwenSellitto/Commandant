//! The panel for a question that is only read, or said yes to: an
//! [`Ask::Confirm`] or an [`Ask::Show`].

use crossterm::event::{KeyCode, KeyEvent};

use crate::state::{Ask, Intent, Scope};

/// Whether `ask` is drawn in this panel, which takes every key while open.
pub fn shown(ask: &Ask) -> bool {
    matches!(ask, Ask::Confirm { .. } | Ask::Show { .. })
}

/// What a key says to the question in the panel, if anything.
pub fn on_key(ask: &Ask, scope: Scope, key: KeyEvent) -> Option<Intent> {
    match (ask, key.code) {
        (Ask::Confirm { .. }, KeyCode::Enter | KeyCode::Char('y')) => Some(Intent::Confirm(scope)),
        (Ask::Confirm { .. }, KeyCode::Esc | KeyCode::Char('n')) => Some(Intent::Dismiss(scope)),
        (Ask::Show { .. }, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')) => {
            Some(Intent::Dismiss(scope))
        }
        _ => None,
    }
}
