//! `commandant run` and `commandant prompt`: start a task on a node and
//! stream its output; `commandant task watch`: stream a task's output again.

use std::io::Write;

use anyhow::{Result, bail};
use commandant_client_core::config::Client;
use commandant_proto::task_event::Event;
use commandant_proto::*;

use crate::cli::{PromptArgs, RunArgs};

/// The shell convention for "interrupted by Ctrl-C".
const EXIT_INTERRUPTED: i32 = 130;
const EXIT_FAILED: i32 = 1;

/// Returns the remote command's exit code.
pub async fn run(client: &Client, args: RunArgs) -> Result<i32> {
    let mut control = client.connect().await?;
    let request = RunCommandRequest {
        node: args.node,
        argv: args.argv,
        cwd: args.cwd.unwrap_or_default(),
        env: args.env.into_iter().collect(),
    };
    let mut events = control.run_command(request).await?.into_inner();
    stream_task(&mut control, &mut events, OnCtrlC::Cancel).await
}

/// Returns 0 once the agent has replied.
pub async fn prompt(client: &Client, args: PromptArgs) -> Result<i32> {
    let mut control = client.connect().await?;
    let request = PromptRequest {
        node: args.node,
        prompt: args.prompt.join(" "),
        command: args.command.unwrap_or_default(),
        session_id: args.session.unwrap_or_default(),
        cwd: args.cwd.unwrap_or_default(),
        model: args.model.unwrap_or_default(),
        agent: args.agent.unwrap_or_default(),
        variant: args.effort.unwrap_or_default(),
    };
    let mut events = control.prompt(request).await?.into_inner();
    stream_task(&mut control, &mut events, OnCtrlC::Cancel).await
}

/// Prints what a task printed so far, then follows it to its end. Returns
/// its exit code, as `run` does.
pub async fn watch(client: &Client, task_id: String) -> Result<i32> {
    let mut control = client.connect().await?;
    let mut events = control
        .watch_task(WatchTaskRequest { task_id })
        .await?
        .into_inner();
    stream_task(&mut control, &mut events, OnCtrlC::Detach).await
}

/// What Ctrl-C does to a task being streamed.
enum OnCtrlC {
    /// Cancel it; a second Ctrl-C detaches.
    Cancel,
    /// Leave it running: only watching stops.
    Detach,
}

/// Prints task output as it arrives, until the task finishes or Ctrl-C does
/// what `on_ctrl_c` says.
async fn stream_task(
    control: &mut ControlClient,
    events: &mut tonic::Streaming<TaskEvent>,
    on_ctrl_c: OnCtrlC,
) -> Result<i32> {
    let mut task_id: Option<String> = None;
    let mut cancel_requested = false;
    loop {
        let event = tokio::select! {
            event = events.message() => event?,
            _ = tokio::signal::ctrl_c() => {
                match (&on_ctrl_c, &task_id, cancel_requested) {
                    (OnCtrlC::Cancel, Some(task_id), false) => {
                        eprintln!("\ncancelling task {task_id} (Ctrl-C again to detach)");
                        let task_id = task_id.clone();
                        control.cancel_task(CancelTaskRequest { task_id }).await?;
                        cancel_requested = true;
                        continue;
                    }
                    _ => return Ok(EXIT_INTERRUPTED),
                }
            }
        };
        let Some(TaskEvent { event: Some(event) }) = event else {
            bail!("stream ended before the task finished");
        };
        match event {
            Event::Started(started) => {
                if started.output_pruned {
                    eprintln!("commandant: output pruned (only the newest tasks keep theirs)");
                }
                task_id = Some(started.task_id);
            }
            Event::Output(output) if output.stream() == OutputStream::Stderr => {
                write_now(std::io::stderr(), &output.data)?
            }
            // The model's thinking isn't part of the reply.
            Event::Output(output) if output.stream() == OutputStream::Reasoning => {}
            Event::Output(output) => write_now(std::io::stdout(), &output.data)?,
            Event::Finished(finished) => {
                if !finished.error.is_empty() {
                    eprintln!("commandant: {}", finished.error);
                }
                if !finished.session_id.is_empty() {
                    eprintln!(
                        "commandant: continue with --session {}",
                        finished.session_id
                    );
                }
                return Ok(exit_code(&finished));
            }
        }
    }
}

fn write_now(mut out: impl Write, data: &[u8]) -> std::io::Result<()> {
    out.write_all(data)?;
    out.flush()
}

fn exit_code(finished: &TaskFinished) -> i32 {
    match (finished.exit_code, finished.cancelled) {
        (Some(code), _) => code,
        (None, true) => EXIT_INTERRUPTED,
        (None, false) => EXIT_FAILED,
    }
}
