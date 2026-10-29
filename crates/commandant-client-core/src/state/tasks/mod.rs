//! What the system has run: the list of tasks, kept fresh while it is
//! open, and the output of one, followed while it runs.

use std::time::Duration;

use commandant_common::time;
use commandant_proto::{AgentSession, RunCommandRequest, TaskInfo, task_event};

use super::chat::Run;
use super::{Ask, Choice, Choices, Choose, Effect, Go, Outcome, State, Wanted};

#[cfg(test)]
mod tests;

/// Tells the lists asked for apart, so the answer to one no longer shown
/// is dropped.
pub type ListId = u64;
/// Tells output views apart for as long as the client runs.
pub type OutputId = u64;

/// How often the open list is asked for again.
pub(crate) const REFRESH: Duration = Duration::from_secs(2);
/// How many tasks it shows, the newest.
pub(crate) const LIMIT: u32 = 200;
/// How much of its task's output an output view keeps: as much as the
/// server does.
const KEPT_IN_VIEW: usize = 1 << 20;
/// The question choosing which node's tasks to show.
const TASKS_OF: &str = "Show the tasks of";

/// The tasks the server keeps, newest first, as last listed.
#[derive(Debug, Default)]
pub struct Tasks {
    /// Only this node's; every node's when `None`.
    pub node: Option<String>,
    pub tasks: Vec<TaskInfo>,
    /// Not listed yet.
    pub loading: bool,
    /// Why the last listing failed; what was listed before stays.
    pub error: Option<String>,
    id: ListId,
}

/// One task's output, read-only, followed while it runs.
#[derive(Debug)]
pub struct Output {
    pub id: OutputId,
    /// What ran.
    pub line: String,
    /// The name of the node it ran on.
    pub node: String,
    /// Where it ran; empty for the worker's directory.
    pub cwd: String,
    /// What it printed and how it ended.
    pub run: Run,
    /// The server no longer keeps what it printed.
    pub pruned: bool,
    /// When it started and, once it has, ended, in Unix seconds.
    pub started: i64,
    pub finished: Option<i64>,
    /// Started from here rather than watched, so cancelled on quitting.
    pub(super) ran_here: bool,
    /// Closed while its command, started here, runs: followed unseen
    /// until it ends.
    pub(super) hidden: bool,
}

impl Output {
    /// Whether more may come: its task still runs.
    pub fn following(&self) -> bool {
        self.run.running()
    }
}

/// What a command task ran: its shell line, else its arguments.
pub fn command_line(task: &TaskInfo) -> String {
    match &task.argv[..] {
        [sh, c, line] if sh == "sh" && c == "-c" => line.clone(),
        argv => argv.join(" "),
    }
}

/// What a task ran: a prompt's text, or a command's line.
pub fn what_ran(task: &TaskInfo) -> String {
    match (task.kind.as_str(), &task.argv[..]) {
        ("prompt", [_harness, prompt]) => prompt.clone(),
        _ => command_line(task),
    }
}

impl State {
    /// Opens the list of `node`'s tasks, or every node's.
    pub(super) fn list_tasks(&mut self, node: Option<String>) -> Outcome {
        self.next_list += 1;
        let list = self.next_list;
        self.notice.clear();
        self.tasks = Some(Tasks {
            node: node.clone(),
            loading: true,
            id: list,
            ..Default::default()
        });
        let fetch = Effect::FetchTasks {
            node,
            after: Duration::ZERO,
            list,
        };
        Outcome::go(Go::Tasks).and(fetch.into())
    }

    /// Offers the nodes whose tasks to show.
    pub(super) fn filter_tasks(&mut self) {
        let current = self.tasks.as_ref().map(|t| t.node.clone());
        let every = Choice::new(Choose::TasksOf(None), "Every node", "");
        let nodes = self.nodes.iter().map(|n| {
            let state = if n.online { "online" } else { "offline" };
            Choice::new(Choose::TasksOf(Some(n.id.clone())), &n.name, state)
        });
        let choices = std::iter::once(every).chain(nodes).collect();
        let current = Some(Choose::TasksOf(current.flatten()));
        self.ask = Some(Choices::new(TASKS_OF, choices, current).into());
    }

    /// What the server listed for `list`, which is asked for again while
    /// it is shown.
    pub(super) fn on_tasks(
        &mut self,
        list: ListId,
        listed: Result<Vec<TaskInfo>, String>,
    ) -> Outcome {
        let Some(tasks) = self.tasks.as_mut().filter(|t| t.id == list) else {
            return Outcome::default();
        };
        tasks.loading = false;
        match listed {
            Ok(listed) => {
                tasks.tasks = listed;
                tasks.error = None;
            }
            Err(e) => tasks.error = Some(e),
        }
        Effect::FetchTasks {
            node: tasks.node.clone(),
            after: REFRESH,
            list,
        }
        .into()
    }

    /// Opens a task: a command's output, or a prompt's session in a chat,
    /// followed if it runs.
    pub(super) fn open_task(&mut self, task_id: &str) -> Outcome {
        let mut listed = self.tasks.iter().flat_map(|t| &t.tasks);
        let Some(task) = listed.find(|t| t.id == task_id).cloned() else {
            return Outcome::default();
        };
        if task.kind == "prompt" {
            return self.open_prompt(task);
        }
        let id = self.new_output(command_line(&task), task.node_name, task.cwd, false);
        if let Some(output) = self.outputs.last_mut() {
            output.started = task.created_at;
            output.finished = task.finished_at;
            output.pruned = task.output_pruned;
            // Known already, so it can be stopped before it reports starting.
            output.run.task_id = Some(task.id.clone());
        }
        let watch = Effect::Watch {
            output: id,
            task_id: task.id,
        };
        Outcome::go(Go::Output(id)).and(watch.into())
    }

