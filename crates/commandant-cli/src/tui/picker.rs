//! The floating window for choosing something from a list narrowed down by
//! typing. What choosing does is the value each choice carries.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Rows moved by PageUp / PageDown.
const PAGE: usize = 10;

pub struct Choice<T> {
    /// What choosing it means.
    pub value: T,
    pub label: String,
    /// Shown dimmed after the label.
    pub detail: String,
    /// The label and detail in lowercase, which the filter searches.
    haystack: String,
}

impl<T> Choice<T> {
    pub fn new(value: T, label: impl Into<String>, detail: impl Into<String>) -> Self {
        let (label, detail) = (label.into(), detail.into());
        Self {
            haystack: format!("{label} {detail}").to_lowercase(),
            value,
            label,
            detail,
        }
    }
}

/// What a key did to the picker.
pub enum Outcome<T> {
    Open,
    Closed,
    Chosen(T),
}

pub struct Picker<T> {
    pub title: &'static str,
    pub filter: String,
    choices: Vec<Choice<T>>,
    /// Indexes into `choices` that match the filter.
    shown: Vec<usize>,
    /// Position in `shown`.
    pub selected: usize,
}

impl<T: Clone + PartialEq> Picker<T> {
    /// Opens with `current`, if there, selected.
    pub fn new(title: &'static str, choices: Vec<Choice<T>>, current: Option<&T>) -> Self {
        let selected = current
            .and_then(|current| choices.iter().position(|c| &c.value == current))
            .unwrap_or(0);
        Self {
            title,
            filter: String::new(),
            shown: (0..choices.len()).collect(),
            choices,
            selected,
        }
    }

    /// Narrowed down to what matches `filter`, as if it had been typed.
    pub fn with_filter(mut self, filter: &str) -> Self {
        if !filter.is_empty() {
            self.filter = filter.to_string();
            self.refilter();
        }
        self
    }

    /// The choices that match the filter, in order.
    pub fn shown(&self) -> impl Iterator<Item = &Choice<T>> {
        self.shown.iter().map(|&i| &self.choices[i])
    }

    pub fn shown_len(&self) -> usize {
        self.shown.len()
    }

    pub fn total(&self) -> usize {
        self.choices.len()
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Outcome<T> {
        match key.code {
            KeyCode::Esc => return Outcome::Closed,
            KeyCode::Enter => {
                return match self.shown.get(self.selected) {
                    Some(&i) => Outcome::Chosen(self.choices[i].value.clone()),
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

    /// Keeps the choices in which every word of the filter appears.
    fn refilter(&mut self) {
        let words: Vec<String> = self
            .filter
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        self.shown = (0..self.choices.len())
            .filter(|&i| {
                let haystack = &self.choices[i].haystack;
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

    #[test]
    fn filters_by_every_word_and_picks_the_selection() {
        let choices = vec![
            Choice::new("", "Default", ""),
            Choice::new("anthropic/claude-sonnet-5", "Claude Sonnet 5", "Anthropic"),
            Choice::new(
                "anthropic/claude-haiku-4-5",
                "Claude Haiku 4.5",
                "Anthropic",
            ),
            Choice::new("openai/gpt-6", "GPT-6", "OpenAI"),
        ];
        let mut picker = Picker::new("Model", choices, Some(&"openai/gpt-6"));
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
            Outcome::Chosen(value) if value == "anthropic/claude-haiku-4-5"
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
        let choices = vec![
            Choice::new(1, "one", "first"),
            Choice::new(2, "two", "second"),
        ];
        let picker = Picker::new("Numbers", choices, Some(&2)).with_filter("SEC");
        assert_eq!((picker.filter.as_str(), picker.shown_len()), ("SEC", 1));
        let unfiltered = Picker::new("Numbers", Vec::<Choice<u8>>::new(), None).with_filter("");
        assert_eq!(unfiltered.selected, 0);
    }
}
