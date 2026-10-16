//! The help: every command and key, narrowed down by typing. Choosing one
//! does what it does.

use commandant_client_core::chat::{ALIASES, COMMANDS, Chat};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::picker::{Filtered, Row};

/// A key, as written in the [`KEYS`].
type Key = (KeyCode, KeyModifiers);

const fn key(code: KeyCode) -> Option<Key> {
    Some((code, KeyModifiers::NONE))
}

const fn ctrl(c: char) -> Option<Key> {
    Some((KeyCode::Char(c), KeyModifiers::CONTROL))
}

/// A chat's keys that do what no command does, with what they do, and the
/// key choosing one in the help presses; none for a pair.
pub const KEYS: [(&str, &str, Option<Key>); 7] = [
    (
        "Tab",
        "switch agent, or complete a /command",
        key(KeyCode::Tab),
    ),
    ("↑ ↓", "earlier prompts, or move in the /command list", None),
    ("Ctrl-T", "next thinking effort", ctrl('t')),
    ("Esc", "stop a command, or cancel a turn", key(KeyCode::Esc)),
    ("PgUp PgDn", "scroll", None),
    ("Alt-← Alt-→", "previous / next session", None),
    ("Ctrl-U", "clear the prompt", ctrl('u')),
];

/// What a line starting with `!` does.
pub const BANG: (&str, &str) = (
    "!<command>",
    "run a shell command on the node, in the chat's directory",
);

/// The keys that do what a command does.
const COMMAND_KEYS: [(&str, &str); 7] = [
    ("help", "F1"),
    ("new", "Ctrl-N"),
    ("sessions", "Ctrl-O"),
    ("close", "Ctrl-W"),
    ("nodes", "Ctrl-G"),
    ("tasks", "Ctrl-H"),
    ("quit", "Ctrl-C"),
];

/// The node list's keys.
const NODE_KEYS: [(&str, &str, Option<Key>); 9] = [
    ("↑ ↓  j k", "move", None),
    ("Enter", "open the node's sessions", key(KeyCode::Enter)),
    (
        "r",
        "run a shell command on the node, in its worker's directory",
        key(KeyCode::Char('r')),
    ),
    (
        "a",
        "add a node: the command that enrols a machine",
        key(KeyCode::Char('a')),
    ),
    ("d", "remove the node", key(KeyCode::Char('d'))),
    (
        "s",
        "start an agent on a node that hosts none",
        key(KeyCode::Char('s')),
    ),
    ("Ctrl-H", "everything the nodes have run", ctrl('h')),
    ("F1  ?", "what every key does", None),
    ("q  Ctrl-C", "leave", key(KeyCode::Char('q'))),
];

/// The task list's keys.
const TASK_KEYS: [(&str, &str, Option<Key>); 6] = [
    ("↑ ↓  j k", "move", None),
    (
        "Enter",
        "read a command's output, or open a prompt's session",
        key(KeyCode::Enter),
    ),
    ("f", "only one node's tasks", key(KeyCode::Char('f'))),
    ("x", "cancel a running task", key(KeyCode::Char('x'))),
    ("Ctrl-G", "the nodes", ctrl('g')),
    ("Esc  q", "back", key(KeyCode::Esc)),
];

/// An output's keys.
const OUTPUT_KEYS: [(&str, &str, Option<Key>); 4] = [
    ("↑ ↓  PgUp PgDn", "scroll", None),
    ("x", "stop the command", key(KeyCode::Char('x'))),
    ("Ctrl-H", "the tasks", ctrl('h')),
    ("Esc  q", "back", key(KeyCode::Esc)),
];

/// Labels are padded to line up the rows, up to this width.
const LABEL_WIDTH: usize = 14;

/// What choosing a row of the help does.
#[derive(Debug, Clone, PartialEq)]
pub enum Does {
    /// Sends a line, as if typed: a `/command`.
    Run(String),
    /// Starts a line in the prompt, for what follows it.
    Start(String),
    Press(KeyEvent),
    /// Nothing but close the help.
    Nothing,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    label: String,
    about: String,
    pub does: Does,
}

impl Row for Entry {
    fn label(&self) -> &str {
        &self.label
    }

    fn detail(&self) -> &str {
        &self.about
    }
}

pub struct Help {
    pub list: Filtered<Entry>,
}

