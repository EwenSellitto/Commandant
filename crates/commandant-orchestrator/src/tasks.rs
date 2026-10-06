//! Fan-out of task events from workers to CLI subscribers, and the output
//! each running task keeps.

use std::collections::HashMap;
use std::sync::Mutex;

use commandant_common::lookup::{self, Match};
use commandant_proto::{TaskEvent, TaskFinished, TaskOutput};
use tokio::sync::broadcast;

use crate::output::Tail;
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
    output: Tail,
    /// Set once the task is over, until its output is stored.
    finished: Option<TaskFinished>,
}

impl RunningTask {
    fn publish(&self, event: impl Into<TaskEvent>) {
        // Having no subscriber left (the CLI detached) is fine.
        let _ = self.events.send(event.into());
    }

    /// Marks the task over and returns the output to store.
    fn finish(&mut self, finished: TaskFinished) -> Vec<TaskOutput> {
        self.finished = Some(finished);
        self.output.chunks()
    }
}

/// What a watcher gets: the events so far, then the rest while it runs.
pub struct Watch {
    pub backlog: Vec<TaskEvent>,
    pub live: Option<broadcast::Receiver<TaskEvent>>,
}

#[derive(Default)]
pub struct TaskHub {
    tasks: Mutex<HashMap<String, RunningTask>>,
}

impl TaskHub {
    /// Registers a task before it is dispatched, so no event can be missed.
    pub fn start(&self, task_id: &str, owner: Owner) -> broadcast::Receiver<TaskEvent> {
        let (events, subscriber) = broadcast::channel(EVENT_BUFFER);
        let task = RunningTask {
            owner,
            events,
            output: Tail::default(),
            finished: None,
        };
        self.tasks.lock().unwrap().insert(task_id.to_string(), task);
        subscriber
    }

    /// Finds a running task by id or unambiguous id prefix.
    pub fn find(&self, needle: &str) -> Option<(String, Owner)> {
        let tasks = self.tasks.lock().unwrap();
        let running = tasks.iter().filter(|(_, t)| t.finished.is_none());
        match lookup::find(running, needle, |(id, _)| id.as_str()) {
            Match::One((id, task)) => Some((id.clone(), task.owner.clone())),
            Match::Ambiguous | Match::None => None,
        }
    }

    /// The output a task kept so far and, while it runs, what follows, with
    /// nothing missed or repeated between the two. None once its output is
    /// stored, or for a task it never had.
    pub fn watch(&self, task_id: &str) -> Option<Watch> {
        let tasks = self.tasks.lock().unwrap();
        let task = tasks.get(task_id)?;
        let mut backlog: Vec<TaskEvent> =
            task.output.chunks().into_iter().map(Into::into).collect();
        let live = match &task.finished {
            Some(finished) => {
                backlog.push(finished.clone().into());
                None
            }
            None => Some(task.events.subscribe()),
        };
        Some(Watch { backlog, live })
    }

    pub fn output(&self, from: ConnId, output: TaskOutput) {
        let mut tasks = self.tasks.lock().unwrap();
        if let Some(task) = tasks
            .get_mut(&output.task_id)
            .filter(|t| t.owner.conn_id == from && t.finished.is_none())
        {
            task.output.push(&output);
            task.publish(output);
        }
    }

    /// Marks a task over and returns its output, to store before
    /// [`close`](Self::close)ing it. None if the task wasn't running on `from`.
    pub fn finish(&self, from: ConnId, finished: TaskFinished) -> Option<Vec<TaskOutput>> {
        let mut tasks = self.tasks.lock().unwrap();
        let task = tasks
            .get_mut(&finished.task_id)
            .filter(|t| t.owner.conn_id == from && t.finished.is_none())?;
        Some(task.finish(finished))
    }

    /// Publishes the final event of a finished task whose result is stored,
    /// so whoever sees it can count on the store, and forgets the task.
    pub fn close(&self, task_id: &str) {
        let task = self.tasks.lock().unwrap().remove(task_id);
        if let Some(RunningTask {
            finished: Some(finished),
            events,
            ..
        }) = task
        {
            let _ = events.send(finished.into());
        }
    }

