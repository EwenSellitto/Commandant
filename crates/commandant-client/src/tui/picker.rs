//! The floating window for answering an [`Ask::Choose`] narrowed down by
//! typing.
//!
//! [`Ask::Choose`]: crate::state::Ask::Choose

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::state::{Choice, Choices, Choose};

/// Rows moved by PageUp / PageDown.
const PAGE: usize = 10;

/// What a key did to the picker.
pub enum Outcome {
    Open,
    Closed,
    Chosen(Choose),
}

pub struct Picker {
    /// What it offers, as the state last had it.
    pick: Choices,
    pub filter: String,
    /// Indexes into the choices that match the filter.
    shown: Vec<usize>,
    /// Position in `shown`.
    pub selected: usize,
}

impl Picker {
    /// Shows `pick` narrowed down by `filter`, as if it had been typed;
    /// unfiltered, on the current choice.
    fn new(pick: &Choices, filter: &str) -> Self {
        let choices = &pick.choices;
        let current = pick.current.as_ref();
        let selected = current
            .and_then(|current| choices.iter().position(|c| &c.value == current))
            .unwrap_or(0);
        let mut picker = Self {
            shown: (0..choices.len()).collect(),
            pick: pick.clone(),
            filter: String::new(),
            selected,
        };
        if !filter.is_empty() {
            picker.filter = filter.to_string();
            picker.refilter();
        }
        picker
    }

    /// Makes the picker in `slot` show `pick`: a new one as it opens, the
    /// same one, its filter kept, as its choices change.
    pub fn sync(slot: &mut Option<Self>, pick: Option<&Choices>) {
        let Some(pick) = pick else {
            *slot = None;
            return;
        };
        match slot {
            Some(shown) if shown.pick.revision == pick.revision => {}
            Some(shown) if shown.pick.id == pick.id => *slot = Some(Self::new(pick, &shown.filter)),
            _ => *slot = Some(Self::new(pick, &pick.filter)),
        }
    }

    pub fn title(&self) -> &'static str {
        self.pick.title
    }

    /// More choices are on their way.
    pub fn loading(&self) -> bool {
        self.pick.loading
    }

    /// The choices that match the filter, in order.
    pub fn shown(&self) -> impl Iterator<Item = &Choice> {
        self.shown.iter().map(|&i| &self.pick.choices[i])
    }

    pub fn shown_len(&self) -> usize {
        self.shown.len()
    }

    pub fn total(&self) -> usize {
        self.pick.choices.len()
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Esc => return Outcome::Closed,
            KeyCode::Enter => {
                return match self.shown().nth(self.selected) {
                    Some(choice) => Outcome::Chosen(choice.value.clone()),
                    None => Outcome::Open,
                };
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
        Outcome::Open
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
        let choices = &self.pick.choices;
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

        for c in "anth son".chars() {
            picker.on_key(key(KeyCode::Char(c)));
        }
        let labels: Vec<_> = picker.shown().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["Claude Sonnet 5"]);

        picker.on_key(key(KeyCode::Backspace));
        picker.on_key(key(KeyCode::Backspace));
        picker.on_key(key(KeyCode::Backspace));
        picker.on_key(key(KeyCode::Down));
        picker.on_key(key(KeyCode::Down));
        assert!(matches!(
            picker.on_key(key(KeyCode::Enter)),
            Outcome::Chosen(Choose::Model(id)) if id == "anthropic/claude-haiku-4-5"
        ));

        for c in "zzz".chars() {
            picker.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(picker.shown_len(), 0);
        assert!(matches!(picker.on_key(key(KeyCode::Enter)), Outcome::Open));
        assert!(matches!(picker.on_key(key(KeyCode::Esc)), Outcome::Closed));
    }

    #[test]
    fn a_filter_given_up_front_narrows_like_typing() {
        let choices = vec![model("1", "one", "first"), model("2", "two", "second")];
        let pick = Choices::new("Numbers", choices, None).with_filter("SEC");
        let mut slot = None;
        Picker::sync(&mut slot, Some(&pick));
        let picker = slot.as_ref().unwrap();
        assert_eq!((picker.filter.as_str(), picker.shown_len()), ("SEC", 1));
    }

    #[test]
    fn a_picker_follows_its_pick() {
        let pick = Choices::new("Numbers", vec![model("1", "one", "")], None);
        let mut slot = None;
        Picker::sync(&mut slot, Some(&pick));
        slot.as_mut().unwrap().filter = "on".into();

        // Revised, it keeps what was typed; another one starts afresh.
        let mut revised = Choices::new("Numbers", vec![model("2", "two", "")], None);
        revised.id = pick.id;
        Picker::sync(&mut slot, Some(&revised));
        assert_eq!(slot.as_ref().unwrap().filter, "on");
        assert_eq!(slot.as_ref().unwrap().total(), 1);
        Picker::sync(&mut slot, Some(&Choices::new("Other", Vec::new(), None)));
        assert_eq!(slot.as_ref().unwrap().filter, "");
        Picker::sync(&mut slot, None);
        assert!(slot.is_none());
    }
}
