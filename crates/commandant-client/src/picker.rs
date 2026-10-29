//! A list narrowed down by typing, and what answers an [`Ask::Choose`]
//! with one: driven by keys in the TUI, by the GUI's filter field.
//!
//! [`Ask::Choose`]: commandant_client_core::Ask::Choose

use commandant_client_core::{Choice, Choices, Intent, Scope};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Rows moved by PageUp / PageDown.
const PAGE: usize = 10;

/// What a [`Filtered`] shows of a row, and what typing narrows it down by.
pub trait Row: Clone {
    fn label(&self) -> &str;
    /// Said after the label.
    fn detail(&self) -> &str;
}

impl Row for Choice {
    fn label(&self) -> &str {
        &self.label
    }

    fn detail(&self) -> &str {
        &self.detail
    }
}

/// Rows narrowed down by typing, one of them highlighted.
pub struct Filtered<R> {
    rows: Vec<R>,
    pub filter: String,
    /// Indexes into the rows that match the filter.
    shown: Vec<usize>,
    /// Position in `shown`.
    pub selected: usize,
}

/// What a key that closes a [`Filtered`] says.
#[derive(Debug, PartialEq)]
pub enum Picked<R> {
    Closed,
    Chosen(R),
}

impl<R: Row> Filtered<R> {
    /// `rows` narrowed down by `filter`, as if it had been typed;
    /// unfiltered, on row `selected`.
    pub fn new(rows: Vec<R>, filter: &str, selected: usize) -> Self {
        let mut list = Self {
            shown: (0..rows.len()).collect(),
            rows,
            filter: String::new(),
            selected,
        };
        if !filter.is_empty() {
            list.set_filter(filter);
        }
        list
    }

    /// The rows that match the filter, in order.
    pub fn shown(&self) -> impl Iterator<Item = &R> {
        self.shown.iter().map(|&i| &self.rows[i])
    }

    pub fn shown_len(&self) -> usize {
        self.shown.len()
    }

    pub fn total(&self) -> usize {
        self.rows.len()
    }

    /// What a key says to the list; nothing while it stays open.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<Picked<R>> {
        match key.code {
            KeyCode::Esc => return Some(Picked::Closed),
            KeyCode::Enter => return self.picked().cloned().map(Picked::Chosen),
            KeyCode::Up => self.step(-1),
            KeyCode::Down => self.step(1),
            KeyCode::PageUp => self.step(-(PAGE as isize)),
            KeyCode::PageDown => self.step(PAGE as isize),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.set_filter("")
            }
            KeyCode::Char(c) => self.set_filter(&format!("{}{c}", self.filter)),
            KeyCode::Backspace => {
                let mut filter = self.filter.clone();
                filter.pop();
                self.set_filter(&filter);
            }
            _ => {}
        }
        None
    }

    /// The highlighted row, if any matches.
    pub fn picked(&self) -> Option<&R> {
        self.shown().nth(self.selected)
    }

    /// Moves the highlight `rows` down (up if negative), stopping at the ends.
    pub fn step(&mut self, rows: isize) {
        let last = self.shown.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(rows).min(last);
    }

    /// Narrows the rows down to those whose label or detail has every word
    /// of `filter`, back on the best match.
    pub fn set_filter(&mut self, filter: &str) {
        self.filter = filter.to_string();
        let words: Vec<String> = filter.split_whitespace().map(str::to_lowercase).collect();
        let rows = &self.rows;
        self.shown = (0..rows.len())
            .filter(|&i| {
                let haystack = format!("{} {}", rows[i].label(), rows[i].detail()).to_lowercase();
                words.iter().all(|word| haystack.contains(word))
            })
            .collect();
        self.selected = 0;
    }
}

/// An [`Ask::Choose`](commandant_client_core::Ask::Choose), as shown.
pub struct Picker {
    /// What it offers, as the state last had it; its choices are the list's.
    choices: Choices,
    pub list: Filtered<Choice>,
}

impl Picker {
    /// Shows `choices` narrowed down by `filter`, as if it had been typed;
    /// unfiltered, on the current choice.
    fn new(choices: &Choices, filter: &str) -> Self {
        let mut choices = choices.clone();
        let rows = std::mem::take(&mut choices.choices);
        let current = choices.current.as_ref();
        let selected = current
            .and_then(|current| rows.iter().position(|c| &c.value == current))
            .unwrap_or(0);
        Self {
            list: Filtered::new(rows, filter, selected),
            choices,
        }
    }

    /// The picker to show `choices` with, given the one `shown`: a new one as
    /// it opens, the same one, its filter kept, as its choices change.
    pub fn sync(shown: Option<Self>, choices: Option<&Choices>) -> Option<Self> {
        let choices = choices?;
        Some(match shown {
            Some(shown) if shown.choices.revision == choices.revision => shown,
            Some(shown) if shown.choices.id == choices.id => Self::new(choices, &shown.list.filter),
            _ => Self::new(choices, &choices.filter),
        })
    }

