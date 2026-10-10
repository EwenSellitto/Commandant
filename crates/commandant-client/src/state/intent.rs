//! What goes into the state and what comes out: intents and updates in,
//! outcomes out.

use std::time::Duration;

use commandant_proto::*;

use super::chat::Message;
use super::{ChatId, Choose};

/// What the person asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    /// Show one of a node's chats: `chat`, the one the front end last
    /// showed, if it is still open, else its newest, else a new one.
    Open { node: String, chat: Option<ChatId> },
    /// Another chat on the same node, with the same settings.
    NewChat(ChatId),
    /// Choose one of the chat's node's chats or saved sessions.
    Sessions(ChatId),
    /// Close a chat, unless its agent is still at work.
    Close(ChatId),
    /// A line typed in a chat: a prompt or a `/command`.
    Submit(ChatId, String),
    /// Answer the scope's [`Ask::Choose`](super::Ask::Choose).
    Choose(Scope, Choose),
    /// Answer the scope's [`Ask::Enter`](super::Ask::Enter) with a line.
    Enter(Scope, String),
    /// Say yes to the scope's [`Ask::Confirm`](super::Ask::Confirm).
    Confirm(Scope),
    /// Leave the scope's question unanswered.
    Dismiss(Scope),
    /// Cancel the chat's turn.
    Cancel(ChatId),
    /// The next (or previous) agent.
    CycleAgent(ChatId, isize),
    /// The next thinking effort.
    CycleEffort(ChatId),
    /// The chat has been looked at, so how its last turn ended is no news.
    Seen(ChatId),
}

/// Where a question, or an intent about one, belongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    App,
    Chat(ChatId),
}

/// Where the front end may go next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Go {
    Chat(ChatId),
    Nodes,
    Quit,
    /// The node hosts an agent now: worth opening, if it is the one in view.
    Ready(String),
}

/// What a chat's prompt becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// The line was taken.
    Clear,
    /// Typed at the cursor.
    Insert(String),
}

/// What an intent or an update comes to, beyond what the state keeps.
#[derive(Default)]
pub struct Outcome {
    pub effects: Vec<Effect>,
    pub go: Option<Go>,
    pub prompt: Option<(ChatId, Edit)>,
}

impl Outcome {
    pub(super) fn go(go: Go) -> Self {
        Self {
            go: Some(go),
            ..Default::default()
        }
    }

    /// This, then `next`.
    pub(super) fn and(mut self, next: Outcome) -> Self {
        self.effects.extend(next.effects);
        self.go = next.go.or(self.go);
        self.prompt = next.prompt.or(self.prompt);
        self
    }
}

/// Nothing more to do.
impl From<()> for Outcome {
    fn from(_: ()) -> Self {
        Self::default()
    }
}

impl From<Effect> for Outcome {
    fn from(effect: Effect) -> Self {
        Some(effect).into()
    }
}

impl From<Option<Effect>> for Outcome {
    fn from(effect: Option<Effect>) -> Self {
        Self {
            effects: effect.into_iter().collect(),
            ..Default::default()
        }
    }
}

/// A call to make; its answer comes back as an [`Update`].
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Send(ChatId, PromptRequest),
    Cancel(String),
    /// Ask a node what its agent offers, after a pause.
    FetchOptions {
        node: String,
        after: Duration,
    },
    /// Ask a node which sessions its agent has saved.
    FetchSessions(String),
    /// Have a node clone a repository (or reuse its clone) for a chat.
    PrepareProject {
        chat: ChatId,
        node: String,
        repository: String,
    },
    /// Ask a node which projects it has cloned, for a chat to browse.
    FetchProjects {
        chat: ChatId,
        node: String,
    },
    /// Ask a node's agent which model providers it knows, for a chat.
    FetchProviders {
        chat: ChatId,
        node: String,
    },
    /// Have a node's agent do a step of signing in to a provider, for a chat.
    Authenticate {
        chat: ChatId,
        node: String,
        provider: String,
        action: AuthAction,
    },
    /// Ask a node what was said in a session, for a chat resuming it.
    FetchHistory {
        chat: ChatId,
        node: String,
        session_id: String,
    },
    /// Connect (or disconnect) one of a node's MCP servers, for a chat.
    SwitchMcp {
        chat: ChatId,
        node: String,
        name: String,
        connect: bool,
    },
    /// Have a node with no agent start one.
    StartHarness {
        node: String,
        harness: String,
    },
}

impl Effect {
    pub fn fetch_options(node: &str) -> Self {
        Self::FetchOptions {
            node: node.to_string(),
            after: Duration::ZERO,
        }
    }
}

/// What background calls report.
pub enum Update {
    Chat(ChatId, Message),
    /// For every chat on the node.
    Options(String, Result<AgentOptions, Failure>),
    /// How the chat's MCP switch went: the node's options after it.
    McpSwitched {
        chat: ChatId,
        node: String,
        name: String,
        options: Result<AgentOptions, String>,
    },
    Nodes(Vec<NodeInfo>),
    Sessions(String, Result<Vec<AgentSession>, String>),
    /// The node, once it hosts the harness it was asked to start.
    HarnessStarted(String, Result<NodeInfo, String>),
}

/// Why a node couldn't say what its agent offers.
#[derive(Debug, Clone)]
pub struct Failure {
    pub message: String,
    /// It may answer in a moment: it timed out, or was out of reach.
    pub transient: bool,
}

impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        Self {
            message: message.to_string(),
            transient: false,
        }
    }
}