    /// Opens the session a prompt went to, attached to its turn if that
    /// still runs; a chat open on it already is shown.
    fn open_prompt(&mut self, task: TaskInfo) -> Outcome {
        let running = task.status == "running";
        if task.session_id.is_empty() {
            self.notice.tell(match running {
                true => "this turn started a new session, which can be opened once it ends".into(),
                false => "this turn left no session to open".into(),
            });
            return Outcome::default();
        }
        let prompt = what_ran(&task);
        let open = self
            .chats
            .iter_mut()
            .find(|c| c.node.id == task.node_id && c.settings.session_id == task.session_id);
        if let Some(chat) = open {
            let attach =
                (running && !chat.activity.working()).then(|| chat.attach(&prompt, task.id));
            return Outcome::go(Go::Chat(chat.id)).and(attach.into());
        }
        let Some(node) = self.node(&task.node_id).cloned() else {
            self.notice
                .fail(format!("{} is no longer a node", task.node_name));
            return Outcome::default();
        };
        let mut saved = self.saved.get(&node.id).into_iter().flatten();
        let saved = saved.find(|s| s.id == task.session_id).cloned();
        let saved = saved.unwrap_or_else(|| AgentSession {
            id: task.session_id.clone(),
            title: prompt.clone(),
            directory: task.cwd.clone(),
            ..Default::default()
        });
        let outcome = self.resume_on(node, saved);
        let attach = match (outcome.go.clone(), running) {
            (Some(Go::Chat(id)), true) => self.chat_mut(id).map(|c| c.attach(&prompt, task.id)),
            _ => None,
        };
        outcome.and(attach.into())
    }

    /// An output view, waiting for what its task printed.
    fn new_output(&mut self, line: String, node: String, cwd: String, ran_here: bool) -> OutputId {
        self.next_output += 1;
        let id = self.next_output;
        self.outputs.push(Output {
            id,
            line,
            node,
            cwd,
            run: Run::new(id, KEPT_IN_VIEW),
            pruned: false,
            started: time::now(),
            finished: None,
            ran_here,
            hidden: false,
        });
        id
    }

    /// Runs `line` on a node, in its worker's directory, in an output
    /// view; asks for the line if there is none.
    pub(super) fn run_on(&mut self, node: &str, line: &str) -> Outcome {
        let Some(node) = self.node(node).cloned() else {
            return Outcome::default();
        };
        let line = line.trim();
        if line.is_empty() {
            self.ask = Some(Ask::Enter {
                label: format!("a shell command to run on {}", node.name),
                secret: false,
                wanted: Wanted::Command { node: node.id },
            });
            return Outcome::default();
        }
        let id = self.new_output(line.to_string(), node.name, String::new(), true);
        let request = RunCommandRequest {
            node: node.id,
            argv: vec!["sh".into(), "-c".into(), line.to_string()],
            ..Default::default()
        };
        let run = Effect::RunOn {
            output: id,
            request,
        };
        Outcome::go(Go::Output(id)).and(run.into())
    }

    /// Cancels a task; its output view, if open, says it is stopping.
    pub(super) fn cancel_task(&mut self, task_id: String) -> Outcome {
        let mut views = self.outputs.iter_mut();
        let view = views.find(|o| o.run.task() == Some(&task_id));
        // Asked already: once is enough.
        if let Some(view) = view {
            return view.run.stop().map(Effect::CancelTask).into();
        }
        Effect::CancelTask(task_id).into()
    }

    /// An open output view.
    pub fn output(&self, id: OutputId) -> Option<&Output> {
        self.outputs.iter().find(|o| o.id == id && !o.hidden)
    }

    /// The output views shown, oldest first.
    pub(super) fn shown_outputs(&self) -> impl Iterator<Item = &Output> {
        self.outputs.iter().filter(|o| !o.hidden)
    }

    /// The output view is no longer shown, so its task is no longer
    /// watched; but a command started here is followed till it ends, so
    /// quitting still stops it.
    pub(super) fn close_output(&mut self, id: OutputId) -> Outcome {
        let Some(output) = self.outputs.iter_mut().find(|o| o.id == id) else {
            return Outcome::default();
        };
        if output.ran_here && output.following() {
            output.hidden = true;
            return Outcome::default();
        }
        self.outputs.retain(|o| o.id != id);
        Effect::StopWatching(id).into()
    }

    /// What an output view's task reports.
    pub(super) fn on_output(
        &mut self,
        id: OutputId,
        event: Result<task_event::Event, String>,
    ) -> Outcome {
        let Some(output) = self.outputs.iter_mut().find(|o| o.id == id) else {
            return Outcome::default();
        };
        if let Ok(task_event::Event::Started(started)) = &event {
            output.pruned = started.output_pruned;
        }
        let cancel = output.run.on_event(event).map(Effect::CancelTask);
        if !output.following() && output.finished.is_none() {
            output.finished = Some(time::now());
        }
        if output.hidden && !output.following() {
            self.outputs.retain(|o| o.id != id);
        }
        cancel.into()
    }
}