    /// The question it shows, told from the others.
    #[cfg(feature = "gui")]
    pub fn id(&self) -> u64 {
        self.choices.id
    }

    pub fn title(&self) -> &'static str {
        self.choices.title
    }

    /// More choices are on their way.
    pub fn loading(&self) -> bool {
        self.choices.loading
    }

    /// What a key says to the question asked in `scope`; nothing while
    /// the picker stays open.
    pub fn on_key(&mut self, scope: Scope, key: KeyEvent) -> Option<Intent> {
        Some(match self.list.on_key(key)? {
            Picked::Closed => Intent::Dismiss(scope),
            Picked::Chosen(choice) => Intent::Choose(scope, choice.value),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commandant_client_core::Choose;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    fn model(id: &str, label: &str, detail: &str) -> Choice {
        Choice::new(Choose::Model(id.into()), label, detail)
    }

    #[test]
    fn filters_by_every_word_and_picks_the_selection() {
        let choices = vec![
            model("", "Default", ""),
            model("anthropic/claude-sonnet-5", "Claude Sonnet 5", "Anthropic"),
            model(
                "anthropic/claude-haiku-4-5",
                "Claude Haiku 4.5",
                "Anthropic",
            ),
            model("openai/gpt-6", "GPT-6", "OpenAI"),
        ];
        let current = Some(Choose::Model("openai/gpt-6".into()));
        let mut picker = Picker::new(&Choices::new("Model", choices, current), "");
        assert_eq!(picker.list.selected, 3);
        let mut press = |code| picker.on_key(Scope::App, key(code));

        for c in "anth son".chars() {
            assert_eq!(press(KeyCode::Char(c)), None);
        }
        for code in [KeyCode::Backspace, KeyCode::Backspace, KeyCode::Backspace] {
            press(code);
        }
        press(KeyCode::Down);
        press(KeyCode::Down);
        let haiku = Choose::Model("anthropic/claude-haiku-4-5".into());
        assert_eq!(
            press(KeyCode::Enter),
            Some(Intent::Choose(Scope::App, haiku))
        );

        for c in "zzz".chars() {
            press(KeyCode::Char(c));
        }
        assert_eq!(press(KeyCode::Enter), None, "nothing to choose");
        assert_eq!(press(KeyCode::Esc), Some(Intent::Dismiss(Scope::App)));
        assert_eq!(picker.list.shown_len(), 0);
    }

    #[test]
    fn narrows_by_every_word_typed() {
        let choices = vec![
            model("a", "Claude Sonnet 5", "Anthropic"),
            model("b", "Claude Haiku 4.5", "Anthropic"),
        ];
        let mut picker = Picker::new(&Choices::new("Model", choices, None), "");
        for c in "anth son".chars() {
            picker.on_key(Scope::App, key(KeyCode::Char(c)));
        }
        let labels: Vec<_> = picker.list.shown().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["Claude Sonnet 5"]);
    }

    #[test]
    fn a_list_is_driven_without_keys() {
        let rows = vec![
            model("a", "Claude Sonnet 5", "Anthropic"),
            model("b", "Claude Haiku 4.5", "Anthropic"),
            model("c", "GPT-6", "OpenAI"),
        ];
        let mut list = Filtered::new(rows, "", 0);
        list.set_filter("claude");
        list.step(5);
        assert_eq!(
            list.picked().map(|c| c.label.as_str()),
            Some("Claude Haiku 4.5")
        );
        list.step(-5);
        assert_eq!(
            list.picked().map(|c| c.label.as_str()),
            Some("Claude Sonnet 5")
        );
        list.set_filter("zzz");
        assert_eq!(list.picked(), None);
    }

    #[test]
    fn a_filter_given_up_front_narrows_like_typing() {
        let choices = vec![model("1", "one", "first"), model("2", "two", "second")];
        let pick = Choices::new("Numbers", choices, None).with_filter("SEC");
        let picker = Picker::sync(None, Some(&pick)).unwrap();
        assert_eq!(
            (picker.list.filter.as_str(), picker.list.shown_len()),
            ("SEC", 1)
        );
    }

    #[test]
    fn a_picker_follows_its_pick() {
        let pick = Choices::new("Numbers", vec![model("1", "one", "")], None);
        let mut shown = Picker::sync(None, Some(&pick)).unwrap();
        shown.list.filter = "on".into();

        // Revised, it keeps what was typed; another one starts afresh.
        let mut revised = Choices::new("Numbers", vec![model("2", "two", "")], None);
        revised.id = pick.id;
        let shown = Picker::sync(Some(shown), Some(&revised)).unwrap();
        assert_eq!((shown.list.filter.as_str(), shown.list.total()), ("on", 1));
        let other = Choices::new("Other", Vec::new(), None);
        let shown = Picker::sync(Some(shown), Some(&other)).unwrap();
        assert_eq!(shown.list.filter, "");
        assert!(Picker::sync(Some(shown), None).is_none());
    }
}
