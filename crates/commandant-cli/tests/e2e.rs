//! Runs an orchestrator and a worker in-process and drives them through the
//! Control API, like the CLI does.

use std::path::Path;
use std::time::Duration;

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
    let orchestrator = Orchestrator::open(data_dir).await.unwrap();
    let token = orchestrator.admin_token().to_string();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    tokio::spawn(orchestrator.serve(listener, async {
        let _ = stop_rx.await;
    }));
    (addr, token, stop_tx)
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
                .find(|n| n.name == "w1" && n.online == online)
            {
                return node;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("node never became online={online}"))
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
    let link = commandant_common::link::Link::for_addr(&admin_token, &addr);
    let state_dir = tmp.path().join("worker");
    let worker = spawn_worker(&link.addr(), Some(link.token.clone()), &state_dir);
    wait_for_node(&mut client, true).await;

    // The worker remembers the orchestrator for argument-less restarts.
    let creds = commandant_worker::state::load(&state_dir).unwrap().unwrap();
    assert_eq!(creds.server.as_deref(), Some(addr.as_str()));
    worker.abort();
}
