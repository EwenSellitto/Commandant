//! Commands run from a chat with `!`: each is a block in the thread, with
//! what it printed and how it ended. They run alongside the agent's turn
//! and never reach the agent.

use commandant_proto::task_event::Event as TaskEvent;
use commandant_proto::{OutputStream, RunCommandRequest, TaskFinished};

use super::events::decode;
use super::{Chat, Entry, Role, Unseen};
use crate::state::{Effect, Outcome};

/// Tells a chat's commands apart.
pub type RunId = u64;

/// How much of a command's output its block keeps: the end of it. The
/// server keeps more.
const KEPT: usize = 64 * 1024;

/// A command run from the chat, as its block shows it.
#[derive(Debug)]
pub struct Run {
    pub id: RunId,
    /// What it printed, in order: the end of it, if it printed a lot.
    pub output: Vec<Printed>,
    /// Whether the start of its output was dropped.
    pub dropped: bool,
    /// How many bytes it has printed in all, kept or not.
    pub printed: usize,
    /// How much of its output is kept, at most: the end of it.
    keep: usize,
    /// How it ended; `None` while it runs.
    pub ended: Option<Ended>,
    /// Known once the server has started it, or from the start when
    /// watched.
    pub(in crate::state) task_id: Option<String>,
    /// Asked to stop; one not started yet is stopped once it is.
    cancelling: bool,
    /// Bytes of a UTF-8 character split across output chunks, per stream.
    partial_stdout: Vec<u8>,
    partial_stderr: Vec<u8>,
}

/// A piece of a command's output, from one stream.
#[derive(Debug, Clone, PartialEq)]
pub struct Printed {
    /// Written to its error output.
    pub stderr: bool,
    pub text: String,
}

/// How a command ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Ended {
    Exited(i32),
    Cancelled,
    /// It couldn't start, was killed, or its stream broke: why.
    Failed(String),
}

impl Ended {
    pub fn succeeded(&self) -> bool {
        matches!(self, Self::Exited(0))
    }
}

impl Run {
    /// A command just asked for, keeping the last `keep` bytes it prints.
    pub fn new(id: RunId, keep: usize) -> Self {
        Self {
            id,
            output: Vec::new(),
            dropped: false,
            printed: 0,
            keep,
            ended: None,
            task_id: None,
            cancelling: false,
            partial_stdout: Vec::new(),
            partial_stderr: Vec::new(),
        }
    }

    pub fn running(&self) -> bool {
        self.ended.is_none()
    }

    /// Whether it has been asked to stop and hasn't yet.
    pub fn stopping(&self) -> bool {
        self.running() && self.cancelling
    }

    /// Its task, while it runs.
    pub fn task(&self) -> Option<&String> {
        self.task_id.as_ref().filter(|_| self.running())
    }

    /// Asks it to stop, once: the task to cancel now, if it has started.
    pub(in crate::state) fn stop(&mut self) -> Option<String> {
        if !self.running() || self.cancelling {
            return None;
        }
        self.cancelling = true;
        self.task_id.clone()
    }

    /// Takes in what its task reports: the task to cancel, if it was asked
    /// to stop before it had started.
    pub(in crate::state) fn on_event(
        &mut self,
        event: Result<TaskEvent, String>,
    ) -> Option<String> {
        let ended = match event {
            Ok(TaskEvent::Started(started)) => {
                let known = self.task_id.replace(started.task_id.clone());
                // Asked to stop before it was known: cancelled now.
                return (self.cancelling && known.is_none()).then_some(started.task_id);
            }
            Ok(TaskEvent::Output(output)) => {
                self.print(output.stream() == OutputStream::Stderr, &output.data);
                return None;
            }
            Ok(TaskEvent::Finished(finished)) => Ended::from(finished),
            Err(error) => Ended::Failed(error),
        };
        self.end(ended);
        None
    }

    /// Adds what a stream printed, to the last piece if it came from the same one.
    fn print(&mut self, stderr: bool, data: &[u8]) {
        self.printed += data.len();
        let partial = match stderr {
            true => &mut self.partial_stderr,
            false => &mut self.partial_stdout,
        };
        let text = decode(partial, data);
        if text.is_empty() {
            return;
        }
        match self.output.last_mut() {
            Some(last) if last.stderr == stderr => last.text.push_str(&text),
            _ => self.output.push(Printed { stderr, text }),
        }
        self.keep_the_end();
    }

