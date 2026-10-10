//! What keys say to a question the state asks. One to choose from is a
//! [`Picker`]; a line asked for is typed in the chat's prompt; one to
//! confirm or read is a panel. All but a line take every key while open.

use crossterm::event::{KeyCode, KeyEvent};

use super::picker::Picker;
use commandant_client_core::{Ask, Intent, Scope};

/// What a key says to `ask`, asked in `scope` and shown with `picker` when
/// it is one to choose from; nothing while it stays open.
pub fn on_key(
    ask: &Ask,
    picker: Option<&mut Picker>,
    scope: Scope,
    key: KeyEvent,
) -> Option<Intent> {
    match (ask, key.code) {
        (Ask::Choose(_), _) => picker?.on_key(scope, key),
        (Ask::Confirm { .. }, KeyCode::Enter | KeyCode::Char('y')) => Some(Intent::Confirm(scope)),
        (Ask::Confirm { .. }, KeyCode::Esc | KeyCode::Char('n'))
        | (Ask::Show { .. }, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q')) => {
            Some(Intent::Dismiss(scope))
        }
        _ => None,
    }
}