    /// Forgets a task that was never dispatched, telling anyone already
    /// watching it why.
    pub fn abandon(&self, task_id: &str, error: &str) {
        if let Some(task) = self.tasks.lock().unwrap().remove(task_id) {
            task.publish(TaskFinished {
                task_id: task_id.to_string(),
                error: error.to_string(),
                ..Default::default()
            });
        }
    }

    /// Ends every task running on a connection that went away and returns
    /// their ids and output, to store before closing them.
    pub fn fail_connection(&self, conn_id: ConnId, error: &str) -> Vec<(String, Vec<TaskOutput>)> {
        let mut tasks = self.tasks.lock().unwrap();
        tasks
            .iter_mut()
            .filter(|(_, task)| task.owner.conn_id == conn_id && task.finished.is_none())
            .map(|(task_id, task)| {
                let output = task.finish(TaskFinished {
                    task_id: task_id.clone(),
                    exit_code: None,
                    error: error.to_string(),
                    ..Default::default()
                });
                (task_id.clone(), output)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use commandant_proto::OutputStream;
    use commandant_proto::task_event::Event;

    use super::*;

    const CONN: ConnId = 7;

    fn owner() -> Owner {
        Owner {
            node_id: "n".into(),
            conn_id: CONN,
        }
    }

    fn out(data: &str) -> TaskOutput {
        TaskOutput {
            task_id: "t".into(),
            stream: OutputStream::Stdout.into(),
            data: data.into(),
        }
    }

    fn text(events: &[TaskEvent]) -> String {
        events
            .iter()
            .filter_map(|e| match &e.event {
                Some(Event::Output(o)) => Some(String::from_utf8(o.data.clone()).unwrap()),
                _ => None,
            })
            .collect()
    }

    fn drain(live: &mut broadcast::Receiver<TaskEvent>) -> Vec<TaskEvent> {
        std::iter::from_fn(|| live.try_recv().ok()).collect()
    }

    fn is_finished(event: &TaskEvent) -> bool {
        matches!(event.event, Some(Event::Finished(_)))
    }

    #[test]
    fn a_watcher_gets_what_was_printed_then_what_follows() {
        let hub = TaskHub::default();
        let _starter = hub.start("t", owner());
        hub.output(CONN, out("one "));
        let Watch { backlog, live } = hub.watch("t").unwrap();
        let mut live = live.expect("still running");
        assert_eq!(text(&backlog), "one ");
        hub.output(CONN, out("two"));
        // Another connection can't speak for the task.
        hub.output(CONN + 1, out("forged"));
        let done = TaskFinished {
            task_id: "t".into(),
            exit_code: Some(0),
            ..Default::default()
        };
        let output = hub.finish(CONN, done).unwrap();
        let output: Vec<TaskEvent> = output.into_iter().map(Into::into).collect();
        assert_eq!(text(&output), "one two");

        // Only once its result is stored are watchers told it is over.
        let followed = drain(&mut live);
        assert_eq!(text(&followed), "two");
        assert!(!followed.iter().any(is_finished));
        hub.close("t");
        let last = drain(&mut live);
        assert_eq!(last.len(), 1);
        assert!(is_finished(&last[0]));
    }

    #[test]
    fn a_finished_task_replays_until_closed() {
        let hub = TaskHub::default();
        let _starter = hub.start("t", owner());
        hub.output(CONN, out("hi"));
        let lost = hub.fail_connection(CONN, "gone");
        assert_eq!(lost.len(), 1);
        assert!(hub.find("t").is_none(), "no longer cancellable");
        assert!(hub.fail_connection(CONN, "gone").is_empty());

        let Watch { backlog, live } = hub.watch("t").unwrap();
        assert!(live.is_none());
        assert_eq!(text(&backlog), "hi");
        assert!(
            matches!(&backlog.last().unwrap().event, Some(Event::Finished(f)) if f.error == "gone")
        );

        hub.close("t");
        assert!(hub.watch("t").is_none());
    }
}