impl Help {
    /// Everything there is to do in `chat`: its commands, running one on
    /// the node, the agent's commands, then the keys.
    pub fn chat(chat: &Chat) -> Self {
        let own = COMMANDS.iter().map(|&(name, about)| {
            let key = COMMAND_KEYS.iter().find(|(n, _)| *n == name);
            let aliases: Vec<String> = ALIASES
                .iter()
                .filter(|(_, n)| *n == name)
                .map(|(alias, _)| format!("/{alias}"))
                .collect();
            let mut about = vec![about.to_string()];
            about.extend(key.map(|(_, key)| key.to_string()));
            if !aliases.is_empty() {
                about.push(format!("also {}", aliases.join(" ")));
            }
            let line = format!("/{name}");
            (line.clone(), about.join(" · "), Does::Run(line))
        });
        let agent = chat.agent_commands().map(|(name, about)| {
            let line = format!("/{name}");
            (
                line.clone(),
                about.to_string(),
                Does::Start(format!("{line} ")),
            )
        });
        let (label, about) = BANG;
        let bang = (
            label.to_string(),
            about.to_string(),
            Does::Start("!".into()),
        );
        let rows: Vec<_> = own
            .chain(std::iter::once(bang))
            .chain(agent)
            .chain(keys(&KEYS))
            .collect();
        Self::new(rows)
    }

    /// What every key of the node list does.
    pub fn nodes() -> Self {
        Self::new(keys(&NODE_KEYS).collect())
    }

    /// What every key of the task list does.
    pub fn tasks() -> Self {
        Self::new(keys(&TASK_KEYS).collect())
    }

    /// What every key of an output does.
    pub fn output() -> Self {
        Self::new(keys(&OUTPUT_KEYS).collect())
    }

    fn new(rows: Vec<(String, String, Does)>) -> Self {
        let width = rows.iter().map(|(l, ..)| l.chars().count()).max();
        let width = width.unwrap_or(0).min(LABEL_WIDTH);
        let entries = rows
            .into_iter()
            .map(|(label, about, does)| Entry {
                label: format!("{label:<width$}"),
                about,
                does,
            })
            .collect();
        Self {
            list: Filtered::new(entries, "", 0),
        }
    }
}

fn keys(keys: &[(&str, &str, Option<Key>)]) -> impl Iterator<Item = (String, String, Does)> {
    keys.iter().map(|&(label, about, key)| {
        let does = key.map_or(Does::Nothing, |(code, mods)| {
            Does::Press(KeyEvent::new(code, mods))
        });
        (label.to_string(), about.to_string(), does)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::picker::Picked;

    fn search(help: &mut Help, text: &str) -> Vec<String> {
        for c in text.chars() {
            help.list.on_key(KeyEvent::from(KeyCode::Char(c)));
        }
        let labels = help.list.shown().map(|e| e.label.trim().to_string());
        labels.collect()
    }

    fn chat() -> Chat {
        let node = commandant_proto::NodeInfo::default();
        Chat::new(1, node, Default::default(), None)
    }

    #[test]
    fn commands_and_keys_are_found_by_what_they_do() {
        let mut help = Help::chat(&chat());
        assert_eq!(
            search(&mut help, "session"),
            ["/new", "/sessions", "/close", "Alt-← Alt-→"]
        );
        let mut help = Help::chat(&chat());
        assert_eq!(search(&mut help, "ctrl-n"), ["/new"], "by its key");
        let mut help = Help::chat(&chat());
        assert_eq!(search(&mut help, "login"), ["/providers"], "by an alias");
    }

    #[test]
    fn choosing_one_does_it() {
        let mut help = Help::chat(&chat());
        let mut choose = |text: &str| {
            search(&mut help, text);
            let picked = help.list.on_key(KeyEvent::from(KeyCode::Enter));
            help.list.filter.clear();
            match picked {
                Some(Picked::Chosen(entry)) => entry.does,
                _ => panic!("nothing chosen for {text:?}"),
            }
        };
        assert_eq!(choose("model"), Does::Run("/model".into()));
        let effort = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL);
        assert_eq!(choose("thinking effort"), Does::Run("/effort".into()));
        assert_eq!(choose("next thinking"), Does::Press(effort));
        assert_eq!(choose("scroll"), Does::Nothing);
        assert_eq!(choose("shell"), Does::Start("!".into()));
    }
}
