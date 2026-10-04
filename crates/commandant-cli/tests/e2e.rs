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

/// Polls `check` until it has an answer, failing with `what` after `WAIT`.
async fn eventually<T, F>(what: &str, mut check: impl FnMut() -> F) -> T
where
    F: Future<Output = Option<T>>,
{
    tokio::time::timeout(WAIT, async {
        loop {
            if let Some(found) = check().await {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what}"))
}

/// A worker named `w1` on `addr`, from `state_dir`; tests change the rest.
fn worker_config(addr: &str, join_token: Option<String>, state_dir: &Path) -> WorkerConfig {
    WorkerConfig {
        server: Some(addr.to_string()),
        join_token,
        name: Some("w1".into()),
        state_dir: state_dir.to_path_buf(),
        pick_free_state_dir: false,
        harness: None,
        harness_bin: None,
    }
}

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
    eventually("the port was never released", || async move {
        TcpListener::bind(("127.0.0.1", port)).await.ok()
    })
    .await
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
    tokio::spawn(commandant_worker::run(worker_config(
        addr, join_token, state_dir,
    )))
}

async fn wait_for_node(client: &mut ControlClient, online: bool) -> NodeInfo {
    wait_for_named(client, "w1", online).await
}

async fn wait_for_named(client: &mut ControlClient, name: &str, online: bool) -> NodeInfo {
    let what = format!("node {name} never became online={online}");
    eventually(&what, || {
        let mut client = client.clone();
        async move {
            let nodes = client.list_nodes(ListNodesRequest {}).await.unwrap();
            let mut nodes = nodes.into_inner().nodes.into_iter();
            nodes.find(|n| n.name == name && n.online == online)
        }
    })
    .await
}

/// The credentials a worker saved. It saves them after the orchestrator
/// already lists it as online, so they may take a moment.
async fn saved_credentials(state_dir: &Path) -> commandant_worker::state::Credentials {
    eventually("the worker never saved its credentials", || async {
        commandant_worker::state::load(state_dir).unwrap()
    })
    .await
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
    started(client.run_command(request).await.unwrap().into_inner()).await
}

/// The task's id, from the event that opens its stream.
async fn started(mut events: tonic::Streaming<TaskEvent>) -> (String, tonic::Streaming<TaskEvent>) {
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
    let err = client
        .list_agent_sessions(ListAgentSessionsRequest { node: "w1".into() })
        .await
        .unwrap_err();
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
            name: Some("w2".into()),
            ..worker_config(&addr, Some(join), other.path())
        }),
    )
    .await
    .expect("a rejected join should fail fast");
    assert!(res.unwrap_err().to_string().contains("join token"));

    worker.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_link_with_several_addresses_uses_the_one_that_answers() {
    let tmp = tempfile::tempdir().unwrap();
    let (addr, admin_token, _stop) = start_orchestrator(&tmp.path().join("server")).await;
    // First an address nobody answers on (TEST-NET-1), as a public IP seen
    // from inside the network might be.
    let hosts = format!("192.0.2.1:7400,{}", addr.trim_start_matches("http://"));
    let link: Link = Link::new(&admin_token, &hosts).to_string().parse().unwrap();
    assert_eq!(link.hosts.len(), 2);

    let mut client = connect_control(&link.addr(), &admin_token).await.unwrap();
    let state_dir = tmp.path().join("worker");
    let worker = spawn_worker(&link.addr(), Some(link.token.clone()), &state_dir);
    wait_for_node(&mut client, true).await;
    // Both are remembered, for when the other one is the one that works.
    let creds = saved_credentials(&state_dir).await;
    assert_eq!(creds.server.as_deref(), Some(link.addr().as_str()));
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
        let new_token = eventually("the reset server never wrote a new token", || async {
            let token = std::fs::read_to_string(&token_file).ok()?;
            let token = token.trim();
            (!token.is_empty() && token != old_token).then(|| token.to_string())
        })
        .await;
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
        assert_eq!(link.hosts[0], format!("box.lan:{port}"));

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

/// A worker whose saved credentials the orchestrator doesn't know (say it
/// was reset) joins again with the token it was given, but only when it
/// starts: a node removed while running stays removed.
#[tokio::test(flavor = "multi_thread")]
async fn stale_credentials_give_way_to_a_fresh_token() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("worker");
    let (old_addr, old_token, _old_stop) = start_orchestrator(&tmp.path().join("old")).await;
    let mut old = connect_control(&old_addr, &old_token).await.unwrap();
    let worker = spawn_worker(&old_addr, Some(old_token), &state_dir);
    wait_for_node(&mut old, true).await;
    let stale = saved_credentials(&state_dir).await;
    // Gone for good, its state directory free.
    worker.abort();
    let _ = worker.await;

    // A fresh orchestrator: without a token, the stale credentials are fatal.
    let (addr, token, _stop) = start_orchestrator(&tmp.path().join("new")).await;
    let mut client = connect_control(&addr, &token).await.unwrap();
    let err = tokio::time::timeout(WAIT, spawn_worker(&addr, None, &state_dir))
        .await
        .expect("an unknown node should fail fast")
        .unwrap()
        .unwrap_err();
    assert!(format!("{err:#}").contains("unknown node"), "{err:#}");

    // With one, it joins afresh and keeps the new credentials.
    let worker = spawn_worker(&addr, Some(token.clone()), &state_dir);
    let node = wait_for_node(&mut client, true).await;
    let fresh = eventually("the new credentials were never saved", || async {
        let creds = commandant_worker::state::load(&state_dir).unwrap()?;
        (creds.node_id != stale.node_id).then_some(creds)
    })
    .await;
    assert_eq!(fresh.node_id, node.id);

    // A second worker can't take its name, and is told what to do.
    let other = tempfile::tempdir().unwrap();
    let err = tokio::time::timeout(WAIT, spawn_worker(&addr, Some(token), other.path()))
        .await
        .expect("a taken name should fail fast")
        .unwrap()
        .unwrap_err();
    assert!(format!("{err:#}").contains("another --name"), "{err:#}");

    // Removed while running, it stops rather than joining again.
    client
        .remove_node(RemoveNodeRequest { node: "w1".into() })
        .await
        .unwrap();
    let stopped = tokio::time::timeout(WAIT, worker)
        .await
        .expect("a removed node should stop")
        .unwrap();
    let err = stopped.unwrap_err();
    assert!(
        format!("{err:#}").contains("this node was removed"),
        "{err:#}"
    );
    let nodes = client
        .list_nodes(ListNodesRequest {})
        .await
        .unwrap()
        .into_inner()
        .nodes;
    assert!(nodes.is_empty(), "it didn't come back");
}

