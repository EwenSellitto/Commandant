//! The thread as rows: what is drawn on its own, what is grouped, and what
//! the scroller is told when the thread changes.

use std::collections::BTreeSet;
use std::ops::Range;

use commandant_client_core::Chat;
use commandant_client_core::chat::{Ended, Entry, Role};

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

/// What a row holds, so that what changes in it shows: how much text has
/// come into it, and how far each of its commands has got, whose Stop and
/// end take a line.
#[derive(Debug, PartialEq)]
struct Held {
    bytes: usize,
    runs: Vec<(bool, Option<Ended>)>,
}

fn held(chat: &Chat, row: &Row) -> Held {
    let entries = chat.thread.get(row.entries()).unwrap_or_default();
    // Printed in all: what a block keeps may stay the same size as it prints.
    let printed = |e: &Entry| e.run.as_ref().map_or(0, |r| r.printed);
    let runs = entries.iter().filter_map(|e| e.run.as_ref());
    Held {
        bytes: entries.iter().map(|e| e.text.len() + printed(e)).sum(),
        runs: runs.map(|r| (r.stopping(), r.ended.clone())).collect(),
    }
}

/// What the scroller knows of the thread, and what came while the reader
/// was scrolled up.
#[derive(Debug, Default)]
pub struct Tally {
    /// What each row the scroller has held.
    held: Vec<Held>,
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
    /// Rows added at the end, and earlier rows written to, then every row
    /// measured again if the turn ended, as its thinking folds up.
    Grew {
        added: usize,
        written: Vec<usize>,
        settled: bool,
    },
}

impl Tally {
    /// What `chat`, shown as `rows`, asks of the scroller.
    pub fn change(&mut self, chat: &Chat, rows: &[Row], scrolled_up: bool) -> Change {
        let count = rows.len();
        let working = chat.activity.working();
        let history_came = self.loading && !chat.loading_history;
        let settled = self.working && !working;
        let before = self.held.len();
        let added = count.saturating_sub(before);
        let shrank = count < before;
        let held: Vec<Held> = rows.iter().map(|row| held(chat, row)).collect();
        // The last row streams; a command's block prints wherever it is.
        let written: Vec<usize> = (self.held.iter().zip(&held).enumerate())
            .filter(|(_, (was, is))| was != is)
            .map(|(at, _)| at)
            .collect();
        self.loading = chat.loading_history;
        self.working = working;
        self.held = held;
        if history_came || shrank {
            self.unread.clear();
            return Change::Reset(count);
        }
        if scrolled_up {
            self.unread
                .extend(written.iter().copied().chain(before..count));
        } else {
            self.unread.clear();
        }
        Change::Grew {
            added,
            written,
            settled,
        }
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
    use commandant_client_core::chat::{Activity, Ended, Printed, Run};

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
                watched: false,
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
        let grew = |added, written: &[usize], settled| Change::Grew {
            added,
            written: written.to_vec(),
            settled,
        };
        let change = tally.change(&chat, &first(&chat, 1), false);
        assert_eq!(change, grew(1, &[], false));
        let change = tally.change(&chat, &first(&chat, 2), true);
        assert_eq!(change, grew(1, &[], false));
        // The reply streams on: still one new row.
        chat.thread[1].text.push_str("more");
        let change = tally.change(&chat, &first(&chat, 2), true);
        assert_eq!(change, grew(0, &[1], false));
        assert_eq!(tally.unread(), 1);
        let change = tally.change(&chat, &first(&chat, 3), true);
        assert_eq!(change, grew(1, &[], false));
        assert_eq!(tally.unread(), 2);
        // Back at the bottom, nothing is new.
        let change = tally.change(&chat, &first(&chat, 3), false);
        assert_eq!(change, grew(0, &[], false));
        assert_eq!(tally.unread(), 0);

        chat.activity = Activity::Idle;
        let change = tally.change(&chat, &first(&chat, 4), false);
        assert_eq!(change, grew(1, &[], true));
    }

    #[test]
    fn a_command_printing_above_later_rows_is_measured_again_and_new() {
        use Role::*;
        let mut tally = Tally::default();
        let mut chat = chat(&[User, Command, Agent], true);
        chat.thread[1].run = Some(Run::new(0, usize::MAX));
        let all = rows(&chat.thread);
        tally.change(&chat, &all, false);
        let run = chat.thread[1].run.as_mut().unwrap();
        run.output.push(Printed {
            stderr: false,
            text: "total 0\n".into(),
        });
        run.printed += 8;
        assert_eq!(
            tally.change(&chat, &all, true),
            Change::Grew {
                added: 0,
                written: vec![1],
                settled: false
            }
        );
        assert_eq!(tally.unread(), 1);
        // Past what is kept, a new line takes the place of the first.
        let run = chat.thread[1].run.as_mut().unwrap();
        run.output[0].text = "total 1\n".into();
        run.printed += 8;
        let change = tally.change(&chat, &all, false);
        assert!(matches!(change, Change::Grew { written, .. } if written == [1]));
        // Its end shows in place of "running…".
        let ended = Some(Ended::Failed("it couldn't start".into()));
        chat.thread[1].run.as_mut().unwrap().ended = ended;
        let change = tally.change(&chat, &all, false);
        assert!(matches!(change, Change::Grew { written, .. } if written == [1]));
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
