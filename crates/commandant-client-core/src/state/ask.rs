//! A question the client puts to the person, which each front end asks as
//! it likes: something to choose, a line to enter, a yes to give, or
//! something to read.

use std::sync::atomic::{AtomicU64, Ordering};

use commandant_proto::{AgentSession, ProjectCopy};

use super::ChatId;

/// A question for the person, open in a scope until it is answered or
/// dismissed; a scope has at most one.
#[derive(Debug, Clone, PartialEq)]
pub enum Ask {
    /// One of several things.
    Choose(Choices),
    /// A line of text, which is never shown if it is `secret`.
    Enter {
        /// What to enter: "the code the page shows".
        label: String,
        /// Masked while typed, and kept out of everything shown.
        secret: bool,
        /// What the line is for.
        wanted: Wanted,
    },
    /// Whether to go ahead with `yes`.
    Confirm {
        /// The question: "Remove box-1?".
        text: String,
        /// What saying yes does.
        yes: Choose,
    },
    /// Something to read, or copy.
    Show {
        /// What it is: "Link".
        title: String,
        /// What to read, or copy.
        text: String,
    },
}

impl Ask {
    /// What there is to choose from, if that is the question.
    pub fn choices(&self) -> Option<&Choices> {
        match self {
            Self::Choose(choices) => Some(choices),
            _ => None,
        }
    }

    /// Whether this asks for a line, typed where a prompt would be.
    pub fn wants_line(&self) -> bool {
        matches!(self, Self::Enter { .. })
    }

    /// Whether this asks for a line that is never shown.
    pub fn secret(&self) -> bool {
        matches!(self, Self::Enter { secret: true, .. })
    }
}

/// What a line entered for an [`Ask::Enter`] is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wanted {
    /// The API key to sign in to a provider with.
    ApiKey {
        /// The provider's id, as the agent names it.
        provider: String,
    },
    /// The code a provider's page showed, to finish its OAuth method `index`.
    Code {
        /// The provider's id, as the agent names it.
        provider: String,
        /// Which of its methods, as the agent numbers them.
        index: u32,
    },
    /// A shell command to run on a node, in its worker's directory.
    Command {
        /// The node's id.
        node: String,
    },
    /// How long a new node's join token lasts.
    Lifetime,
}

/// What choosing one of the [`Choices`] means.
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
        /// The provider's id, as the agent names it.
        provider: String,
        /// Which of its OAuth methods; none for an API key.
        oauth: Option<u32>,
    },
    /// Sign out of a provider, by its id.
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
    /// Close a chat, stopping what it is at.
    CancelAndClose(ChatId),
    /// Show the tasks of a node, or of every node.
    TasksOf(Option<String>),
    /// Remove a node.
    RemoveNode(String),
    /// Leave, forgetting the server.
    LogOut,
    /// An agent harness to start on a node that hosts none.
    Harness {
        /// The node's id.
        node: String,
        /// The harness, as the node names it.
        harness: String,
    },
}

/// One line of an [`Ask::Choose`].
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    /// What choosing it means.
    pub value: Choose,
    /// What it is called, to filter by.
    pub label: String,
    /// Said after the label.
    pub detail: String,
}

impl Choice {
    /// `value`, shown as `label` then `detail`.
    pub fn new(value: Choose, label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            value,
            label: label.into(),
            detail: detail.into(),
        }
    }
}

/// What an [`Ask::Choose`] offers.
#[derive(Debug, Clone, PartialEq)]
pub struct Choices {
    /// Tells it from the questions before it.
    pub id: u64,
    /// Changes whenever its choices do, while it stays open.
    pub revision: u64,
    /// The question, which also tells which one it is.
    pub title: &'static str,
    /// What it offers, in order.
    pub choices: Vec<Choice>,
    /// The choice in effect, to start on.
    pub current: Option<Choose>,
    /// What to narrow the choices down by, to start with.
    pub filter: String,
    /// More choices are on their way.
    pub loading: bool,
}

impl Choices {
    /// A new question, unfiltered, starting on `current`.
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

    /// The same, narrowed down by `filter` to start with.
    pub fn with_filter(mut self, filter: &str) -> Self {
        self.filter = filter.to_string();
        self
    }

    /// The same question, open, with new choices.
    pub(super) fn revise(self, previous: &Self) -> Self {
        Self {
            id: previous.id,
            ..self
        }
    }
}

impl From<Choices> for Ask {
    fn from(choices: Choices) -> Self {
        Self::Choose(choices)
    }
}
