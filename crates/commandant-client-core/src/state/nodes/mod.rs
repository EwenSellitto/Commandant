//! Managing the nodes: adding one with a join token, removing one, and
//! starting an agent on one that hosts none.

use std::time::Duration;

use commandant_common::time;

use super::{Ask, Choose, Effect, Outcome, State, Wanted};

#[cfg(test)]
mod tests;

/// How long a new node's token lasts unless told.
const LIFETIME: Duration = Duration::from_secs(60 * 60);

impl State {
    /// Asks how long the new node's token lasts.
    pub(super) fn add_node(&mut self) -> Outcome {
        self.notice.clear();
        self.ask = Some(Ask::Enter {
            label: format!(
                "how long the new node's token lasts: 30m, 1h, 2d… ({} if empty)",
                time::duration(LIFETIME)
            ),
            secret: false,
            wanted: Wanted::Lifetime,
        });
        Outcome::default()
    }

    /// Makes a token lasting `typed`, or the default if empty; asks again
    /// for a lifetime it can't read.
    pub(super) fn create_token(&mut self, typed: &str) -> Outcome {
        let ttl = match typed.trim() {
            "" => Ok(LIFETIME),
            typed => time::parse_duration(typed),
        };
        match ttl {
            Ok(ttl) if !ttl.is_zero() => {
                self.notice.clear();
                Effect::CreateToken { ttl }.into()
            }
            // Tokens expire on their own: none lasts for ever.
            Ok(_) => self.add_again("a token has to expire: give it a lifetime, like 1h"),
            Err(e) => self.add_again(&e),
        }
    }

    fn add_again(&mut self, why: &str) -> Outcome {
        self.add_node();
        self.notice = why.to_string();
        Outcome::default()
    }

    /// Asks whether to remove a node.
    pub(super) fn ask_to_remove(&mut self, node: &str) -> Outcome {
        let Some(name) = self.node(node).map(|n| n.name.clone()) else {
            return Outcome::default();
        };
        let mut text = format!("Remove {name}? It can only join again with a new token.");
        match self.chats_on(node).count() {
            0 => {}
            1 => text.push_str(" Its open chat closes."),
            open => text.push_str(&format!(" Its {open} open chats close.")),
        }
        self.ask = Some(Ask::Confirm {
            text,
            yes: Choose::RemoveNode(node.to_string()),
        });
        Outcome::default()
    }

    /// Has the server forget a node.
    pub(super) fn remove_node(&mut self, node: String) -> Outcome {
        self.notice = format!("removing {}…", self.node_name(&node));
        Effect::RemoveNode(node).into()
    }

    /// Drops a node the server forgot, and its chats.
    pub(super) fn on_removed(&mut self, node: &str, removed: Result<(), String>) -> Outcome {
        let name = self.node_name(node);
        if let Err(e) = removed {
            self.notice = format!("couldn't remove {name}: {e}");
            return Outcome::default();
        }
        self.nodes.retain(|n| n.id != node);
        self.chats.retain(|c| c.node.id != node);
        self.saved.remove(node);
        self.options.remove(node);
        self.starting.remove(node);
        self.notice = format!("removed {name}");
        Outcome::default()
    }

    /// Offers the agents a node hosting none can start.
    pub(super) fn start_agent(&mut self, node: &str) -> Outcome {
        let Some(node) = self.node(node).cloned() else {
            return Outcome::default();
        };
        self.notice.clear();
        if !node.online {
            self.notice = format!("{} is offline", node.name);
        } else if !node.harnesses.is_empty() {
            self.notice = format!("{} already hosts {}", node.name, node.harnesses.join(", "));
        } else {
            self.choose_harness(node);
        }
        Outcome::default()
    }

    /// Shows the command that enrols a node with the token made.
    pub(super) fn on_token(&mut self, ttl: Duration, link: Result<String, String>) -> Outcome {
        match link {
            Ok(link) => {
                self.ask = Some(Ask::Show {
                    title: format!(
                        "Run this on the new node, within {}; it works once",
                        time::duration(ttl)
                    ),
                    text: format!("commandant-server worker {link}"),
                });
            }
            Err(e) => self.notice = format!("couldn't make a token: {e}"),
        }
        Outcome::default()
    }
}