/// Having joined afresh, a worker reconnects as its new node from then on:
/// its old credentials are gone, not merely set aside for one connection.
#[tokio::test(flavor = "multi_thread")]
async fn a_rejoined_worker_reconnects_as_its_new_node() {
    let tmp = tempfile::tempdir().unwrap();
    let state_dir = tmp.path().join("worker");
    let (old_addr, old_token, _old_stop) = start_orchestrator(&tmp.path().join("old")).await;
    let mut old = connect_control(&old_addr, &old_token).await.unwrap();
    let worker = spawn_worker(&old_addr, Some(old_token), &state_dir);
    wait_for_node(&mut old, true).await;
    let stale = saved_credentials(&state_dir).await;
    // Gone for good, its state directory free.
    worker.abort();
    let _ = worker.await;

    let data_dir = tmp.path().join("new");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = serve_on(&data_dir, listener).await;
    let (addr, token) = (server.addr.clone(), server.token.clone());
    let mut client = connect_control(&addr, &token).await.unwrap();
    let _worker = spawn_worker(&addr, Some(token.clone()), &state_dir);
    let node = wait_for_node(&mut client, true).await;
    assert_ne!(node.id, stale.node_id);

    // The orchestrator restarts; the worker comes back as the same node.
    server.stop().await;
    let server = serve_on(&data_dir, rebind(port).await).await;
    let mut client = connect_control(&server.addr, &server.token).await.unwrap();
    let back = wait_for_node(&mut client, true).await;
    assert_eq!(back.id, node.id);
    let nodes = client
        .list_nodes(ListNodesRequest {})
        .await
        .unwrap()
        .into_inner()
        .nodes;
    assert_eq!(nodes.len(), 1);
}

