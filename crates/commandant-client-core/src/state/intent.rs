//! What goes into the state and what comes out: intents and updates in,
//! outcomes out.

use std::time::Duration;

use commandant_proto::{
    AgentOptions, AgentSession, AuthAction, NodeInfo, PromptRequest, RunCommandRequest, TaskInfo,
    task_event,
};

use super::chat::{Menu, Message, RunId};
use super::tasks::{ListId, OutputId};
use super::{ChatId, Choose};

/// What the person asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum Intent {
    /// Show one of a node's chats: `chat`, the one the front end last
    /// showed, if it is still open, else its newest, else a new one.
    Open {
        /// The node's id.
        node: String,
        /// The chat the front end last showed on it.
        chat: Option<ChatId>,
    },
    /// A new chat on a node, whatever state it is in, on one of its agent's
    /// sessions or else a new one: what a front end starts on.
    OpenSession {
        /// The node's id.
        node: String,
        /// The saved session to resume; none starts a new one.
        session: Option<String>,
    },
    /// Another chat on the same node, with the same settings.
    NewChat(ChatId),
    /// Choose one of the chat's node's chats or saved sessions.
    Sessions(ChatId),
    /// Choose one of a node's chats or saved sessions, over the chat
    /// [`Intent::Open`] shows on it.
    NodeSessions {
        /// The node's id.
        node: String,
        /// The chat [`Intent::Open`] would show on it.
        chat: Option<ChatId>,
    },
    /// Close a chat, unless its agent or a command is still at work.
    Close(ChatId),
    /// The front end shows the chat `to` instead of `chat`, which closes if
    /// it is blank and `to` isn't: nobody is left to write in it.
    Leave {
        /// The chat left.
        chat: ChatId,
        /// The chat shown instead.
        to: ChatId,
    },
    /// A line typed in a chat: a prompt, a `/command` or a `!command`.
    Submit(ChatId, String),
    /// Offer what the chat's menu chooses from, as its `/command` does,
    /// leaving what is typed alone.
    Menu(ChatId, Menu),
    /// Browse the chat's node's projects, as `/project` does, leaving what
    /// is typed alone.
    Projects(ChatId),
    /// Answer the scope's [`Ask::Choose`](super::Ask::Choose).
    Choose(Scope, Choose),
    /// Answer the scope's [`Ask::Enter`](super::Ask::Enter) with a line.
    Enter(Scope, String),
    /// Say yes to the scope's [`Ask::Confirm`](super::Ask::Confirm).
    Confirm(Scope),
    /// Leave the scope's question unanswered.
    Dismiss(Scope),
    /// Stop the chat's commands, or else cancel its turn.
    Cancel(ChatId),
    /// Stop one of the chat's commands, from its block.
    StopRun(ChatId, RunId),
    /// The next (or previous) agent.
    CycleAgent(ChatId, isize),
    /// The next thinking effort.
    CycleEffort(ChatId),
    /// The chat has been looked at, so how its last turn ended is no news.
    Seen(ChatId),
    /// Show the tasks of a node, or of every node, kept fresh until closed.
    ListTasks {
        /// The node's id; none for every node.
        node: Option<String>,
    },
    /// Choose which node's tasks to show.
    FilterTasks,
    /// The tasks are no longer shown.
    CloseTasks,
    /// Show what a listed task did: a command's output, followed while it runs.
    OpenTask(String),
    /// The output view is no longer shown.
    CloseOutput(OutputId),
    /// Cancel a task, wherever it was started.
    CancelTask(String),
    /// Run a shell command on a node, in its worker's directory, in an
    /// output view; an empty line asks for one.
    Run {
        /// The node's id.
        node: String,
        /// The command line, as typed.
        line: String,
    },
    /// Add a node: ask how long its token lasts, then show the command
    /// that enrols the machine it runs on.
    AddNode,
    /// Remove a node, once confirmed; it needs a new token to join again.
    RemoveNode(String),
    /// Start an agent on a node that hosts none, chosen among those it can.
    StartAgent(String),
    /// Show the server's link, to copy, as `/link` does.
    ShowLink,
    /// Leave, forgetting the server, once confirmed.
    LogOut,
}

/// Where a question, or an intent about one, belongs.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub enum Scope {
    /// The app as a whole, whatever is in view.
    App,
    /// One chat, answered there.
    Chat(ChatId),
}

/// Where the front end may go next.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Go {
    /// The chat.
    Chat(ChatId),
    /// The nodes, to open one.
    Nodes,
    /// The tasks the system has run.
    Tasks,
    /// One task's output.
    Output(OutputId),
    /// Close the front end.
    Quit,
    /// Leave, forgetting the server: back to pasting a link.
    LogOut,
    /// What every command and key does.
    Help,
    /// The node hosts an agent now: worth opening, if it is the one in view.
    Ready(String),
}

/// What a chat's prompt becomes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Edit {
    /// The line was taken.
    Clear,
    /// Typed at the cursor.
    Insert(String),
}

/// What an intent or an update comes to, beyond what the state keeps.
#[derive(Default)]
pub(crate) struct Outcome {
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
    pub(super) fn and(mut self, next: Self) -> Self {
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
        effect.into_iter().collect::<Vec<_>>().into()
    }
}

impl From<Vec<Effect>> for Outcome {
    fn from(effects: Vec<Effect>) -> Self {
        Self {
            effects,
            ..Default::default()
        }
    }
}

