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
        opencode_bin: None,
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
            server: addr.clone(),
            join_token: Some(join),
            name: Some("w2".into()),
            state_dir: other.path().to_path_buf(),
            harness: None,
            opencode_bin: None,
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
    worker.abort();

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
    let fresh = tokio::time::timeout(WAIT, async {
        loop {
            let creds = commandant_worker::state::load(&state_dir).unwrap().unwrap();
            if creds.node_id != stale.node_id {
                return creds;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the new credentials were never saved");
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
            server: addr.clone(),
            join_token: Some(admin_token.clone()),
            name: Some("w1".into()),
            state_dir: tmp.path().join("worker"),
            harness: Some(HarnessKind::Opencode),
            opencode_bin: Some(fake.binary.clone()),
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
        let mut events = client.prompt(request).await.unwrap().into_inner();
        let Some(TaskEvent {
            event: Some(task_event::Event::Started(started)),
        }) = events.message().await.unwrap()
        else {
            panic!("first event must be Started");
        };
        (started.task_id, events)
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
        let list = |client: &mut ControlClient| {
            let mut client = client.clone();
            async move {
                client
                    .list_agent_sessions(ListAgentSessionsRequest { node: "w1".into() })
                    .await
                    .unwrap()
                    .into_inner()
                    .sessions
            }
        };
        assert!(list(&mut client).await.is_empty());

        let done = prompt(&mut client, ask("one")).await;
        let (_, held) = start(&mut client, ask("hold two")).await;
        let busy = fake.wait_held(1).await.remove(0);
        let sessions = list(&mut client).await;
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
        assert!(list(&mut client).await.iter().all(|s| !s.busy));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn older_opencode_lists_its_sessions_too() {
        let Cluster {
            mut client,
            _stop,
            _tmp,
            ..
        } = cluster_with(FakeOpencode::start_legacy().await).await;
        let done = prompt(&mut client, ask("one")).await;
        let sessions = client
            .list_agent_sessions(ListAgentSessionsRequest { node: "w1".into() })
            .await
            .unwrap()
            .into_inner()
            .sessions;
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, done.finished.session_id);
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
        let options = client
            .get_agent_options(GetAgentOptionsRequest { node: "w1".into() })
            .await
            .unwrap()
            .into_inner();
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
}
