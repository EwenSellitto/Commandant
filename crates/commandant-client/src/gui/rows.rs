//! The thread as rows: what is drawn on its own, what is grouped, and what
//! the scroller is told when the thread changes.

use std::collections::BTreeSet;
use std::ops::Range;

use commandant_client_core::Chat;
use commandant_client_core::chat::{Entry, Role};

/// One row of the thread: an entry, or tool calls in a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// The entry at this index, drawn on its own.
    Entry(usize),
    /// Consecutive tool calls, drawn as one row.
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

    fn entries(&self) -> Range<usize> {
        match self {
            Self::Entry(at) => *at..*at + 1,
            Self::Tools(range) => range.clone(),
        }
    }
}

/// The thread's entries as rows, tool calls in a row grouped.
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

/// How much a row holds, so that what streams into it shows.
fn size(chat: &Chat, row: &Row) -> usize {
    let entries = chat.thread.get(row.entries()).unwrap_or_default();
    let printed = |e: &Entry| -> usize {
        let runs = e.run.iter().flat_map(|r| &r.output);
        runs.map(|p| p.text.len()).sum()
    };
    entries.iter().map(|e| e.text.len() + printed(e)).sum()
}

/// What the scroller knows of the thread, and what came while the reader
/// was scrolled up.
#[derive(Debug, Default)]
pub struct Tally {
    /// The rows the scroller has.
    rows: usize,
    /// How much the last of them held.
    last: usize,
    /// Whether the earlier turns were still loading.
    loading: bool,
    /// Whether the agent was at work.
    working: bool,
    /// The rows added, or added to, since the reader scrolled up.
    unread: BTreeSet<usize>,
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
    /// What `chat`, shown as `rows`, asks of the scroller.
    pub fn change(&mut self, chat: &Chat, rows: &[Row], scrolled_up: bool) -> Change {
        let count = rows.len();
        let working = chat.activity.working();
        let history_came = self.loading && !chat.loading_history;
        let settled = self.working && !working;
        let added = count.saturating_sub(self.rows);
        let shrank = count < self.rows;
        // The row that was last may have been written to since.
        let written = self
            .rows
            .checked_sub(1)
            .filter(|&at| rows.get(at).is_some_and(|row| size(chat, row) != self.last));
        let before = self.rows;
        self.loading = chat.loading_history;
        self.working = working;
        self.rows = count;
        self.last = rows.last().map_or(0, |row| size(chat, row));
        if history_came || shrank {
            self.unread.clear();
            return Change::Reset(count);
        }
        if scrolled_up {
            self.unread.extend(written.into_iter().chain(before..count));
        } else {
            self.unread.clear();
        }
        Change::Grew { added, settled }
    }

    /// How many rows are new to a reader scrolled up.
    pub fn unread(&self) -> usize {
        self.unread.len()
    }

    /// The reader is at the bottom: nothing is new.
    pub fn read(&mut self) {
        self.unread.clear();
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

    /// The rows of the first `n` entries of `chat`.
    fn first(chat: &Chat, n: usize) -> Vec<Row> {
        rows(&chat.thread[..n])
    }

    #[test]
    fn rows_added_or_written_to_while_scrolled_up_are_new() {
        use Role::*;
        let mut tally = Tally::default();
        let mut chat = chat(&[User, Agent, Error, Info], true);
        let grew = |added, settled| Change::Grew { added, settled };
        assert_eq!(tally.change(&chat, &first(&chat, 1), false), grew(1, false));
        assert_eq!(tally.change(&chat, &first(&chat, 2), true), grew(1, false));
        // The reply streams on: still one new row.
        chat.thread[1].text.push_str("more");
        assert_eq!(tally.change(&chat, &first(&chat, 2), true), grew(0, false));
        assert_eq!(tally.unread(), 1);
        assert_eq!(tally.change(&chat, &first(&chat, 3), true), grew(1, false));
        assert_eq!(tally.unread(), 2);
        // Back at the bottom, nothing is new.
        assert_eq!(tally.change(&chat, &first(&chat, 3), false), grew(0, false));
        assert_eq!(tally.unread(), 0);
        // Nor is the prompt the reader was looking at.
        chat.thread[0].text.push_str("typed");
        tally.change(&chat, &first(&chat, 3), true);
        assert_eq!(tally.unread(), 0);

        chat.activity = Activity::Idle;
        assert_eq!(tally.change(&chat, &first(&chat, 4), false), grew(1, true));
    }

    #[test]
    fn the_earlier_turns_coming_start_the_rows_again() {
        let mut tally = Tally::default();
        let mut chat = chat(&[Role::Agent; 6], false);
        chat.loading_history = true;
        tally.change(&chat, &first(&chat, 1), true);
        chat.loading_history = false;
        assert_eq!(
            tally.change(&chat, &first(&chat, 6), true),
            Change::Reset(6)
        );
        assert_eq!(tally.unread(), 0);
    }
}
