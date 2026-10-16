//! What the terminal keeps of a chat (its prompt, where the thread is
//! scrolled) and what keys in it ask for.

use commandant_client_core::chat::Chat;
use commandant_client_core::{Edit, Intent, Scope};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Lines moved by PageUp / PageDown.
const PAGE: u16 = 10;

#[derive(Default)]
pub struct ChatView {
    pub input: Input,
    /// How many lines the thread is scrolled up from the bottom.
    pub scroll: u16,
    /// The highlighted completion of a `/command` being typed.
    pub suggested: usize,
    /// Which prompt sent before is shown, while going through them; what
    /// was being typed before, to come back to.
    recalled: Option<usize>,
    draft: String,
}

impl ChatView {
    /// The commands completing what is typed, with what they do; the
    /// highlighted one is `suggested`.
    pub fn suggestions(&self, chat: &Chat) -> Vec<(String, String)> {
        // A recalled one was complete when sent.
        match self.recalled {
            Some(_) => Vec::new(),
            None => chat.completions(&self.input.text),
        }
    }

    pub fn paste(&mut self, text: &str) {
        self.recalled = None;
        self.input.paste(text);
    }

    pub fn edit(&mut self, edit: Edit) {
        match edit {
            Edit::Clear => self.input.clear(),
            Edit::Insert(text) => self.input.insert_str(&text),
        }
    }

    /// Edits the prompt, or says what the key asks of the chat.
    pub fn on_key(&mut self, chat: &Chat, key: KeyEvent) -> Option<Intent> {
        let id = chat.id;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let suggestions = self.suggestions(chat);
        let completing = !suggestions.is_empty();
        // What's typed changed: back to the best match, and a recalled
        // prompt becomes one being written.
        if matches!(
            key.code,
            KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete
        ) {
            self.suggested = 0;
            self.recalled = None;
        }
        let count = suggestions.len();
        match key.code {
            KeyCode::Up if completing => self.suggested = (self.suggested + count - 1) % count,
            KeyCode::Down if completing => self.suggested = (self.suggested + 1) % count,
            KeyCode::Up => self.recall(chat.sent(), -1),
            KeyCode::Down => self.recall(chat.sent(), 1),
            KeyCode::Tab if completing => self.complete(chat),
            KeyCode::Char('t') if ctrl => return Some(Intent::CycleEffort(id)),
            KeyCode::Tab => return Some(Intent::CycleAgent(id, 1)),
            KeyCode::BackTab => return Some(Intent::CycleAgent(id, -1)),
            KeyCode::Enter => return self.submit(chat),
            // Typing what is asked for, which goes with the line.
            KeyCode::Esc if chat.entering() => {
                self.input.clear();
                return Some(Intent::Dismiss(Scope::Chat(id)));
            }
            KeyCode::Esc => return Some(Intent::Cancel(id)),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_add(PAGE),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(PAGE),
            _ => self.input.edit(key),
        }
        None
    }

    /// Hands over what is typed: the line asked for, which is taken at
    /// once, else a prompt the chat says whether it takes.
    fn submit(&mut self, chat: &Chat) -> Option<Intent> {
        let text = self.input.text.trim().to_string();
        if text.is_empty() {
            return None;
        }
        if chat.entering() {
            self.input.clear();
            return Some(Intent::Enter(Scope::Chat(chat.id), text));
        }
        // A partly typed command runs the one highlighted; a recalled one
        // was complete when sent.
        let text = match self.recalled.take() {
            None => chat.sent_for(&text, self.suggested),
            Some(_) => text,
        };
        Some(Intent::Submit(chat.id, text))
    }

    /// Fills in the highlighted completion, ready for its arguments.
    fn complete(&mut self, chat: &Chat) {
        if let Some(text) = chat.completed(&self.input.text, self.suggested) {
            self.input.set(&text);
        }
        self.suggested = 0;
    }

    /// Shows the prompt sent before (`-1`) or after (`1`) the one shown; past
    /// the latest, what was being typed.
    fn recall(&mut self, sent: &[String], step: isize) {
        let at = match (self.recalled, step) {
            (None, 1) => return,
            (None, _) => {
                self.draft = self.input.text.clone();
                sent.len().checked_sub(1)
            }
            (Some(at), -1) => Some(at.saturating_sub(1)),
            (Some(at), _) => Some(at + 1).filter(|&next| next < sent.len()),
        };
        self.recalled = at;
        match at {
            Some(at) => self.input.set(&sent[at]),
            None => self.input.set(&std::mem::take(&mut self.draft)),
        }
    }
}

/// A single-line text field. `cursor` counts characters, not bytes.
#[derive(Default)]
pub struct Input {
    pub text: String,
    pub cursor: usize,
}

impl Input {
    /// What a key does to the text: typing, deleting, moving the cursor.
    pub fn edit(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('u') if ctrl => self.clear(),
            KeyCode::Char(c) => self.insert(c),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left => self.left(),
            KeyCode::Right => self.right(),
            KeyCode::Home => self.home(),
            KeyCode::End => self.end(),
            _ => {}
        }
    }

    /// Pasted text, on the one line.
    pub fn paste(&mut self, text: &str) {
        self.insert_str(&text.replace(['\r', '\n'], " "));
    }

    fn byte_index(&self) -> usize {
        self.text
            .char_indices()
            .nth(self.cursor)
            .map_or(self.text.len(), |(i, _)| i)
    }

    fn insert(&mut self, c: char) {
        let at = self.byte_index();
        self.text.insert(at, c);
        self.cursor += 1;
    }

    fn insert_str(&mut self, s: &str) {
        let at = self.byte_index();
        self.text.insert_str(at, s);
        self.cursor += s.chars().count();
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.delete();
        }
    }

    fn delete(&mut self) {
        let at = self.byte_index();
        if at < self.text.len() {
            self.text.remove(at);
        }
    }

    fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.text.chars().count());
    }

    fn home(&mut self) {
        self.cursor = 0;
    }

    fn end(&mut self) {
        self.cursor = self.text.chars().count();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Replaces the text, the cursor at its end.
    pub fn set(&mut self, text: &str) {
        self.text = text.to_string();
        self.end();
    }
}
