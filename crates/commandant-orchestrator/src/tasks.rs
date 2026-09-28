//! Fan-out of task events from workers to CLI subscribers.

use std::collections::HashMap;
use std::sync::Mutex;

use commandant_common::lookup::{self, Match};
use commandant_proto::{TaskEvent, TaskFinished, TaskOutput};
use tokio::sync::broadcast;

use crate::registry::ConnId;

const EVENT_BUFFER: usize = 4096;

/// The worker connection a task was dispatched on. Only it may report on the task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    pub node_id: String,
    pub conn_id: ConnId,
}

struct RunningTask {
    owner: Owner,
    events: broadcast::Sender<TaskEvent>,
}

impl RunningTask {
    fn publish(&self, event: impl Into<TaskEvent>) {
        // Having no subscriber left (the CLI detached) is fine.
        let _ = self.events.send(event.into());
    }
}

#[derive(Default)]
pub struct TaskHub {
    tasks: Mutex<HashMap<String, RunningTask>>,
}

impl TaskHub {
    /// Registers a task before it is dispatched, so no event can be missed.
    pub fn start(&self, task_id: &str, owner: Owner) -> broadcast::Receiver<TaskEvent> {
        let (events, subscriber) = broadcast::channel(EVENT_BUFFER);
        self.tasks
            .lock()
            .unwrap()
            .insert(task_id.to_string(), RunningTask { owner, events });
        subscriber
    }

    /// Finds a running task by id or unambiguous id prefix.
    pub fn find(&self, needle: &str) -> Option<(String, Owner)> {
        let tasks = self.tasks.lock().unwrap();
        match lookup::find(tasks.iter(), needle, |(id, _)| id.as_str()) {
            Match::One((id, task)) => Some((id.clone(), task.owner.clone())),
            Match::Ambiguous | Match::None => None,
        }
    }

    pub fn output(&self, from: ConnId, output: TaskOutput) {
        let tasks = self.tasks.lock().unwrap();
        if let Some(task) = tasks
            .get(&output.task_id)
            .filter(|t| t.owner.conn_id == from)
        {
            task.publish(output);
        }
    }

    /// Publishes the final event and forgets the task. Returns false if the
    /// task wasn't running on `from`.
    pub fn finish(&self, from: ConnId, finished: TaskFinished) -> bool {
        let mut tasks = self.tasks.lock().unwrap();
        let is_owner = tasks
            .get(&finished.task_id)
            .is_some_and(|t| t.owner.conn_id == from);
        if is_owner {
            let task = tasks
                .remove(&finished.task_id)
                .expect("present: just checked");
            task.publish(finished);
        }
        is_owner
    }

    /// Forgets a task that was never dispatched.
    pub fn abandon(&self, task_id: &str) {
        self.tasks.lock().unwrap().remove(task_id);
    }

    /// Ends every task running on a connection that went away and returns their ids.
    pub fn fail_connection(&self, conn_id: ConnId, error: &str) -> Vec<String> {
        let mut tasks = self.tasks.lock().unwrap();
        tasks
            .extract_if(|_, task| task.owner.conn_id == conn_id)
            .map(|(task_id, task)| {
                task.publish(TaskFinished {
                    task_id: task_id.clone(),
                    exit_code: None,
                    error: error.to_string(),
                    ..Default::default()
                });
                task_id
            })
            .collect()
    }
}
