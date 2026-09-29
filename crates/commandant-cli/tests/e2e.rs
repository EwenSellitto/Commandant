//! Runs an orchestrator and a worker in-process and drives them through the
//! Control API, like the CLI does.

use std::path::Path;
use std::time::Duration;

use commandant_common::link::Link;
use commandant_orchestrator::Orchestrator;
use commandant_proto::*;
use commandant_worker::WorkerConfig;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const WAIT: Duration = Duration::from_secs(15);

struct TaskResult {
    stdout: String,
    stderr: String,
    finished: TaskFinished,
}

async fn start_orchestrator(data_dir: &Path) -> (String, String, oneshot::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let running = serve_on(data_dir, listener).await;
    (running.addr, running.token, running.stop)
}

/// An orchestrator that can be stopped and waited for, so another can take
/// its port.
struct Running {
    addr: String,
    token: String,
    stop: oneshot::Sender<()>,
    served: JoinHandle<anyhow::Result<()>>,
}

impl Running {
    async fn stop(self) {
        let _ = self.stop.send(());
        tokio::time::timeout(WAIT, self.served)
            .await
            .expect("orchestrator never stopped")
            .unwrap()
            .unwrap();
    }
}

/// Binds `port` again once a stopped orchestrator has let go of it, which can
/// take a moment after it stops.
async fn rebind(port: u16) -> TcpListener {
    tokio::time::timeout(WAIT, async {
        loop {
            match TcpListener::bind(("127.0.0.1", port)).await {
                Ok(listener) => return listener,
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    })
    .await
    .expect("the port was never released")
}

async fn serve_on(data_dir: &Path, listener: TcpListener) -> Running {
    let orchestrator = Orchestrator::open(data_dir).await.unwrap();
    let token = orchestrator.admin_token().to_string();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    let (stop, stop_rx) = oneshot::channel::<()>();
    let served = tokio::spawn(orchestrator.serve(listener, async {
        let _ = stop_rx.await;
    }));
    Running {
        addr,
        token,
        stop,
        served,
    }
}

fn spawn_worker(
    addr: &str,
    join_token: Option<String>,
    state_dir: &Path,
) -> JoinHandle<anyhow::Result<()>> {
    tokio::spawn(commandant_worker::run(WorkerConfig {
        server: addr.to_string(),
        join_token,
        name: Some("w1".into()),
        state_dir: state_dir.to_path_buf(),
        harness: None,
    }))
}

async fn wait_for_node(client: &mut ControlClient, online: bool) -> NodeInfo {
    wait_for_named(client, "w1", online).await
}

async fn wait_for_named(client: &mut ControlClient, name: &str, online: bool) -> NodeInfo {
    tokio::time::timeout(WAIT, async {
        loop {
            let nodes = client
                .list_nodes(ListNodesRequest {})
                .await
                .unwrap()
                .into_inner()
                .nodes;
            if let Some(node) = nodes
                .into_iter()
                .find(|n| n.name == name && n.online == online)
            {
                return node;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("node {name} never became online={online}"))
}

/// The credentials a worker saved. It saves them after the orchestrator
/// already lists it as online, so they may take a moment.
async fn saved_credentials(state_dir: &Path) -> commandant_worker::state::Credentials {
    tokio::time::timeout(WAIT, async {
        loop {
            if let Some(creds) = commandant_worker::state::load(state_dir).unwrap() {
                return creds;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the worker never saved its credentials")
}

async fn start_task(
    client: &mut ControlClient,
    argv: &[&str],
) -> (String, tonic::Streaming<TaskEvent>) {
    let request = RunCommandRequest {
        node: "w1".into(),
        argv: argv.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };
    let mut events = client.run_command(request).await.unwrap().into_inner();
    let Some(TaskEvent {
        event: Some(task_event::Event::Started(started)),
    }) = events.message().await.unwrap()
    else {
        panic!("first event must be Started");
    };
    (started.task_id, events)
}

async fn collect(mut events: tonic::Streaming<TaskEvent>) -> TaskResult {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let finished = tokio::time::timeout(WAIT, async {
        loop {
            match events.message().await.unwrap().and_then(|e| e.event) {
                Some(task_event::Event::Output(out)) if out.stream() == OutputStream::Stderr => {
                    stderr.extend(out.data)
                }
                Some(task_event::Event::Output(out)) => stdout.extend(out.data),
                Some(task_event::Event::Finished(done)) => return done,
                other => panic!("unexpected event {other:?}"),
            }
        }
    })
    .await
    .expect("task never finished");
    TaskResult {
        stdout: String::from_utf8(stdout).unwrap(),
        stderr: String::from_utf8(stderr).unwrap(),
        finished,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_joins_runs_commands_and_reconnects() {
    let tmp = tempfile::tempdir().unwrap();
    let (addr, admin_token, _stop) = start_orchestrator(&tmp.path().join("server")).await;
    let mut client = connect_control(&addr, &admin_token).await.unwrap();

    // A bad admin token is rejected.
    let mut intruder = connect_control(&addr, "cmda_nope").await.unwrap();
    let err = intruder.list_nodes(ListNodesRequest {}).await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unauthenticated);

    // Join with a single-use token.
    let join = client
        .create_join_token(CreateJoinTokenRequest {
            ttl_secs: 600,
            reusable: false,
        })
        .await
        .unwrap()
        .into_inner()
        .token;
    let state_dir = tmp.path().join("worker");
    let worker = spawn_worker(&addr, Some(join.clone()), &state_dir);
    let node = wait_for_node(&mut client, true).await;
    assert_eq!(node.os, std::env::consts::OS);

    // Without a harness, the node has no agent to prompt.
    assert!(node.harnesses.is_empty());
    let prompt = PromptRequest {
        node: "w1".into(),
        prompt: "hello".into(),
        ..Default::default()
    };
    let err = client.prompt(prompt).await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);

    // Output and exit code are streamed back.
    let (_, events) = start_task(&mut client, &["sh", "-c", "echo hi; echo err >&2; exit 3"]).await;
    let result = collect(events).await;
    assert_eq!(result.stdout, "hi\n");
    assert_eq!(result.stderr, "err\n");
    assert_eq!(result.finished.exit_code, Some(3));
    assert!(!result.finished.cancelled);

    // Cancelling kills the whole process group.
    let (task_id, events) = start_task(&mut client, &["sh", "-c", "sleep 30 & sleep 30"]).await;
    client
        .cancel_task(CancelTaskRequest { task_id })
        .await
        .unwrap();
    let result = collect(events).await;
    assert!(result.finished.cancelled, "{:?}", result.finished);

    let tasks = client
        .list_tasks(ListTasksRequest { limit: 10 })
        .await
        .unwrap()
        .into_inner()
        .tasks;
    let statuses: Vec<_> = tasks.iter().map(|t| t.status.as_str()).collect();
    assert_eq!(statuses, ["cancelled", "failed"]);

    // A restarted worker reuses its stored credentials (the token is spent).
    worker.abort();
    wait_for_node(&mut client, false).await;
    let worker = spawn_worker(&addr, None, &state_dir);
    let again = wait_for_node(&mut client, true).await;
    assert_eq!(again.id, node.id);
    let nodes = client
        .list_nodes(ListNodesRequest {})
        .await
        .unwrap()
        .into_inner()
        .nodes;
    assert_eq!(nodes.len(), 1);

    // The spent single-use token can't enrol another node.
    let other = tempfile::tempdir().unwrap();
    let res = tokio::time::timeout(
        WAIT,
        commandant_worker::run(WorkerConfig {
            server: addr.clone(),
            join_token: Some(join),
            name: Some("w2".into()),
            state_dir: other.path().to_path_buf(),
            harness: None,
        }),
    )
    .await
    .expect("a rejected join should fail fast");
    assert!(res.unwrap_err().to_string().contains("join token"));

    worker.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn admin_link_enrols_workers() {
    let tmp = tempfile::tempdir().unwrap();
    let (addr, admin_token, _stop) = start_orchestrator(&tmp.path().join("server")).await;
    let mut client = connect_control(&addr, &admin_token).await.unwrap();

    // The admin token is reusable as a join token, so one link serves everyone.
    // The worker goes through the link as printed, which stays short.
    let printed = Link::for_addr(&admin_token, &addr).to_string();
    assert!(printed.len() <= 48, "{printed} is {} chars", printed.len());
    let link: Link = printed.parse().unwrap();
    let state_dir = tmp.path().join("worker");
    let worker = spawn_worker(&link.addr(), Some(link.token.clone()), &state_dir);
    wait_for_node(&mut client, true).await;

    // The worker remembers the orchestrator for argument-less restarts.
    let creds = saved_credentials(&state_dir).await;
    assert_eq!(creds.server.as_deref(), Some(addr.as_str()));
    worker.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restarted_server_keeps_its_link_and_nodes() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("server");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let first = serve_on(&data_dir, listener).await;
    let (addr, token) = (first.addr.clone(), first.token.clone());
    let mut client = connect_control(&addr, &token).await.unwrap();
    let state_dir = tmp.path().join("worker");
    let worker = spawn_worker(&addr, Some(token.clone()), &state_dir);
    let node = wait_for_node(&mut client, true).await;
    saved_credentials(&state_dir).await;

    // Even without its admin.token file, the database brings the token back.
    first.stop().await;
    std::fs::remove_file(data_dir.join(commandant_orchestrator::ADMIN_TOKEN_FILE)).unwrap();
    let second = serve_on(&data_dir, rebind(port).await).await;
    assert_eq!(second.token, token);

    // The worker reconnects as the same node, and the old link still works.
    let mut client = connect_control(&addr, &token).await.unwrap();
    let again = wait_for_node(&mut client, true).await;
    assert_eq!(again.id, node.id);

    worker.abort();
    second.stop().await;
}

/// `commandant server --reset`, run as a process: the confirmations need a
/// terminal, so its stdin is a pseudo-terminal the test types into.
#[cfg(unix)]
mod reset {
    use std::fs::File;
    use std::io::Write;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::process::Stdio;

    use super::*;

    struct Pty {
        /// What the test types into.
        keyboard: File,
        /// The server's stdin.
        terminal: OwnedFd,
    }

    fn pty() -> Pty {
        let (mut master, mut slave) = (0, 0);
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(opened, 0, "openpty: {}", std::io::Error::last_os_error());
        unsafe {
            Pty {
                keyboard: File::from_raw_fd(master),
                terminal: OwnedFd::from_raw_fd(slave),
            }
        }
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn server(data_dir: &Path, port: u16, stdin: Stdio) -> tokio::process::Child {
        tokio::process::Command::new(env!("CARGO_BIN_EXE_commandant"))
            .args(["server", "--reset", "--local-worker", "--data-dir"])
            .arg(data_dir)
            .args(["--listen", &format!("127.0.0.1:{port}")])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .stdin(stdin)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    }

    /// Runs a reset answered with `answers` that must not go through.
    async fn refused(data_dir: &Path, port: u16, answers: Option<&str>) -> String {
        let child = match answers {
            None => server(data_dir, port, Stdio::null()),
            Some(answers) => {
                let mut pty = pty();
                let child = server(data_dir, port, pty.terminal.into());
                pty.keyboard.write_all(answers.as_bytes()).unwrap();
                // Kept open until the server exits, or it would read EOF.
                let out = tokio::time::timeout(WAIT, child.wait_with_output()).await;
                drop(pty.keyboard);
                return stderr_of_failure(out.expect("reset never gave up").unwrap());
            }
        };
        let out = tokio::time::timeout(WAIT, child.wait_with_output())
            .await
            .expect("reset never gave up")
            .unwrap();
        stderr_of_failure(out)
    }

    fn stderr_of_failure(out: std::process::Output) -> String {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(!out.status.success(), "reset went through: {stderr}");
        stderr
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn asks_twice_then_starts_afresh() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join("server");
        let token_file = data_dir.join(commandant_orchestrator::ADMIN_TOKEN_FILE);
        let port = free_port();

        // A server whose local worker has joined, and a remembered host.
        let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
        let running = serve_on(&data_dir, listener).await;
        let (addr, old_token) = (running.addr.clone(), running.token.clone());
        let mut client = connect_control(&addr, &old_token).await.unwrap();
        let state_dir = data_dir.join("local-worker");
        let worker = spawn_worker(&addr, Some(old_token.clone()), &state_dir);
        let old_node = wait_for_node(&mut client, true).await;
        saved_credentials(&state_dir).await;
        worker.abort();
        running.stop().await;
        drop(rebind(port).await);
        std::fs::write(data_dir.join("advertise"), "box.lan\n").unwrap();

        // Without a terminal nobody can confirm, and anything but "y" then
        // "reset" cancels. None of it touches the database.
        let stderr = refused(&data_dir, port, None).await;
        assert!(stderr.contains("needs a terminal"), "{stderr}");
        for answers in ["n\n", "\n", "y\nyes\n"] {
            let stderr = refused(&data_dir, port, Some(answers)).await;
            assert!(stderr.contains("reset cancelled"), "{answers:?}: {stderr}");
            assert_eq!(read_trimmed(&token_file), old_token);
        }

        // Confirmed: a new token, and the local worker joins the new database.
        let mut pty = pty();
        let mut child = server(&data_dir, port, pty.terminal.into());
        pty.keyboard.write_all(b"y\nreset\n").unwrap();
        let new_token = tokio::time::timeout(WAIT, async {
            loop {
                match std::fs::read_to_string(&token_file) {
                    Ok(token) if !token.trim().is_empty() && token.trim() != old_token => {
                        return token.trim().to_string();
                    }
                    _ => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            }
        })
        .await
        .expect("the reset server never wrote a new token");
        assert!(
            connect_control(&addr, &old_token)
                .await
                .unwrap()
                .list_nodes(ListNodesRequest {})
                .await
                .is_err()
        );
        let mut client = connect_control(&addr, &new_token).await.unwrap();
        let hostname = old_node.hostname.clone();
        let node = wait_for_named(&mut client, &hostname, true).await;
        assert_ne!(node.id, old_node.id);

        // The link carries the new token; the remembered host stays.
        let link: Link = read_trimmed(&data_dir.join("link")).parse().unwrap();
        assert_eq!(link.token, new_token);
        assert_eq!(link.host, format!("box.lan:{port}"));

        let pid = child.id().expect("still running") as i32;
        assert_eq!(unsafe { libc::kill(pid, libc::SIGINT) }, 0);
        let status = tokio::time::timeout(WAIT, child.wait())
            .await
            .expect("server never stopped")
            .unwrap();
        assert!(status.success(), "{status}");
        drop(pty.keyboard);
    }

    fn read_trimmed(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap().trim().to_string()
    }
}
