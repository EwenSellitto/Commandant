//! Something to choose from, which the front end offers as it likes.

use std::sync::atomic::{AtomicU64, Ordering};

use commandant_proto::{AgentSession, ProjectCopy};

use super::ChatId;

/// What choosing from a picker means.
#[derive(Debug, Clone, PartialEq)]
pub enum Choose {
    /// The agent a chat's prompts go to; empty for the default.
    Agent(String),
    /// The model a chat's prompts go to; empty for the default.
    Model(String),
    /// The thinking effort; empty for the model's default.
    Effort(String),
    /// One of the agent's commands, to fill in with its arguments.
    Command(String),
    /// An MCP server, to connect or disconnect.
    Mcp(String),
    /// A model provider, to sign in to or out of.
    Provider(String),
    /// Sign in to a provider: with an API key, or its OAuth method `oauth`.
    SignIn {
        provider: String,
        oauth: Option<u32>,
    },
    SignOut(String),
    /// Clone a new copy of a project (a repository, or its name) to work in.
    NewCopy(String),
    /// Work in an existing copy of a project, alongside its other sessions.
    Join(ProjectCopy),
    /// Another chat on the node, with a new session.
    NewSession,
    /// One of the node's open chats.
    Chat(ChatId),
    /// A session the node saved, to resume in a new chat.
    Saved(AgentSession),
    /// An agent harness to start on a node that hosts none.
    Harness {
        node: String,
        harness: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub value: Choose,
    pub label: String,
    /// Said after the label.
    pub detail: String,
}

impl Choice {
    pub fn new(value: Choose, label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            value,
            label: label.into(),
            detail: detail.into(),
        }
    }
}

/// Something to choose from, for the front end to offer.
#[derive(Debug, Clone, PartialEq)]
pub struct Pick {
    /// Tells it from the pickers before it.
    pub id: u64,
    /// Changes whenever its choices do, while it stays open.
    pub revision: u64,
    pub title: &'static str,
    pub choices: Vec<Choice>,
    /// The choice in effect, to start on.
    pub current: Option<Choose>,
    /// What to narrow the choices down by, to start with.
    pub filter: String,
    /// More choices are on their way.
    pub loading: bool,
}

impl Pick {
    pub fn new(title: &'static str, choices: Vec<Choice>, current: Option<Choose>) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        Self {
            id,
            revision: id,
            title,
            choices,
            current,
            filter: String::new(),
            loading: false,
        }
    }

    pub fn with_filter(mut self, filter: &str) -> Self {
        self.filter = filter.to_string();
        self
    }

    /// The same picker, open, with new choices.
    pub(super) fn revise(self, previous: &Pick) -> Self {
        Self {
            id: previous.id,
            ..self
        }
    }
}