    /// Drops the start of the output past what is kept, up to the end of
    /// a line, so the first line kept is whole.
    fn keep_the_end(&mut self) {
        let size: usize = self.output.iter().map(|p| p.text.len()).sum();
        let mut excess = size.saturating_sub(self.keep);
        while excess > 0
            && let Some(first) = self.output.first_mut()
        {
            let text = &mut first.text;
            let newline = text.bytes().skip(excess).position(|b| b == b'\n');
            let cut = newline.map_or(text.len(), |at| excess + at + 1);
            text.drain(..cut);
            if text.is_empty() {
                self.output.remove(0);
            }
            excess = excess.saturating_sub(cut);
            self.dropped = true;
        }
    }

    fn end(&mut self, ended: Ended) {
        self.partial_stdout.clear();
        self.partial_stderr.clear();
        self.ended = Some(ended);
    }
}

impl From<TaskFinished> for Ended {
    fn from(finished: TaskFinished) -> Self {
        match finished.exit_code {
            _ if finished.cancelled => Self::Cancelled,
            _ if !finished.error.is_empty() => Self::Failed(finished.error),
            Some(code) => Self::Exited(code),
            None => Self::Failed("it ended without an exit status".into()),
        }
    }
}

impl Chat {
    /// Runs `line` as a shell command on the node, in the chat's directory,
    /// in a block of its own.
    pub(super) fn run(&mut self, line: &str) -> Outcome {
        if line.is_empty() {
            self.info("! runs a shell command on the node, in this chat's directory: !ls");
            return Outcome::default();
        }
        let id = self.next_run;
        self.next_run += 1;
        self.thread.push(Entry {
            role: Role::Command,
            text: line.to_string(),
            agent: self.agent().to_string(),
            run: Some(Run::new(id, KEPT)),
        });
        let request = RunCommandRequest {
            node: self.node.id.clone(),
            argv: vec!["sh".into(), "-c".into(), line.to_string()],
            cwd: self.settings.cwd.clone(),
            env: Default::default(),
        };
        let run = Effect::RunCommand {
            chat: self.id,
            run: id,
            request,
        };
        self.cleared(Some(run))
    }

    /// The commands run from the chat, oldest first, each with its line.
    pub fn commands(&self) -> impl Iterator<Item = (&str, &Run)> {
        let runs = self.thread.iter();
        runs.filter_map(|e| Some((e.text.as_str(), e.run.as_ref()?)))
    }

    /// The commands run from the chat, oldest first.
    pub fn runs(&self) -> impl Iterator<Item = &Run> {
        self.commands().map(|(_, run)| run)
    }

    /// The tasks of the commands still running.
    pub(super) fn run_tasks(&self) -> impl Iterator<Item = &String> {
        self.runs().filter_map(Run::task)
    }

    fn runs_mut(&mut self) -> impl Iterator<Item = &mut Run> {
        self.thread.iter_mut().filter_map(|e| e.run.as_mut())
    }

    /// Stops every command still running; `None` if none is.
    pub(super) fn cancel_runs(&mut self) -> Option<Vec<Effect>> {
        let chat = self.id;
        let mut runs = self.runs_mut().filter(|r| r.running()).peekable();
        runs.peek()?;
        let stops = runs.filter_map(Run::stop);
        Some(stops.map(|task_id| Effect::Cancel(chat, task_id)).collect())
    }

    /// Stops the command `id`, if it still runs.
    pub(in crate::state) fn stop_run(&mut self, id: RunId) -> Option<Effect> {
        let chat = self.id;
        let run = self.runs_mut().find(|r| r.id == id)?;
        run.stop().map(|task_id| Effect::Cancel(chat, task_id))
    }

    /// Takes in what a command's task reports, which may call for an effect.
    pub(super) fn on_run(&mut self, id: RunId, event: Result<TaskEvent, String>) -> Option<Effect> {
        let chat = self.id;
        // What comes after its end is too late to matter.
        let run = self.runs_mut().find(|r| r.id == id && r.running())?;
        // Esc was pressed before the task had an id.
        let cancel = run
            .on_event(event)
            .map(|task_id| Effect::Cancel(chat, task_id));
        if let Some(ended) = &run.ended {
            self.unseen = Some(match ended.succeeded() {
                true => Unseen::Done,
                false => Unseen::Failed,
            });
        }
        cancel
    }

    /// The command whose task wouldn't stop, which can then be asked to
    /// again: its line; `None` if no command has that task.
    pub(super) fn stop_refused(&mut self, task_id: &str) -> Option<String> {
        let entry = self.thread.iter_mut().find(|e| {
            let run = e.run.as_ref();
            run.is_some_and(|r| r.task_id.as_deref() == Some(task_id))
        })?;
        entry.run.as_mut()?.cancelling = false;
        Some(entry.text.clone())
    }
}
