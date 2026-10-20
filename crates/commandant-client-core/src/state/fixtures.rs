//! What the clients' tests build their nodes and task events from.

use commandant_common::link::{Link, Pin};
use commandant_proto::task_event::Event as TaskEvent;
use commandant_proto::*;

use super::chat::{Message, RunId};

/// A server's admin link, pinned to its certificate.
pub fn link() -> Link {
    let admin = format!("cmda_{}", "ab".repeat(16));
    Link::new(&admin, "192.168.1.15:7400").with_pin(Some(Pin::of(b"a certificate")))
}

pub fn node(id: &str, online: bool) -> NodeInfo {
    NodeInfo {
        id: id.into(),
        name: format!("box-{id}"),
        online,
        harnesses: vec!["opencode".into()],
        ..Default::default()
    }
}

/// An online node hosting no agent, which can start one.
pub fn bare(id: &str) -> NodeInfo {
    NodeInfo {
        harnesses: Vec::new(),
        can_host: vec!["opencode".into()],
        ..node(id, true)
    }
}

pub fn started(task_id: &str) -> Message {
    Message::Task(TaskEvent::Started(TaskStarted {
        task_id: task_id.into(),
        ..Default::default()
    }))
}

/// What a command's task reports, from what `message` (a task event) says
/// for a turn's.
pub fn for_run(run: RunId, message: Message) -> Message {
    match message {
        Message::Task(event) => Message::Run(run, Ok(event)),
        other => other,
    }
}

pub fn output(stream: OutputStream, data: &[u8]) -> Message {
    Message::Task(TaskEvent::Output(TaskOutput {
        stream: stream as i32,
        data: data.to_vec(),
        ..Default::default()
    }))
}

/// A model provider, signed out, to sign in to with `methods`.
pub fn acme(methods: Vec<AuthMethod>) -> ModelProvider {
    ModelProvider {
        id: "acme".into(),
        name: "Acme".into(),
        methods,
        ..Default::default()
    }
}

pub fn method(label: &str, oauth: bool, index: u32) -> AuthMethod {
    AuthMethod {
        label: label.into(),
        oauth,
        index,
    }
}
