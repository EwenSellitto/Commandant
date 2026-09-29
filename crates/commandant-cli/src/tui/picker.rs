//! The floating window for choosing an agent, a model or an effort: a list
//! narrowed down by typing.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Rows moved by PageUp / PageDown.
const PAGE: usize = 10;

/// What is being chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Agent,
    Model,
    Effort,
    /// One of the agent's commands or skills, to fill in.
    Command,
    /// An MCP server, to connect or disconnect.
    Mcp,
}

impl Pick {
    pub fn title(self) -> &'static str {
        match self {
            Self::Agent => "Agent",
            Self::Model => "Model",
            Self::Effort => "Thinking effort",
            Self::Command => "Commands and skills",
            Self::Mcp => "MCP servers (Enter connects or disconnects)",
        }
    }
}

pub struct Choice {
    /// What the setting becomes; empty means the default.
    pub value: String,
    pub label: String,
    /// Shown dimmed after the label.
    pub detail: String,
}

impl Choice {
    pub fn new(
        value: impl Into<String>,
        label: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            detail: detail.into(),
        }
    }

    /// Whether every word of the filter appears somewhere in the choice.
    fn matches(&self, words: &[String]) -> bool {
        let text = format!("{} {} {}", self.value, self.label, self.detail).to_lowercase();
        words.iter().all(|word| text.contains(word))
    }
}

/// What a key did to the picker.
pub enum Outcome {
    Open,
    Closed,
    Chosen(String),
}

pub struct Picker {
    pub pick: Pick,
    pub filter: String,
    choices: Vec<Choice>,
    /// Indexes into `choices` that match the filter.
    shown: Vec<usize>,
    /// Position in `shown`.
    pub selected: usize,
}

impl Picker {
    /// Opens with `current` selected.
    pub fn new(pick: Pick, choices: Vec<Choice>, current: &str) -> Self {
        let selected = choices.iter().position(|c| c.value == current).unwrap_or(0);
        Self {
            pick,
            filter: String::new(),
            shown: (0..choices.len()).collect(),
            choices,
            selected,
        }
    }

    /// The choices that match the filter, in order.
    pub fn shown(&self) -> impl Iterator<Item = &Choice> {
        self.shown.iter().map(|&i| &self.choices[i])
    }

    pub fn shown_len(&self) -> usize {
        self.shown.len()
    }

    pub fn total(&self) -> usize {
        self.choices.len()
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
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

    fn refilter(&mut self) {
        let words: Vec<String> = self
            .filter
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        self.shown = (0..self.choices.len())
            .filter(|&i| self.choices[i].matches(&words))
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
        let mut picker = Picker::new(Pick::Model, choices, "openai/gpt-6");
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
}
