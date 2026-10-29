//! The line language: what a submitted line asks for (a prompt, a
//! `/command`, or a shell command after `!`), and the commands that complete
//! what is being typed.

use commandant_common::or;

use super::{Chat, Menu};
use crate::state::{Edit, Effect, Outcome};

/// What a submitted line asks of the app rather than the chat.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(in crate::state) enum AppCommand {
    Quit,
    /// Leave, forgetting the server.
    LogOut,
    /// Show the server's link.
    Link,
    New,
    Sessions,
    Nodes,
    Tasks,
    Close,
    Help,
}

/// The chat's own commands, for completing and for a front end's help.
pub const COMMANDS: [(&str, &str); 16] = [
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
    (
        "tasks",
        "everything the nodes have run, to read, follow or cancel",
    ),
    ("link", "the server's link, to connect another client"),
    ("logout", "forget this server, to paste another's link"),
    ("quit", "leave"),
];
/// Other names for some of the [`COMMANDS`]: each alias, and the command it
/// stands for.
pub const ALIASES: [(&str, &str); 3] = [("commands", "skills"), ("exit", "quit"), ("?", "help")];

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
        if let Some(line) = text.strip_prefix('!') {
            return (self.run(line.trim()), None);
        }
        if let Some(command) = text.strip_prefix('/') {
            let (command, filter) = command.split_once(' ').unwrap_or((command, ""));
            let app = match command {
                "quit" | "exit" => return (Outcome::default(), Some(AppCommand::Quit)),
                "logout" => return (Outcome::default(), Some(AppCommand::LogOut)),
                "link" => Some(AppCommand::Link),
                "new" => Some(AppCommand::New),
                "sessions" => Some(AppCommand::Sessions),
                "nodes" => Some(AppCommand::Nodes),
                "tasks" => Some(AppCommand::Tasks),
                "close" => Some(AppCommand::Close),
                "help" | "?" => Some(AppCommand::Help),
                _ => None,
            };
            if app.is_some() {
                return (self.cleared(None), app);
            }
            let menu = match command {
                "agent" => Menu::Agent,
                "model" => Menu::Model,
                "effort" => Menu::Effort,
                "commands" | "skills" => Menu::Command,
                "mcp" => Menu::Mcp,
                "providers" => {
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
                    return (self.send(&text, name, arguments), None);
                }
                // Unless the agent has its own, logging in is the client's,
                // and done: no way to sign in.
                "login" => {
                    self.info(
                        "this client is logged in to its server: /logout forgets it; \
                         /providers signs the agent in to a model provider",
                    );
                    return (self.cleared(None), None);
                }
                // It may be one of those, once they've loaded.
                _ if self.loading() => {
                    self.info("the agent's commands are still loading; send it again in a moment");
                    return Default::default();
                }
                // Not a command: a prompt that starts with a slash.
                _ => return (self.send(&text, String::new(), text.clone()), None),
            };
            let effect = self.open_menu(menu, filter.trim());
            return (self.cleared(effect), None);
        }
        (self.send(&text, String::new(), text.clone()), None)
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
        let aliases = ALIASES
            .iter()
            .map(|(a, n)| (a.to_string(), format!("same as /{n}")));
        let agent = self
            .agent_commands()
            .map(|(n, d)| (n.to_string(), d.to_string()));
        own.chain(aliases)
            .chain(agent)
            .filter(|(n, _)| n.starts_with(typed))
            .collect()
    }

    /// `text` with its completion `at` filled in, ready for its arguments.
    pub fn completed(&self, text: &str, at: usize) -> Option<String> {
        let (name, _) = self.completions(text).into_iter().nth(at)?;
        Some(format!("/{name} "))
    }

    /// What Enter sends of `text`, its completion `at` highlighted: a partly
    /// typed command runs that one.
    pub fn sent_for(&self, text: &str, at: usize) -> String {
        match self.completed(text, at) {
            Some(command) if text.len() > 1 && !self.is_command(&text[1..]) => {
                command.trim_end().to_string()
            }
            _ => text.to_string(),
        }
    }

    /// Whether `name` is one of the chat's commands or the agent's.
    pub fn is_command(&self, name: &str) -> bool {
        let own = COMMANDS.iter().chain(&ALIASES).any(|(n, _)| *n == name);
        own || self.is_agent_command(name)
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

    /// The agent's own commands and skills, with what they do; none until
    /// its options have loaded.
    pub fn agent_commands(&self) -> impl Iterator<Item = (&str, &str)> {
        let commands = self.options.iter().flat_map(|o| &o.commands);
        commands.map(|c| (c.name.as_str(), or(&c.description, &c.source)))
    }

    fn is_agent_command(&self, name: &str) -> bool {
        self.agent_commands().any(|(n, _)| n == name)
    }
}