/// Workers started together on one machine, from the default state
/// directory, become separate nodes and stay connected side by side.
#[tokio::test(flavor = "multi_thread")]
async fn workers_on_one_machine_are_separate_nodes() {
    const WORKERS: usize = 4;
    let tmp = tempfile::tempdir().unwrap();
    let (addr, token, _stop) = start_orchestrator(&tmp.path().join("server")).await;
    let mut client = connect_control(&addr, &token).await.unwrap();
    let state_dir = tmp.path().join("worker");
    let workers: Vec<_> = (0..WORKERS)
        .map(|_| {
            tokio::spawn(commandant_worker::run(WorkerConfig {
                name: None,
                pick_free_state_dir: true,
                ..worker_config(&addr, Some(token.clone()), &state_dir)
            }))
        })
        .collect();

    let online = |client: &mut ControlClient| {
        let mut client = client.clone();
        async move {
            let nodes = client.list_nodes(ListNodesRequest {}).await.unwrap();
            let mut online: Vec<_> = nodes
                .into_inner()
                .nodes
                .into_iter()
                .filter(|n| n.online)
                .collect();
            online.sort_by(|a, b| a.name.cmp(&b.name));
            online
        }
    };
    let nodes = eventually("every worker should come online", || {
        let online = online(&mut client.clone());
        async move {
            let nodes = online.await;
            (nodes.len() == WORKERS).then_some(nodes)
        }
    })
    .await;
    // Each its own node, name and state directory.
    let mut ids: Vec<_> = nodes.iter().map(|n| n.id.clone()).collect();
    ids.dedup();
    assert_eq!(ids.len(), WORKERS);
    let host = &nodes[0].name;
    let names: Vec<_> = nodes.iter().map(|n| n.name.as_str()).collect();
    let expected: Vec<String> = std::iter::once(host.clone())
        .chain((2..=WORKERS).map(|n| format!("{host}-{n}")))
        .collect();
    assert_eq!(names, expected);
    for n in 2..=WORKERS {
        saved_credentials(&tmp.path().join(format!("worker-{n}"))).await;
    }

    // They stay connected rather than taking turns.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let still: Vec<_> = online(&mut client)
        .await
        .into_iter()
        .map(|n| n.id)
        .collect();
    assert_eq!(still.len(), WORKERS);
    assert!(workers.iter().all(|w| !w.is_finished()));

    // A worker told to use a directory another one holds is refused.
    let err = tokio::time::timeout(
        WAIT,
        commandant_worker::run(WorkerConfig {
            name: Some("intruder".into()),
            ..worker_config(&addr, Some(token.clone()), &state_dir)
        }),
    )
    .await
    .expect("a held directory should fail fast")
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("its own --state-dir"),
        "{err:#}"
    );

    // Once they stop, the next worker takes the first free directory back,
    // and is the same node again.
    for worker in workers {
        worker.abort();
        let _ = worker.await;
    }
    for name in &names {
        wait_for_named(&mut client, name, false).await;
    }
    let _back = tokio::spawn(commandant_worker::run(WorkerConfig {
        server: None,
        name: None,
        pick_free_state_dir: true,
        ..worker_config(&addr, None, &state_dir)
    }));
    wait_for_named(&mut client, host, true).await;
}

/// Credentials copied to a second worker don't make two workers take turns
/// as one node: the one that connected first is told why and stops.
#[tokio::test(flavor = "multi_thread")]
async fn copied_credentials_stop_the_older_worker() {
    let tmp = tempfile::tempdir().unwrap();
    let (addr, token, _stop) = start_orchestrator(&tmp.path().join("server")).await;
    let mut client = connect_control(&addr, &token).await.unwrap();
    let original = tmp.path().join("original");
    let first = spawn_worker(&addr, Some(token), &original);
    let node = wait_for_node(&mut client, true).await;
    saved_credentials(&original).await;

    let copy = tmp.path().join("copy");
    std::fs::create_dir_all(&copy).unwrap();
    std::fs::copy(original.join("node.json"), copy.join("node.json")).unwrap();
    let _second = spawn_worker(&addr, None, &copy);

    let stopped = tokio::time::timeout(WAIT, first)
        .await
        .expect("the older worker should stop")
        .unwrap()
        .unwrap_err();
    assert!(
        format!("{stopped:#}").contains("same credentials"),
        "{stopped:#}"
    );
    let still = wait_for_node(&mut client, true).await;
    assert_eq!(still.id, node.id);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(wait_for_node(&mut client, true).await.online);
}

#[cfg(unix)]
mod fake_opencode;

/// A worker hosting a fake OpenCode, driven like the CLI and the TUI drive a
/// real one.
#[cfg(unix)]
mod agents {
    use commandant_worker::HarnessKind;

    use super::fake_opencode::FakeOpencode;
    use super::*;

    struct Cluster {
        client: ControlClient,
        fake: FakeOpencode,
        worker: JoinHandle<anyhow::Result<()>>,
        _stop: oneshot::Sender<()>,
        _tmp: tempfile::TempDir,
    }

    async fn cluster() -> Cluster {
        cluster_with(FakeOpencode::start().await).await
    }

    async fn cluster_with(fake: FakeOpencode) -> Cluster {
        let tmp = tempfile::tempdir().unwrap();
        let (addr, admin_token, stop) = start_orchestrator(&tmp.path().join("server")).await;
        let worker = tokio::spawn(commandant_worker::run(WorkerConfig {
            harness: Some(HarnessKind::Opencode),
            harness_bin: Some(fake.binary.clone()),
            ..worker_config(&addr, Some(admin_token.clone()), &tmp.path().join("worker"))
        }));
        let mut client = connect_control(&addr, &admin_token).await.unwrap();
        let node = wait_for_node(&mut client, true).await;
        assert_eq!(node.harnesses, ["opencode"]);
        Cluster {
            client,
            fake,
            worker,
            _stop: stop,
            _tmp: tmp,
        }
    }

    fn ask(text: &str) -> PromptRequest {
        PromptRequest {
            node: "w1".into(),
            prompt: text.into(),
            ..Default::default()
        }
    }

    async fn get_options(client: &mut ControlClient) -> AgentOptions {
        let request = GetAgentOptionsRequest { node: "w1".into() };
        client
            .get_agent_options(request)
            .await
            .unwrap()
            .into_inner()
    }

