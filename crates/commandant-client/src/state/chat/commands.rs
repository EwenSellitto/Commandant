//! The line language: what a submitted line asks for, and the commands
//! that complete what is being typed.

use commandant_common::or;

use super::{Chat, Menu};
use crate::state::{Edit, Effect, Outcome};

/// What a submitted line asks of the app rather than the chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state) enum AppCommand {
    Quit,
    New,
    Sessions,
    Nodes,
    Close,
}

/// The chat's own commands, for completing and `/help`.
pub const COMMANDS: [(&str, &str); 18] = [
    ("help", "what every command and key does"),
    ("agent", "choose an agent"),
    ("model", "choose a model"),
    ("effort", "choose a thinking effort"),
    ("skills", "the agent's commands and skills"),
    ("mcp", "connect or disconnect MCP servers"),
    ("providers", "sign in to or out of a model provider"),
    (
        "project",
        "browse the node's projects, or clone one: /project <repository>",
    ),
    ("new", "another session, working alongside"),
    ("sessions", "switch, or resume a saved one"),
    ("close", "close this session"),
    ("nodes", "the other nodes"),
    ("commands", "same as /skills"),
    ("login", "same as /providers"),
    ("quit", "leave"),
    ("exit", "same as /quit"),
    ("logout", "same as /providers"),
    ("?", "same as /help"),
];
/// The terminal's keys, for `/help` and an empty chat.
pub const KEYS: [(&str, &str); 11] = [
    ("Tab", "switch agent, or complete a /command"),
    ("↑ ↓", "earlier prompts, or move in the /command list"),
    ("Ctrl-T", "next thinking effort"),
    ("Esc", "cancel a turn"),
    ("PgUp PgDn", "scroll"),
    ("Ctrl-N", "new session"),
    ("Ctrl-O", "sessions"),
    ("Alt-← Alt-→", "previous / next session"),
    ("Ctrl-W", "close this session"),
    ("Ctrl-G", "the other nodes"),
    ("Ctrl-U", "clear the prompt"),
];

impl Chat {
    /// Takes a line: a prompt or a `/command`. A line that is the app's to
    /// carry out comes back as its command.
    pub(in crate::state) fn submit(&mut self, text: &str) -> (Outcome, Option<AppCommand>) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Default::default();
        }
        // A line asked for, a key say, is never a prompt.
        if self.entering() {
            let entered = self.enter(&text);
            return (self.cleared(None).and(entered), None);
        }
        if self.sent.last() != Some(&text) {
            self.sent.push(text.clone());
        }
        if let Some(command) = text.strip_prefix('/') {
            let (command, filter) = command.split_once(' ').unwrap_or((command, ""));
            let app = match command {
                "quit" | "exit" => return (Outcome::default(), Some(AppCommand::Quit)),
                "new" => Some(AppCommand::New),
                "sessions" => Some(AppCommand::Sessions),
                "nodes" => Some(AppCommand::Nodes),
                "close" => Some(AppCommand::Close),
                _ => None,
            };
            if app.is_some() {
                return (self.cleared(None), app);
            }
            let menu = match command {
                "help" | "?" => {
                    self.help();
                    return (self.cleared(None), None);
                }
                "agent" => Menu::Agent,
                "model" => Menu::Model,
                "effort" => Menu::Effort,
                "commands" | "skills" => Menu::Command,
                "mcp" => Menu::Mcp,
                "providers" | "login" | "logout" => {
                    let effect = self.list_providers(filter.trim());
                    return (self.cleared(effect), None);
                }
                "project" => {
                    let effect = self.project(filter.trim());
                    return (self.cleared(effect), None);
                }
                // One of the agent's own commands or skills, its arguments after it.
                name if self.is_agent_command(name) => {
                    let (name, arguments) = (name.to_string(), filter.trim().to_string());
                    return (self.send(text, name, arguments), None);
                }
                // It may be one of those, once they've loaded.
                _ if self.loading() => {
                    self.info("the agent's commands are still loading; send it again in a moment");
                    return Default::default();
                }
                // Not a command: a prompt that starts with a slash.
                _ => return (self.send(text.clone(), String::new(), text), None),
            };
            let effect = self.open_menu(menu, filter.trim());
            return (self.cleared(effect), None);
        }
        (self.send(text.clone(), String::new(), text), None)
    }

    /// What `effect` asks, the line it came from taken out of the prompt.
    pub(super) fn cleared(&self, effect: Option<Effect>) -> Outcome {
        Outcome {
            prompt: Some((self.id, Edit::Clear)),
            ..effect.into()
        }
    }

    /// The commands, the chat's own then the agent's, that complete the
    /// `/name` being typed in `text` (before its arguments), with what they do.
    pub fn completions(&self, text: &str) -> Vec<(String, String)> {
        let Some(typed) = text.strip_prefix('/') else {
            return Vec::new();
        };
        if typed.contains(' ') || self.entering() || self.signing_in.is_some() {
            return Vec::new();
        }
        let own = COMMANDS.iter().map(|(n, d)| (n.to_string(), d.to_string()));
        let agent = self
            .options
            .iter()
            .flat_map(|o| &o.commands)
            .map(|c| (c.name.clone(), or(&c.description, &c.source).to_string()));
        own.chain(agent)
            .filter(|(n, _)| n.starts_with(typed))
            .collect()
    }

    /// Whether `name` is one of the chat's commands or the agent's.
    pub fn is_command(&self, name: &str) -> bool {
        COMMANDS.iter().any(|(n, _)| *n == name) || self.is_agent_command(name)
    }

    /// How many characters of `text` name a known `/command`, before its
    /// arguments; 0 when it names none.
    pub fn command_len(&self, text: &str) -> usize {
        let Some(typed) = text.strip_prefix('/') else {
            return 0;
        };
        let name = typed.split(' ').next().unwrap_or_default();
        match self.is_command(name) {
            true => 1 + name.chars().count(),
            false => 0,
        }
    }

    /// Every command and key, in the thread.
    fn help(&mut self) {
        let mut lines = vec!["commands".to_string()];
        lines.extend(COMMANDS.iter().map(|(n, d)| format!("  /{n:<12}{d}")));
        if let Some(options) = self.options.clone().filter(|o| !o.commands.is_empty()) {
            lines.push("the agent's commands and skills".into());
            let agent = options.commands.iter();
            lines.extend(
                agent.map(|c| format!("  /{:<12}{}", c.name, or(&c.description, &c.source))),
            );
        }
        lines.push("keys".into());
        lines.extend(KEYS.iter().map(|(k, d)| format!("  {k:<13}{d}")));
        self.info(&lines.join("\n"));
    }

    fn is_agent_command(&self, name: &str) -> bool {
        self.options
            .as_ref()
            .is_some_and(|o| o.commands.iter().any(|c| c.name == name))
    }
}
