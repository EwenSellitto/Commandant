//! The client core: what any Commandant front end knows and does, apart
//! from how it is shown. It keeps the [`state`] and makes every call to the
//! server on a tokio runtime it is given, so a front end never touches
//! tonic: it sends [`Intent`]s, passes on the [`Update`]s it receives, and
//! reads what it shows through `&` accessors.

mod calls;
pub mod config;
pub mod state;

pub use self::state::chat::{self, Chat, Settings};
pub use self::state::{
    Ask, ChatId, Choice, Choices, Choose, Edit, Effect, Go, Intent, Scope, Update, Wanted, cycle,
    lacks_agent,
};

use anyhow::Result;
use commandant_proto::*;
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

use self::config::Client;
use self::state::{Outcome, State};

/// The state, and the calls it asks for.
pub struct Core {
    state: State,
    /// Where calls go; an offline core has none.
    server: Option<Server>,
    /// The calls an offline core would have made, for a front end's tests.
    #[cfg(any(test, feature = "test-support"))]
    kept: Vec<Effect>,
}

struct Server {
    control: ControlClient,
    handle: Handle,
    tx: mpsc::UnboundedSender<Update>,
    /// Keeps the nodes' details current.
    poll: AbortHandle,
}

/// What background calls report, for the front end to hand to
/// [`Core::on_update`]. Awaiting it needs no tokio runtime.
pub struct Updates(mpsc::UnboundedReceiver<Update>);

impl Updates {
    pub async fn next(&mut self) -> Option<Update> {
        self.0.recv().await
    }

    /// One that has come already, if any.
    pub fn ready(&mut self) -> Option<Update> {
        self.0.try_recv().ok()
    }
}

/// What the front end may do after an intent or an update.
#[derive(Debug, Default, PartialEq)]
pub struct Next {
    /// Where to go, if anywhere.
    pub go: Option<Go>,
    /// What a chat's prompt becomes.
    pub prompt: Option<(ChatId, Edit)>,
}

impl Core {
    /// Connects to the server and learns its nodes; new chats start with
    /// `defaults`. Calls are made on `handle`.
    pub async fn start(
        client: &Client,
        defaults: Settings,
        handle: Handle,
    ) -> Result<(Self, Updates)> {
        let client = client.clone();
        let connect = handle.spawn(async move {
            let mut control = client.connect().await?;
            let nodes = control.list_nodes(ListNodesRequest {}).await?;
            anyhow::Ok((control, nodes.into_inner().nodes))
        });
        let (control, nodes) = connect.await??;
        let (tx, rx) = mpsc::unbounded_channel();
        let poll = handle
            .spawn(calls::poll_nodes(control.clone(), tx.clone()))
            .abort_handle();
        let server = Server {
            control,
            handle,
            tx,
            poll,
        };
        let core = Self {
            state: State::new(nodes, defaults),
            server: Some(server),
            #[cfg(any(test, feature = "test-support"))]
            kept: Vec::new(),
        };
        Ok((core, Updates(rx)))
    }

    /// A core knowing `nodes` and no server: the calls it would make are
    /// kept, for [`take_effects`](Self::take_effects).
    #[cfg(any(test, feature = "test-support"))]
    pub fn offline(nodes: Vec<NodeInfo>, defaults: Settings) -> Self {
        Self {
            state: State::new(nodes, defaults),
            server: None,
            kept: Vec::new(),
        }
    }

    /// The calls an offline core would have made since last asked.
    #[cfg(any(test, feature = "test-support"))]
    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.kept)
    }

    /// Asks `ask` in `scope`, as the core will once it has a reason to.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_ask(&mut self, scope: Scope, ask: Ask) {
        self.state.set_ask(scope, ask);
    }

    /// Does what the person asks.
    pub fn act(&mut self, intent: Intent) -> Next {
        let outcome = self.state.intent(intent);
        self.follow(outcome)
    }

    /// Takes in what a background call reported.
    pub fn on_update(&mut self, update: Update) -> Next {
        let outcome = self.state.update(update);
        self.follow(outcome)
    }

    /// Cancels the tasks still running, so no agent works for nobody, and
    /// stops calling the server.
    pub async fn shutdown(&mut self) {
        let Some(server) = &self.server else {
            return;
        };
        server.poll.abort();
        let cancel = calls::cancel_all(server.control.clone(), self.state.running_tasks());
        let _ = server.handle.spawn(cancel).await;
    }

    /// Makes the calls an outcome asks for.
    fn follow(&mut self, outcome: Outcome) -> Next {
        let Outcome {
            effects,
            go,
            prompt,
        } = outcome;
        for effect in effects {
            self.call(effect);
        }
        Next { go, prompt }
    }

    fn call(&mut self, effect: Effect) {
        let Some(Server {
            control,
            handle,
            tx,
            ..
        }) = &self.server
        else {
            #[cfg(any(test, feature = "test-support"))]
            self.kept.push(effect);
            return;
        };
        // A failed cancel is told to the chat whose task it was.
        let chat = match &effect {
            Effect::Cancel(task_id) => self
                .state
                .chats
                .iter()
                .find(|c| c.running_task().as_ref() == Some(task_id))
                .map(|c| c.id),
            _ => None,
        };
        calls::make(effect, chat, control.clone(), handle, tx.clone());
    }

    /// The nodes, as last listed.
    pub fn nodes(&self) -> &[NodeInfo] {
        &self.state.nodes
    }

    /// Every open chat, on every node, oldest first.
    pub fn chats(&self) -> &[Chat] {
        &self.state.chats
    }

    pub fn chat(&self, id: ChatId) -> Option<&Chat> {
        self.state.chat(id)
    }

    /// The chats on a node, oldest first.
    pub fn chats_on<'a>(&'a self, node_id: &'a str) -> impl Iterator<Item = &'a Chat> {
        self.state.chats_on(node_id)
    }

    /// What is asked in `scope`, if anything.
    pub fn ask(&self, scope: Scope) -> Option<&Ask> {
        self.state.ask(scope)
    }

    /// Whether anything shown moves on its own: a spinner for a working
    /// chat, something loading, or a node starting its agent.
    pub fn busy(&self) -> bool {
        self.state.busy()
    }

    /// A remark about the nodes; empty if there is none.
    pub fn notice(&self) -> &str {
        &self.state.notice
    }

    /// Whether a node is starting its agent.
    pub fn starting(&self, node_id: &str) -> bool {
        self.state.starting.contains(node_id)
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        if let Some(server) = &self.server {
            server.poll.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn starting_needs_a_server_that_answers() {
        // A port nothing listens on.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let client = Client {
            addr: addr.clone(),
            token: "cmda_x".into(),
        };
        let handle = Handle::current();
        let Err(e) = Core::start(&client, Settings::default(), handle).await else {
            panic!("started with no server");
        };
        assert_eq!(e.to_string(), format!("connecting to {addr}"));
    }
}