    async fn list_sessions(client: &mut ControlClient) -> Vec<AgentSession> {
        let request = ListAgentSessionsRequest { node: "w1".into() };
        let sessions = client.list_agent_sessions(request).await.unwrap();
        sessions.into_inner().sessions
    }

    fn resume(session_id: &str, text: &str) -> PromptRequest {
        PromptRequest {
            session_id: session_id.into(),
            ..ask(text)
        }
    }

    async fn start(
        client: &mut ControlClient,
        request: PromptRequest,
    ) -> (String, tonic::Streaming<TaskEvent>) {
        super::started(client.prompt(request).await.unwrap().into_inner()).await
    }

    async fn prompt(client: &mut ControlClient, request: PromptRequest) -> TaskResult {
        let (_, events) = start(client, request).await;
        collect(events).await
    }

    fn ok(result: &TaskResult) {
        let finished = &result.finished;
        assert_eq!(
            (finished.exit_code, finished.error.as_str()),
            (Some(0), ""),
            "stderr: {}",
            result.stderr
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prompts_run_side_by_side_without_mixing() {
        let Cluster {
            mut client,
            fake,
            _stop,
            _tmp,
            ..
        } = cluster().await;
        let (_, one) = start(&mut client, ask("hold one")).await;
        let (_, two) = start(&mut client, ask("hold two")).await;
        // Both turns are under way at once, each in its own session.
        let held = fake.wait_held(2).await;
        assert_ne!(held[0], held[1]);
        for session in &held {
            assert!(fake.release(session));
        }
        let (one, two) = tokio::join!(collect(one), collect(two));
        ok(&one);
        ok(&two);
        assert_eq!(one.stdout.trim(), "echo: hold one");
        assert_eq!(two.stdout.trim(), "echo: hold two");
        assert_ne!(one.finished.session_id, two.finished.session_id);
        assert_eq!(one.finished.model, "fake/echo");
        let usage = one.finished.usage.unwrap();
        assert_eq!((usage.input, usage.output, usage.cost), (10, 3, 0.01));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_session_answers_one_prompt_at_a_time() {
        let Cluster {
            mut client,
            fake,
            _stop,
            _tmp,
            ..
        } = cluster().await;
        let (_, first) = start(&mut client, ask("hold on")).await;
        let session = fake.wait_held(1).await.remove(0);

        // A second prompt in that session is refused; other sessions carry on.
        let refused = prompt(&mut client, resume(&session, "me too")).await;
        assert_eq!(refused.finished.exit_code, None);
        assert!(
            refused.finished.error.contains("already answering"),
            "{}",
            refused.finished.error
        );
        let elsewhere = prompt(&mut client, ask("meanwhile")).await;
        ok(&elsewhere);
        assert!(fake.holding(&session), "the refusal left the turn alone");

        fake.release(&session);
        ok(&collect(first).await);
        // Once it's done, the session takes the next prompt.
        let next = prompt(&mut client, resume(&session, "next")).await;
        ok(&next);
        assert_eq!(next.finished.session_id, session);
        assert_eq!(next.stdout.trim(), "echo: next");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancelling_aborts_the_turn_and_frees_the_session() {
        let Cluster {
            mut client,
            fake,
            _stop,
            _tmp,
            ..
        } = cluster().await;
        let (task_id, events) = start(&mut client, ask("hold it")).await;
        let session = fake.wait_held(1).await.remove(0);
        client
            .cancel_task(CancelTaskRequest {
                task_id: task_id.clone(),
            })
            .await
            .unwrap();
        let cancelled = collect(events).await;
        assert!(cancelled.finished.cancelled);
        assert_eq!(cancelled.finished.session_id, session);
        assert!(
            fake.log()
                .contains(&format!("POST /session/{session}/abort"))
        );

        // It is over: cancelling again finds nothing, and the session is free.
        let again = client
            .cancel_task(CancelTaskRequest { task_id })
            .await
            .unwrap_err();
        assert_eq!(again.code(), tonic::Code::NotFound);
        ok(&prompt(&mut client, resume(&session, "after")).await);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn saved_sessions_are_listed_latest_first_with_busy_ones_marked() {
        let Cluster {
            mut client,
            fake,
            _stop,
            _tmp,
            ..
        } = cluster().await;
        assert!(list_sessions(&mut client).await.is_empty());

        let done = prompt(&mut client, ask("one")).await;
        let (_, held) = start(&mut client, ask("hold two")).await;
        let busy = fake.wait_held(1).await.remove(0);
        let sessions = list_sessions(&mut client).await;
        let ids: Vec<_> = sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, [busy.as_str(), done.finished.session_id.as_str()]);
        assert_eq!(
            sessions.iter().map(|s| s.busy).collect::<Vec<_>>(),
            [true, false]
        );
        assert_eq!(
            (sessions[0].agent.as_str(), sessions[0].model.as_str()),
            ("build", "fake/echo")
        );
        assert!(!sessions[0].directory.is_empty());

        fake.release(&busy);
        ok(&collect(held).await);
        assert!(list_sessions(&mut client).await.iter().all(|s| !s.busy));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_saved_session_shows_what_was_said() {
        let Cluster {
            mut client,
            _stop,
            _tmp,
            ..
        } = cluster().await;
        let done = prompt(&mut client, ask("one")).await;
        let request = GetSessionHistoryRequest {
            node: "w1".into(),
            session_id: done.finished.session_id.clone(),
        };
        let history = client.get_session_history(request).await.unwrap();
        let entries: Vec<_> = history
            .into_inner()
            .entries
            .into_iter()
            .map(|e| (e.role, e.text))
            .collect();
        assert_eq!(
            entries,
            [("user", "one"), ("agent", "echo: one")].map(|(r, t)| (r.into(), t.into()))
        );
    }

    async fn providers(client: &mut ControlClient) -> Vec<ModelProvider> {
        let request = ListProvidersRequest { node: "w1".into() };
        client
            .list_providers(request)
            .await
            .unwrap()
            .into_inner()
            .providers
    }

    async fn authenticate(
        client: &mut ControlClient,
        provider: &str,
        action: auth_action::Action,
    ) -> Result<ProviderAuthResult, tonic::Status> {
        let request = AuthenticateProviderRequest {
            node: "w1".into(),
            provider: provider.into(),
            action: Some(AuthAction {
                action: Some(action),
            }),
        };
        client
            .authenticate_provider(request)
            .await
            .map(tonic::Response::into_inner)
    }

    fn reloads(fake: &FakeOpencode) -> usize {
        fake.log()
            .iter()
            .filter(|l| *l == "POST /global/dispose")
            .count()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn providers_are_signed_in_to_without_disturbing_running_prompts() {
        let Cluster {
            mut client,
            fake,
            _stop,
            _tmp,
            ..
        } = cluster().await;
        let listed = providers(&mut client).await;
        let other = listed.iter().find(|p| p.id == "other").unwrap();
        assert!(!other.connected);
        assert_eq!(other.methods.len(), 3);
        assert_eq!(
            listed.iter().find(|p| p.id == "fake").unwrap().methods[0].label,
            "API key"
        );

        // Signed in while a prompt runs: OpenCode reloads only once it's done.
        let (_, held) = start(&mut client, ask("hold on")).await;
        let session = fake.wait_held(1).await.remove(0);
        authenticate(
            &mut client,
            "fake",
            auth_action::Action::ApiKey(" sk-1 ".into()),
        )
        .await
        .unwrap();
        assert!(fake.log().iter().any(|l| l.contains(r#""key":"sk-1""#)));
        get_options(&mut client).await;
        assert_eq!(reloads(&fake), 0, "the running prompt would be aborted");
        fake.release(&session);
        ok(&collect(held).await);
        get_options(&mut client).await;
        assert_eq!(reloads(&fake), 1);
        get_options(&mut client).await;
        assert_eq!(reloads(&fake), 1, "once is enough");

        // OAuth with a code to paste back: a wrong one fails, the right one works.
        let started = authenticate(&mut client, "other", auth_action::Action::OauthStart(1))
            .await
            .unwrap();
        assert!(started.needs_code && started.url.contains("/other/"));
        let finish = |code: &str| {
            auth_action::Action::OauthFinish(OauthFinish {
                index: 1,
                code: code.into(),
            })
        };
        assert!(
            authenticate(&mut client, "other", finish("bad"))
                .await
                .is_err()
        );
        authenticate(&mut client, "other", finish("good"))
            .await
            .unwrap();
        let listed = providers(&mut client).await;
        assert_eq!(
            listed
                .iter()
                .filter(|p| p.connected)
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            ["fake", "other"],
            "signed-in ones first"
        );

        authenticate(&mut client, "other", auth_action::Action::SignOut(true))
            .await
            .unwrap();
        assert!(
            !providers(&mut client)
                .await
                .iter()
                .any(|p| p.id == "other" && p.connected)
        );
        let empty = authenticate(
            &mut client,
            "fake",
            auth_action::Action::ApiKey("  ".into()),
        );
        assert!(empty.await.is_err());
    }

    /// A stand-in for `claude`: it answers `initialize` and `mcp_status`,
    /// echoes a prompt as a stream-json turn, and logs how it was run.
    const FAKE_CLAUDE: &str = r#"#!/bin/sh
log="$(dirname "$0")/claude.log"
case "$1" in
  --version) echo "9.9.9 (Claude Code)"; exit 0 ;;
  auth) echo '{"loggedIn":true,"authMethod":"claude.ai"}'; exit 0 ;;
esac
echo "args: $* token: ${CLAUDE_CODE_OAUTH_TOKEN:-none}" >> "$log"
case " $* " in *" --input-format "*)
  while read -r line; do
    case "$line" in
      *initialize*) echo '{"type":"control_response","response":{"subtype":"success","request_id":"init","response":{"agents":[{"name":"Explore","description":"Searches"}],"commands":[{"name":"review","description":"Review"}],"models":[{"value":"default"},{"value":"sonnet","displayName":"Sonnet","supportedEffortLevels":["low","high"]}]}}}' ;;
      *mcp_status*) echo '{"type":"control_response","response":{"subtype":"success","request_id":"mcp","response":{"mcpServers":[{"name":"docs","status":"connected"}]}}}'; exit 0 ;;
    esac
  done
  exit 0 ;;
esac
prompt=$(cat)
echo "prompt: $prompt" >> "$log"
echo '{"type":"system","subtype":"init","apiKeySource":"none","model":"claude-fake"}'
echo '{"type":"stream_event","event":{"type":"content_block_start"}}'
printf '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"echo: %s"}}}\n' "$prompt"
echo '{"type":"result","is_error":false,"total_cost_usd":1.5,"usage":{"input_tokens":3,"output_tokens":2}}'
"#;

    #[tokio::test(flavor = "multi_thread")]
    async fn claude_code_runs_on_the_subscription_through_its_own_cli() {
        let tmp = tempfile::tempdir().unwrap();
        let binary = tmp.path().join("claude");
        super::fake_opencode::write_script(&binary, FAKE_CLAUDE);
        let (addr, admin_token, _stop) = start_orchestrator(&tmp.path().join("server")).await;
        let worker = tokio::spawn(commandant_worker::run(WorkerConfig {
            harness: Some(HarnessKind::ClaudeCode),
            harness_bin: Some(binary),
            ..worker_config(&addr, Some(admin_token.clone()), &tmp.path().join("worker"))
        }));
        let mut client = connect_control(&addr, &admin_token).await.unwrap();
        let node = wait_for_node(&mut client, true).await;
        assert_eq!(node.harnesses, ["claude-code"]);
        let log = || std::fs::read_to_string(tmp.path().join("claude.log")).unwrap_or_default();

        let options = get_options(&mut client).await;
        assert_eq!(options.agents[0].name, "Explore");
        assert_eq!(options.models.len(), 1, "its default is no model at all");
        assert_eq!(options.models[0].variants, ["low", "high"]);
        assert_eq!(options.commands[0].name, "review");
        assert_eq!(options.mcp_servers[0].status, "connected");

        let request = PromptRequest {
            model: "sonnet".into(),
            variant: "high".into(),
            ..ask("hi there")
        };
        let done = prompt(&mut client, request).await;
        ok(&done);
        assert_eq!(done.stdout, "echo: hi there\n");
        assert_eq!(done.finished.model, "claude-fake");
        assert_eq!(
            done.finished.usage.unwrap().cost,
            0.0,
            "the subscription pays"
        );
        let session = done.finished.session_id.clone();
        assert_eq!(session.len(), 36, "a UUID it was started with");
        assert!(
            log().contains(&format!(
                "--session-id {session} --model sonnet --effort high"
            )),
            "{}",
            log()
        );

        // A token from `claude setup-token` signs it in from then on.
        let providers = client
            .list_providers(ListProvidersRequest { node: "w1".into() })
            .await
            .unwrap()
            .into_inner()
            .providers;
        assert!(providers[0].connected);
        let request = AuthenticateProviderRequest {
            node: "w1".into(),
            provider: "claude".into(),
            action: Some(AuthAction {
                action: Some(auth_action::Action::ApiKey(" sk-ant-oat-x ".into())),
            }),
        };
        client.authenticate_provider(request).await.unwrap();
        let again = PromptRequest {
            session_id: session.clone(),
            command: "review".into(),
            cwd: tmp.path().to_string_lossy().into(),
            ..ask("the parser")
        };
        ok(&prompt(&mut client, again).await);
        let log = log();
        assert!(log.contains(&format!("--resume {session}")), "{log}");
        assert!(log.contains("token: sk-ant-oat-x"), "{log}");
        assert!(log.contains("prompt: /review the parser"), "{log}");
        worker.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn older_opencode_lists_its_sessions_too() {
        let Cluster {
            mut client,
            fake,
            _stop,
            _tmp,
            ..
        } = cluster_with(FakeOpencode::start_legacy().await).await;
        let done = prompt(&mut client, ask("one")).await;
        let sessions = list_sessions(&mut client).await;
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, done.finished.session_id);
        // It asks the newer way once, then goes straight to the older one.
        assert_eq!(list_sessions(&mut client).await.len(), 1);
        let tries = |path: &str| fake.log().iter().filter(|l| *l == path).count();
        assert_eq!(tries("GET /experimental/session"), 1);
        assert_eq!(tries("GET /session"), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn commands_are_checked_then_run_with_their_arguments() {
        let Cluster {
            mut client,
            fake,
            _stop,
            _tmp,
            ..
        } = cluster().await;
        let command = |name: &str, arguments: &str| PromptRequest {
            command: name.into(),
            ..ask(arguments)
        };

        let review = prompt(&mut client, command("review", "the parser")).await;
        ok(&review);
        assert_eq!(review.stdout.trim(), "echo: /review the parser");
        let sent = fake
            .log()
            .into_iter()
            .find(|l| l.starts_with("command "))
            .unwrap();
        assert!(sent.contains(r#""arguments":"the parser""#), "{sent}");

        // Arguments are optional.
        ok(&prompt(&mut client, command("pdf", "")).await);

        // An unknown one is refused before OpenCode hears of it.
        let before = fake
            .log()
            .iter()
            .filter(|l| l.starts_with("POST") && l.ends_with("/command"))
            .count();
        let unknown = prompt(&mut client, command("nope", "x")).await;
        assert!(
            unknown.finished.error.contains("no /nope command"),
            "{}",
            unknown.finished.error
        );
        let after = fake
            .log()
            .iter()
            .filter(|l| l.starts_with("POST") && l.ends_with("/command"))
            .count();
        assert_eq!(before, after);

        // Neither a prompt nor a command: nothing to send.
        let err = client.prompt(ask("  ")).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn options_list_what_the_agent_offers_and_switch_mcp_servers() {
        let Cluster {
            mut client,
            _stop,
            _tmp,
            ..
        } = cluster().await;
        let options = get_options(&mut client).await;
        let agents: Vec<_> = options.agents.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(agents, ["build", "plan"], "no subagents");
        assert_eq!(
            (
                options.default_agent.as_str(),
                options.default_model.as_str()
            ),
            ("build", "fake/echo")
        );
        assert_eq!(options.models.len(), 1, "no deprecated models");
        assert_eq!(options.models[0].variants, ["low", "high"]);
        assert_eq!(options.models[0].context, 1000);
        let commands: Vec<_> = options
            .commands
            .iter()
            .map(|c| (c.name.as_str(), c.source.as_str()))
            .collect();
        assert_eq!(commands, [("review", "command"), ("pdf", "skill")]);
        let servers: Vec<_> = options
            .mcp_servers
            .iter()
            .map(|m| (m.name.as_str(), m.status.as_str(), m.error.as_str()))
            .collect();
        assert_eq!(
            servers,
            [
                ("broken", "failed", "connection refused"),
                ("docs", "disabled", "")
            ]
        );

        let switch = |name: &str, connect| {
            let mut client = client.clone();
            let request = SwitchMcpServerRequest {
                node: "w1".into(),
                name: name.into(),
                connect,
            };
            async move { client.switch_mcp_server(request).await }
        };
        let status = |options: AgentOptions, name: &str| {
            let server = options.mcp_servers.into_iter().find(|m| m.name == name);
            server.unwrap().status
        };
        let connected = switch("docs", true).await.unwrap().into_inner();
        assert_eq!(status(connected, "docs"), "connected");
        let disconnected = switch("docs", false).await.unwrap().into_inner();
        assert_eq!(status(disconnected, "docs"), "disabled");

        let unknown = switch("nope", true).await.unwrap_err();
        assert_eq!(unknown.code(), tonic::Code::Unavailable);
        assert!(
            unknown.message().contains("no MCP server named nope"),
            "{}",
            unknown.message()
        );
        let unnamed = switch("", true).await.unwrap_err();
        assert_eq!(unnamed.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn turns_that_fail_or_need_permission() {
        let Cluster {
            mut client,
            fake,
            _stop,
            _tmp,
            ..
        } = cluster().await;
        let failed = prompt(&mut client, ask("fail please")).await;
        assert_eq!(failed.finished.exit_code, Some(1));
        assert_eq!(failed.finished.error, "APIError: boom");

        // Nobody is there to answer, so the worker allows it once and says so.
        let asked = prompt(&mut client, ask("permission to build")).await;
        ok(&asked);
        assert!(
            asked.stderr.contains("allowed bash make"),
            "{}",
            asked.stderr
        );
        assert!(fake.log().contains(&"permission once".to_string()));

        // The choices reach OpenCode; a malformed model never leaves the worker.
        let chosen = PromptRequest {
            model: "fake/echo".into(),
            agent: "plan".into(),
            variant: "high".into(),
            ..ask("with choices")
        };
        ok(&prompt(&mut client, chosen).await);
        let sent = fake
            .log()
            .into_iter()
            .rfind(|l| l.starts_with("prompt "))
            .unwrap();
        for part in [
            r#""agent":"plan""#,
            r#""variant":"high""#,
            r#""providerID":"fake""#,
            r#""modelID":"echo""#,
        ] {
            assert!(sent.contains(part), "{part} missing from {sent}");
        }
        let bad = PromptRequest {
            model: "echo".into(),
            ..ask("bad model")
        };
        let bad = prompt(&mut client, bad).await;
        assert!(
            bad.finished.error.contains("provider/model"),
            "{}",
            bad.finished.error
        );

        // A session the agent doesn't know.
        let missing = prompt(&mut client, resume("ses_missing", "hello?")).await;
        assert!(
            missing.finished.error.contains("404"),
            "{}",
            missing.finished.error
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_worker_that_stops_aborts_its_turns() {
        let Cluster {
            mut client,
            fake,
            worker,
            _stop,
            _tmp,
        } = cluster().await;
        let (_, one) = start(&mut client, ask("hold one")).await;
        let (_, two) = start(&mut client, ask("hold two")).await;
        let held = fake.wait_held(2).await;
        worker.abort();
        // Whether its last words arrive or not, both turns end...
        let (one, two) = tokio::join!(collect(one), collect(two));
        for ended in [one, two] {
            let finished = ended.finished;
            assert!(
                finished.cancelled || finished.error == "node disconnected",
                "{finished:?}"
            );
        }
        // ...and the agent isn't left working for nobody.
        let log = fake.log();
        for session in &held {
            assert!(log.contains(&format!("POST /session/{session}/abort")));
        }
        wait_for_node(&mut client, false).await;
        let err = client
            .list_agent_sessions(ListAgentSessionsRequest { node: "w1".into() })
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unavailable);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_node_starts_the_agent_it_is_asked_for() {
        let tmp = tempfile::tempdir().unwrap();
        let (addr, token, _stop) = start_orchestrator(&tmp.path().join("server")).await;
        let fake = FakeOpencode::start().await;
        let worker = |token: Option<String>| {
            tokio::spawn(commandant_worker::run(WorkerConfig {
                harness_bin: Some(fake.binary.clone()),
                ..worker_config(&addr, token, &tmp.path().join("worker"))
            }))
        };
        let first = worker(Some(token.clone()));
        let mut client = connect_control(&addr, &token).await.unwrap();
        let node = wait_for_node(&mut client, true).await;
        assert!(node.harnesses.is_empty());
        assert_eq!(node.can_host, ["opencode", "claude-code"]);
        let starter = client.clone();
        let start = |harness: &str| {
            let mut client = starter.clone();
            let request = StartHarnessRequest {
                node: "w1".into(),
                harness: harness.into(),
            };
            async move { client.start_harness(request).await }
        };

        // Nothing to prompt yet, and only what it can host can be started.
        let err = client.prompt(ask("hi")).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
        assert!(err.message().contains("start-agent"), "{}", err.message());
        let unknown = start("claude").await.unwrap_err();
        assert_eq!(unknown.code(), tonic::Code::InvalidArgument);
        assert!(
            unknown.message().contains("it can host opencode"),
            "{}",
            unknown.message()
        );
        assert_eq!(
            start("").await.unwrap_err().code(),
            tonic::Code::InvalidArgument
        );

        let started = start("opencode").await.unwrap().into_inner();
        assert_eq!(started.harnesses, ["opencode"]);
        let listed = wait_for_node(&mut client, true).await;
        assert_eq!(listed.harnesses, ["opencode"]);
        ok(&prompt(&mut client, ask("hello")).await);
        // Starting it again is a no-op.
        assert_eq!(
            start("opencode").await.unwrap().into_inner().harnesses,
            ["opencode"]
        );

        // Restarted with no --harness, it hosts none until asked again.
        first.abort();
        wait_for_node(&mut client, false).await;
        let _second = worker(None);
        let back = wait_for_node(&mut client, true).await;
        assert!(back.harnesses.is_empty());
        assert_eq!(
            start("opencode").await.unwrap().into_inner().harnesses,
            ["opencode"]
        );
        ok(&prompt(&mut client, ask("again")).await);
    }

    /// OpenCode lists its commands only once its MCP servers have connected;
    /// the models and agents don't wait for that.
    #[tokio::test(flavor = "multi_thread")]
    async fn options_dont_wait_for_mcp_servers_to_connect() {
        let Cluster {
            mut client,
            fake,
            _stop,
            _tmp,
            ..
        } = cluster().await;
        // As long as a real one took, and longer than a question may.
        fake.delay_mcp(Duration::from_secs(20));
        let asked = std::time::Instant::now();
        let options = get_options(&mut client).await;
        assert!(
            asked.elapsed() < Duration::from_secs(5),
            "{:?}",
            asked.elapsed()
        );
        assert!(options.loading);
        assert!(options.commands.is_empty());
        assert!(options.mcp_servers.is_empty());
        assert_eq!(options.models.len(), 1);
        assert_eq!(options.default_agent, "build");

        fake.delay_mcp(Duration::ZERO);
        let options = get_options(&mut client).await;
        assert!(!options.loading);
        assert_eq!(options.commands.len(), 2);
        assert_eq!(options.mcp_servers.len(), 2);
    }
}