/// A call to make; its answer comes back as an [`Update`].
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Prompt the chat's agent; what it says comes back as the chat's messages.
    Send(ChatId, PromptRequest),
    /// Run a command for one of the chat's blocks.
    RunCommand {
        /// The chat the block is in.
        chat: ChatId,
        /// The block the output goes to.
        run: RunId,
        /// What to run, and where.
        request: RunCommandRequest,
    },
    /// Cancel a task of the chat's; a failure is told to the chat.
    Cancel(ChatId, String),
    /// Ask a node what its agent offers, after a pause.
    FetchOptions {
        /// The node's id.
        node: String,
        /// How long to wait first; zero asks now.
        after: Duration,
    },
    /// Ask a node which sessions its agent has saved.
    FetchSessions(String),
    /// Have a node clone a repository (or reuse its clone) for a chat.
    PrepareProject {
        /// The chat that works in the clone.
        chat: ChatId,
        /// The node's id.
        node: String,
        /// The repository's URL, as typed.
        repository: String,
    },
    /// Ask a node which projects it has cloned, for a chat to browse.
    FetchProjects {
        /// The chat that browses them.
        chat: ChatId,
        /// The node's id.
        node: String,
    },
    /// Ask a node's agent which model providers it knows, for a chat.
    FetchProviders {
        /// The chat that lists them.
        chat: ChatId,
        /// The node's id.
        node: String,
    },
    /// Have a node's agent do a step of signing in to a provider, for a chat.
    Authenticate {
        /// The chat signing in, told how it went.
        chat: ChatId,
        /// The node's id.
        node: String,
        /// The provider's id, as the agent names it.
        provider: String,
        /// The step to take.
        action: AuthAction,
    },
    /// Ask a node what was said in a session, for a chat resuming it.
    FetchHistory {
        /// The chat resuming the session.
        chat: ChatId,
        /// The node's id.
        node: String,
        /// The session, as the agent names it.
        session_id: String,
    },
    /// Connect (or disconnect) one of a node's MCP servers, for a chat.
    SwitchMcp {
        /// The chat told how it went.
        chat: ChatId,
        /// The node's id.
        node: String,
        /// The MCP server, as the agent names it.
        name: String,
        /// Connect it; `false` disconnects it.
        connect: bool,
    },
    /// Have a node with no agent start one.
    StartHarness {
        /// The node's id.
        node: String,
        /// One of the harnesses the node can host, as it names them.
        harness: String,
    },
    /// Follow a task's output, for an output view: what it printed so far,
    /// then what it prints until it ends.
    Watch {
        /// The view the output goes to.
        output: OutputId,
        /// The task, as the server names it.
        task_id: String,
    },
    /// Follow a chat's turn started elsewhere: what it said so far, then
    /// the rest.
    WatchTurn {
        /// The chat the turn is in.
        chat: ChatId,
        /// The turn's task, as the server names it.
        task_id: String,
    },
    /// Run a command for an output view.
    RunOn {
        /// The view the output goes to.
        output: OutputId,
        /// What to run, and where.
        request: RunCommandRequest,
    },
    /// Stop following the task of an output view.
    StopWatching(OutputId),
    /// Cancel a task; a failure is told in the notice.
    CancelTask(String),
    /// List the tasks of a node, or of every node, after a pause.
    FetchTasks {
        /// The node's id; none for every node.
        node: Option<String>,
        /// How long to wait first; zero asks now.
        after: Duration,
        /// The listing the answer is for, so a stale one is dropped.
        list: ListId,
    },
    /// Make a join token that works once, within `ttl`.
    CreateToken {
        /// How long it lasts unused.
        ttl: Duration,
    },
    /// Have the server forget a node.
    RemoveNode(String),
}

impl Effect {
    /// Ask the node with id `node` what its agent offers, now.
    pub fn fetch_options(node: &str) -> Self {
        Self::FetchOptions {
            node: node.to_string(),
            after: Duration::ZERO,
        }
    }
}

/// What background calls report.
pub enum Update {
    /// For the chat, as its agent or commands report.
    Chat(ChatId, Message),
    /// For every chat on the node.
    Options(String, Result<AgentOptions, Failure>),
    /// How the chat's MCP switch went: the node's options after it.
    McpSwitched {
        /// The chat that asked.
        chat: ChatId,
        /// The node's id.
        node: String,
        /// The MCP server switched.
        name: String,
        /// The node's options after it, or why the switch failed.
        options: Result<AgentOptions, String>,
    },
    /// The nodes, as the server last listed them.
    Nodes(Vec<NodeInfo>),
    /// The server couldn't list the nodes: it is out of reach, for now.
    Unreachable,
    /// The sessions a node's agent has saved, by the node's id.
    Sessions(String, Result<Vec<AgentSession>, String>),
    /// The node, once it hosts the harness it was asked to start.
    HarnessStarted(String, Result<NodeInfo, String>),
    /// What an output view's task reports, or why its stream broke.
    Output(OutputId, Result<task_event::Event, String>),
    /// A task couldn't be cancelled.
    CancelFailed {
        /// The task, as the server names it.
        task_id: String,
        /// Why, as the server says.
        error: String,
    },
    /// The tasks listed for a list, newest first.
    Tasks {
        /// The listing asked for.
        list: ListId,
        /// The tasks, or why they couldn't be listed.
        tasks: Result<Vec<TaskInfo>, String>,
    },
    /// A join token lasting `ttl` was made: the link a worker joins with.
    TokenCreated {
        /// How long the token lasts unused.
        ttl: Duration,
        /// The link, or why the token couldn't be made.
        link: Result<String, String>,
    },
    /// Whether the server forgot the node.
    NodeRemoved(String, Result<(), String>),
}

/// Why a node couldn't say what its agent offers.
#[derive(Clone, Debug)]
pub struct Failure {
    /// What went wrong, to show.
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
