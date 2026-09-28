//! Runs a single command and streams its output back.

use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use commandant_proto::{OutputStream, RunTask, TaskFinished, TaskOutput, WorkerMsg};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};

const CHUNK_SIZE: usize = 16 * 1024;
/// How long to keep draining pipes after the process exits (a background
/// grandchild may hold them open).
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Runs `task` until it exits or `cancel` fires (or its sender is dropped),
/// then reports a TaskFinished.
pub async fn run(task: RunTask, out: mpsc::Sender<WorkerMsg>, cancel: oneshot::Receiver<()>) {
    let task_id = task.task_id.clone();
    let finished = execute(task, &out, cancel)
        .await
        .unwrap_or_else(|e| TaskFinished {
            task_id,
            exit_code: None,
            error: e.to_string(),
            ..Default::default()
        });
    let _ = out.send(finished.into()).await;
}

async fn execute(
    task: RunTask,
    out: &mpsc::Sender<WorkerMsg>,
    cancel: oneshot::Receiver<()>,
) -> std::io::Result<TaskFinished> {
    let mut child = command(&task)?.spawn()?;
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let pipes = [
        tokio::spawn(forward_output(
            stdout,
            OutputStream::Stdout,
            task.task_id.clone(),
            out.clone(),
        )),
        tokio::spawn(forward_output(
            stderr,
            OutputStream::Stderr,
            task.task_id.clone(),
            out.clone(),
        )),
    ];

    let (status, cancelled) = tokio::select! {
        status = child.wait() => (status?, false),
        _ = cancel => {
            kill_group(&mut child).await;
            (child.wait().await?, true)
        }
    };
    for pipe in pipes {
        if tokio::time::timeout(DRAIN_TIMEOUT, pipe).await.is_err() {
            tracing::debug!(task_id = %task.task_id, "output still open after exit; detaching");
        }
    }

    Ok(TaskFinished {
        task_id: task.task_id,
        exit_code: status.code(),
        error: signal_description(status).unwrap_or_default(),
        cancelled,
        ..Default::default()
    })
}

/// No shell and no stdin. The command gets its own process group, so
/// cancelling also kills anything it spawned.
fn command(task: &RunTask) -> std::io::Result<Command> {
    let Some((program, args)) = task.argv.split_first() else {
        return Err(std::io::Error::other("empty command"));
    };
    let mut cmd = Command::new(program);
    cmd.args(args)
        .envs(&task.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if !task.cwd.is_empty() {
        cmd.current_dir(&task.cwd);
    }
    #[cfg(unix)]
    cmd.process_group(0);
    Ok(cmd)
}

fn signal_description(status: ExitStatus) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status
            .signal()
            .map(|signal| format!("terminated by signal {signal}"))
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

async fn kill_group(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: plain syscall; the group id equals the child's pid.
        unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
        return;
    }
    let _ = child.kill().await;
}

async fn forward_output(
    mut pipe: impl AsyncRead + Unpin,
    stream: OutputStream,
    task_id: String,
    out: mpsc::Sender<WorkerMsg>,
) {
    let mut buf = vec![0u8; CHUNK_SIZE];
    loop {
        let n = match pipe.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        let output = TaskOutput {
            task_id: task_id.clone(),
            stream: stream.into(),
            data: buf[..n].to_vec(),
        };
        if out.send(output.into()).await.is_err() {
            return;
        }
    }
}
