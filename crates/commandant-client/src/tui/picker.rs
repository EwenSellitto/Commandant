//! The floating window for answering an [`Ask::Choose`] narrowed down by
//! typing.
//!
//! [`Ask::Choose`]: commandant_client_core::Ask::Choose

use commandant_client_core::{Choice, Choices, Intent, Scope};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Rows moved by PageUp / PageDown.
const PAGE: usize = 10;

pub struct Picker {
    /// What it offers, as the state last had it.
    choices: Choices,
    pub filter: String,
    /// Indexes into the choices that match the filter.
    shown: Vec<usize>,
    /// Position in `shown`.
    pub selected: usize,
}

impl Picker {
    /// Shows `choices` narrowed down by `filter`, as if it had been typed;
    /// unfiltered, on the current choice.
    fn new(choices: &Choices, filter: &str) -> Self {
        let current = choices.current.as_ref();
        let selected = current
            .and_then(|current| choices.choices.iter().position(|c| &c.value == current))
            .unwrap_or(0);
        let mut picker = Self {
            shown: (0..choices.choices.len()).collect(),
            choices: choices.clone(),
            filter: String::new(),
            selected,
        };
        if !filter.is_empty() {
            picker.filter = filter.to_string();
            picker.refilter();
        }
        picker
    }

    /// The picker to show `choices` with, given the one `shown`: a new one as
    /// it opens, the same one, its filter kept, as its choices change.
    pub fn sync(shown: Option<Self>, choices: Option<&Choices>) -> Option<Self> {
        let choices = choices?;
        Some(match shown {
            Some(shown) if shown.choices.revision == choices.revision => shown,
            Some(shown) if shown.choices.id == choices.id => Self::new(choices, &shown.filter),
            _ => Self::new(choices, &choices.filter),
        })
    }

    pub fn title(&self) -> &'static str {
        self.choices.title
    }

    /// More choices are on their way.
    pub fn loading(&self) -> bool {
        self.choices.loading
    }

    /// The choices that match the filter, in order.
    pub fn shown(&self) -> impl Iterator<Item = &Choice> {
        self.shown.iter().map(|&i| &self.choices.choices[i])
    }

    pub fn shown_len(&self) -> usize {
        self.shown.len()
    }

    pub fn total(&self) -> usize {
        self.choices.choices.len()
    }

    /// What a key says to the question asked in `scope`; nothing while
    /// the picker stays open.
    pub fn on_key(&mut self, scope: Scope, key: KeyEvent) -> Option<Intent> {
        match key.code {
            KeyCode::Esc => return Some(Intent::Dismiss(scope)),
            KeyCode::Enter => {
                let choice = self.shown().nth(self.selected)?;
                return Some(Intent::Choose(scope, choice.value.clone()));
            }
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.move_down(1),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(PAGE),
            KeyCode::PageDown => self.move_down(PAGE),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter.clear();
                self.refilter();
            }
            KeyCode::Char(c) => {
                self.filter.push(c);
                self.refilter();
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.refilter();
            }
            _ => {}
        }
        None
    }

    fn move_down(&mut self, rows: usize) {
        let last = self.shown.len().saturating_sub(1);
        self.selected = (self.selected + rows).min(last);
    }

    /// Keeps the choices whose label or detail has every word of the filter.
    fn refilter(&mut self) {
        let words: Vec<String> = self
            .filter
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let choices = &self.choices.choices;
        self.shown = (0..choices.len())
            .filter(|&i| {
                let haystack = format!("{} {}", choices[i].label, choices[i].detail).to_lowercase();
                words.iter().all(|word| haystack.contains(word))
            })
            .collect();
        self.selected = 0;
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
        assert_eq!(picker.selected, 3);
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
        assert_eq!(picker.shown_len(), 0);
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
        let labels: Vec<_> = picker.shown().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["Claude Sonnet 5"]);
    }

    #[test]
    fn a_filter_given_up_front_narrows_like_typing() {
        let choices = vec![model("1", "one", "first"), model("2", "two", "second")];
        let pick = Choices::new("Numbers", choices, None).with_filter("SEC");
        let picker = Picker::sync(None, Some(&pick)).unwrap();
        assert_eq!((picker.filter.as_str(), picker.shown_len()), ("SEC", 1));
    }

    #[test]
    fn a_picker_follows_its_pick() {
        let pick = Choices::new("Numbers", vec![model("1", "one", "")], None);
        let mut shown = Picker::sync(None, Some(&pick)).unwrap();
        shown.filter = "on".into();

        // Revised, it keeps what was typed; another one starts afresh.
        let mut revised = Choices::new("Numbers", vec![model("2", "two", "")], None);
        revised.id = pick.id;
        let shown = Picker::sync(Some(shown), Some(&revised)).unwrap();
        assert_eq!((shown.filter.as_str(), shown.total()), ("on", 1));
        let other = Choices::new("Other", Vec::new(), None);
        let shown = Picker::sync(Some(shown), Some(&other)).unwrap();
        assert_eq!(shown.filter, "");
        assert!(Picker::sync(Some(shown), None).is_none());
    }
}
