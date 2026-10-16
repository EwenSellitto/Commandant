//! The thread as rows: what is drawn on its own, what is grouped, and what
//! the scroller is told when the thread changes.

use std::ops::Range;

use commandant_client_core::Chat;
use commandant_client_core::chat::{Entry, Role};

/// One row of the thread: an entry, or tool calls in a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Entry(usize),
    Tools(Range<usize>),
}

impl Row {
    /// Its first entry, which names it while the thread only grows.
    pub fn first(&self) -> usize {
        match self {
            Self::Entry(at) => *at,
            Self::Tools(range) => range.start,
        }
    }
}

pub fn rows(thread: &[Entry]) -> Vec<Row> {
    let mut rows = Vec::new();
    for (at, entry) in thread.iter().enumerate() {
        match rows.last_mut() {
            Some(Row::Tools(range)) if entry.role == Role::Tool => range.end = at + 1,
            _ if entry.role == Role::Tool => rows.push(Row::Tools(at..at + 1)),
            _ => rows.push(Row::Entry(at)),
        }
    }
    rows
}

/// Whether the entry at `at` is part of the turn the agent is still at,
/// whose thinking is shown in full.
pub fn running(chat: &Chat, at: usize) -> bool {
    let asked = chat.thread.iter().rposition(|e| e.role == Role::User);
    chat.activity.working() && asked.is_none_or(|asked| at > asked)
}

/// What the scroller knows of the thread, and the rows added since the
/// reader scrolled up.
#[derive(Debug, Default)]
pub struct Tally {
    rows: usize,
    loading: bool,
    working: bool,
    pub new: usize,
}

/// What the scroller is to do once the thread has changed.
#[derive(Debug, PartialEq, Eq)]
pub enum Change {
    /// Start again with this many rows: the earlier turns came.
    Reset(usize),
    /// Rows added at the end, then every row measured again if the turn
    /// ended, as its thinking folds up.
    Grew { added: usize, settled: bool },
}

impl Tally {
    /// What `chat`, shown as `rows` rows, asks of the scroller.
    pub fn change(&mut self, chat: &Chat, rows: usize, scrolled_up: bool) -> Change {
        let working = chat.activity.working();
        let came = self.loading && !chat.loading_history;
        let settled = self.working && !working;
        let added = rows.saturating_sub(self.rows);
        let shrank = rows < self.rows;
        self.loading = chat.loading_history;
        self.working = working;
        self.rows = rows;
        if came || shrank {
            self.new = 0;
            return Change::Reset(rows);
        }
        self.new = if scrolled_up { self.new + added } else { 0 };
        Change::Grew { added, settled }
    }
}

#[cfg(test)]
mod tests {
    use commandant_client_core::chat::Activity;

    use super::*;

    fn entry(role: Role) -> Entry {
        Entry {
            role,
            text: String::new(),
            agent: String::new(),
            run: None,
        }
    }

    fn chat(roles: &[Role], working: bool) -> Chat {
        let mut chat = Chat::default();
        chat.thread = roles.iter().map(|&role| entry(role)).collect();
        if working {
            chat.activity = Activity::Working {
                task_id: None,
                since: std::time::Instant::now(),
                cancelling: false,
                agent: String::new(),
                effort: String::new(),
            };
        }
        chat
    }

    #[test]
    fn tool_calls_in_a_row_are_one_row() {
        use Role::*;
        let chat = chat(&[User, Tool, Tool, Thinking, Tool, Agent], false);
        assert_eq!(
            rows(&chat.thread),
            [
                Row::Entry(0),
                Row::Tools(1..3),
                Row::Entry(3),
                Row::Tools(4..5),
                Row::Entry(5)
            ]
        );
    }

    #[test]
    fn only_the_running_turn_is_running() {
        use Role::*;
        let thread = [User, Thinking, Agent, Summary, User, Thinking];
        let working = chat(&thread, true);
        assert!(!running(&working, 1), "an earlier turn's thinking");
        assert!(!running(&working, 4), "the prompt itself");
        assert!(running(&working, 5));
        assert!(!running(&chat(&thread, false), 5), "once it has ended");
    }

    #[test]
    fn rows_added_while_scrolled_up_are_new() {
        use Role::*;
        let mut tally = Tally::default();
        let mut chat = chat(&[User], true);
        let grew = |added, settled| Change::Grew { added, settled };
        assert_eq!(tally.change(&chat, 1, false), grew(1, false));
        assert_eq!(tally.change(&chat, 3, true), grew(2, false));
        assert_eq!(tally.change(&chat, 4, true), grew(1, false));
        assert_eq!(tally.new, 3);
        // Back at the bottom, nothing is new.
        assert_eq!(tally.change(&chat, 4, false), grew(0, false));
        assert_eq!(tally.new, 0);

        chat.activity = Activity::Idle;
        assert_eq!(tally.change(&chat, 5, false), grew(1, true));
    }

    #[test]
    fn the_earlier_turns_coming_start_the_rows_again() {
        let mut tally = Tally::default();
        let mut chat = chat(&[Role::Agent], false);
        chat.loading_history = true;
        tally.change(&chat, 1, true);
        chat.loading_history = false;
        assert_eq!(tally.change(&chat, 6, true), Change::Reset(6));
        assert_eq!(tally.new, 0);
    }
}
