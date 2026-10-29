use super::super::fixtures::node;
use super::*;
use crate::state::ChatId;
use crate::state::chat::{Ended, Message};
use crate::state::{
    Ask, Choices, Choose, Effect, Go, Intent, Outcome, Scope, Settings, State, Update,
};

fn state() -> State {
    let nodes = vec![node("n1", true), node("n2", true)];
    State::new(nodes, Settings::default())
}

fn task(id: &str, kind: &str, status: &str) -> TaskInfo {
    TaskInfo {
        id: id.into(),
        node_id: "n1".into(),
        node_name: "box-n1".into(),
        kind: kind.into(),
        status: status.into(),
        ..Default::default()
    }
}

/// The listing an outcome asks for: which node's, after how long, for which list.
fn fetch(outcome: &Outcome) -> (Option<String>, Duration, ListId) {
    match &outcome.effects[..] {
        [Effect::FetchTasks { node, after, list }] => (node.clone(), *after, *list),
        effects => panic!("should list the tasks, not {effects:?}"),
    }
}

fn listed(state: &mut State, list: ListId, tasks: Vec<TaskInfo>) -> Outcome {
    state.update(Update::Tasks {
        list,
        tasks: Ok(tasks),
    })
}

#[test]
fn the_task_list_refreshes_while_it_is_open() {
    let mut state = state();
    let opened = state.intent(Intent::ListTasks { node: None });
    assert_eq!(opened.go, Some(Go::Tasks));
    let (node, after, list) = fetch(&opened);
    assert_eq!((node, after), (None, Duration::ZERO));
    assert!(state.tasks().is_some_and(|t| t.loading));

    // Listed, it is shown and asked for again in a moment.
    let again = listed(&mut state, list, vec![task("t2", "prompt", "running")]);
    assert_eq!(fetch(&again), (None, REFRESH, list));
    let tasks = state.tasks().unwrap();
    assert!(!tasks.loading);
    assert_eq!(tasks.tasks[0].id, "t2");

    // A failed refresh says why, keeps what was listed, and tries again.
    let failed = state.update(Update::Tasks {
        list,
        tasks: Err("unreachable".into()),
    });
    assert_eq!(fetch(&failed).1, REFRESH);
    let tasks = state.tasks().unwrap();
    assert_eq!(
        (tasks.error.as_deref(), tasks.tasks.len()),
        (Some("unreachable"), 1)
    );

    // Closed, the refreshing stops.
    state.intent(Intent::CloseTasks);
    assert!(state.tasks().is_none());
    assert!(listed(&mut state, list, Vec::new()).effects.is_empty());
}

#[test]
fn the_task_list_is_filtered_by_node() {
    let mut state = state();
    let (_, _, all) = fetch(&state.intent(Intent::ListTasks { node: None }));
    listed(&mut state, all, vec![task("t1", "command", "succeeded")]);

    // The nodes to choose from, every node's tasks first and in effect.
    state.intent(Intent::FilterTasks);
    let Some(Ask::Choose(Choices {
        choices, current, ..
    })) = state.ask(Scope::App)
    else {
        panic!("should ask which node's tasks to show");
    };
    let labels: Vec<_> = choices.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, ["Every node", "box-n1", "box-n2"]);
    assert_eq!(current, &Some(Choose::TasksOf(None)));

    let chosen = state.intent(Intent::Choose(
        Scope::App,
        Choose::TasksOf(Some("n2".into())),
    ));
    let (node, after, on_n2) = fetch(&chosen);
    assert_eq!((node.as_deref(), after), (Some("n2"), Duration::ZERO));
    assert!(state.ask(Scope::App).is_none());
    assert_eq!(state.tasks().unwrap().node.as_deref(), Some("n2"));
    assert!(
        state.tasks().unwrap().tasks.is_empty(),
        "not another node's"
    );

    // What every node's list was still waiting for is too late to matter.
    assert!(
        listed(&mut state, all, vec![task("t1", "command", "succeeded")])
            .effects
            .is_empty()
    );
    assert!(state.tasks().unwrap().tasks.is_empty());
    assert_eq!(fetch(&listed(&mut state, on_n2, Vec::new())).2, on_n2);
}

/// Lists `tasks` and says which list they are on.
fn list(state: &mut State, tasks: Vec<TaskInfo>) -> ListId {
    let (_, _, list) = fetch(&state.intent(Intent::ListTasks { node: None }));
    listed(state, list, tasks);
    list
}

/// The output view an outcome goes to.
fn output_shown(outcome: &Outcome) -> OutputId {
    match outcome.go {
        Some(Go::Output(id)) => id,
        ref go => panic!("should show an output, not {go:?}"),
    }
}

fn event(event: task_event::Event) -> Result<task_event::Event, String> {
    Ok(event)
}

fn printed(data: &[u8], stream: OutputStream) -> Result<task_event::Event, String> {
    event(task_event::Event::Output(TaskOutput {
        stream: stream as i32,
        data: data.to_vec(),
        ..Default::default()
    }))
}

#[test]
fn a_command_task_opens_on_its_output_followed_while_it_runs() {
    let mut state = state();
    let ls = TaskInfo {
        argv: vec!["sh".into(), "-c".into(), "ls -l".into()],
        cwd: "/src".into(),
        created_at: 100,
        ..task("t1", "command", "running")
    };
    list(&mut state, vec![ls]);
    let opened = state.intent(Intent::OpenTask("t1".into()));
    let id = output_shown(&opened);
    assert!(matches!(&opened.effects[..],
        [Effect::Watch { output, task_id }] if *output == id && task_id == "t1"));
    let output = state.output(id).unwrap();
    assert_eq!(
        (
            output.line.as_str(),
            output.node.as_str(),
            output.cwd.as_str()
        ),
        ("ls -l", "box-n1", "/src")
    );
    assert!(output.following());

    let started = task_event::Event::Started(TaskStarted {
        task_id: "t1".into(),
        ..Default::default()
    });
    state.update(Update::Output(id, event(started)));
    state.update(Update::Output(id, printed(b"a\n", OutputStream::Stdout)));
    state.update(Update::Output(id, printed(b"oops\n", OutputStream::Stderr)));
    let finished = task_event::Event::Finished(TaskFinished {
        exit_code: Some(2),
        ..Default::default()
    });
    state.update(Update::Output(id, event(finished)));
    let output = state.output(id).unwrap();
    assert!(!output.following());
    assert_eq!(output.run.ended, Some(Ended::Exited(2)));
    let text: Vec<_> = output
        .run
        .output
        .iter()
        .map(|p| (p.stderr, p.text.as_str()))
        .collect();
    assert_eq!(text, [(false, "a\n"), (true, "oops\n")]);

    // Closed, it is watched no more.
    let closed = state.intent(Intent::CloseOutput(id));
    assert!(matches!(&closed.effects[..], [Effect::StopWatching(o)] if *o == id));
    assert!(state.output(id).is_none());
    let late = state.update(Update::Output(id, printed(b"late\n", OutputStream::Stdout)));
    assert!(late.effects.is_empty());
}

#[test]
fn a_task_whose_output_was_dropped_says_so() {
    let mut state = state();
    list(&mut state, vec![task("t1", "", "succeeded")]);
    let id = output_shown(&state.intent(Intent::OpenTask("t1".into())));
    let started = task_event::Event::Started(TaskStarted {
        task_id: "t1".into(),
        output_pruned: true,
        ..Default::default()
    });
    state.update(Update::Output(id, event(started)));
    assert!(state.output(id).unwrap().pruned);
}

#[test]
fn a_task_listed_as_pruned_says_so_before_it_is_watched() {
    let mut state = state();
    let pruned = TaskInfo {
        output_pruned: true,
        ..task("t1", "command", "succeeded")
    };
    list(&mut state, vec![pruned]);
    let id = output_shown(&state.intent(Intent::OpenTask("t1".into())));
    assert!(state.output(id).unwrap().pruned);
    // A task no longer listed opens nothing.
    let gone = state.intent(Intent::OpenTask("t9".into()));
    assert_eq!((gone.go, gone.effects.len()), (None, 0));
}

fn started(id: OutputId, task_id: &str) -> Update {
    let started = task_event::Event::Started(TaskStarted {
        task_id: task_id.into(),
        ..Default::default()
    });
    Update::Output(id, event(started))
}

#[test]
fn a_command_run_on_a_node_asks_for_its_line_then_shows_its_output() {
    let mut state = state();
    let run = |line: &str| Intent::Run {
        node: "n2".into(),
        line: line.into(),
    };
    assert!(state.intent(run(" ")).effects.is_empty());
    assert!(matches!(
        state.ask(Scope::App),
        Some(Ask::Enter { secret: false, .. })
    ));
    let entered = state.intent(Intent::Enter(Scope::App, "make test".into()));
    assert!(state.ask(Scope::App).is_none());
    let id = output_shown(&entered);
    let [Effect::RunOn { output, request }] = &entered.effects[..] else {
        panic!("should run it, not {:?}", entered.effects);
    };
    assert_eq!(*output, id);
    // In the worker's directory.
    assert_eq!(
        (
            request.node.as_str(),
            &request.argv[..],
            request.cwd.as_str()
        ),
        (
            "n2",
            &["sh".to_string(), "-c".into(), "make test".into()][..],
            ""
        )
    );
    let output = state.output(id).unwrap();
    assert_eq!(
        (output.line.as_str(), output.node.as_str()),
        ("make test", "box-n2")
    );

    // Started here, it is cancelled on quitting; one only watched isn't.
    state.update(started(id, "t9"));
    list(&mut state, vec![task("t1", "command", "running")]);
    let watched = output_shown(&state.intent(Intent::OpenTask("t1".into())));
    state.update(started(watched, "t1"));
    assert_eq!(state.running_tasks(), ["t9"]);
    assert!(state.busy());
}

#[test]
fn a_task_is_cancelled_from_the_list_or_its_output() {
    let mut state = state();
    list(&mut state, vec![task("t1", "command", "running")]);
    let cancel = state.intent(Intent::CancelTask("t1".into()));
    assert!(matches!(&cancel.effects[..], [Effect::CancelTask(t)] if t == "t1"));

    // Its output view says it is stopping.
    let id = output_shown(&state.intent(Intent::OpenTask("t1".into())));
    state.update(started(id, "t1"));
    state.intent(Intent::CancelTask("t1".into()));
    assert!(state.output(id).unwrap().run.stopping());

    let refused = Update::CancelFailed {
        task_id: "t1".into(),
        error: "it had ended".into(),
    };
    state.update(refused);
    assert_eq!(state.notice.text, "couldn't cancel task t1: it had ended");
}

#[test]
fn a_watched_task_is_stopped_before_it_reports_starting() {
    let mut state = state();
    list(&mut state, vec![task("t1", "command", "running")]);
    let id = output_shown(&state.intent(Intent::OpenTask("t1".into())));
    // Its Stop knows the task at once, and cancels it once only.
    assert_eq!(
        state.output(id).unwrap().run.task().map(String::as_str),
        Some("t1")
    );
    let stop = state.intent(Intent::CancelTask("t1".into()));
    assert!(matches!(&stop.effects[..], [Effect::CancelTask(t)] if t == "t1"));
    assert!(state.output(id).unwrap().run.stopping());
    assert!(state.update(started(id, "t1")).effects.is_empty());
    assert!(
        state
            .intent(Intent::CancelTask("t1".into()))
            .effects
            .is_empty()
    );
}

#[test]
fn back_from_an_output_the_list_keeps_its_node_and_refreshing() {
    let mut state = state();
    let (_, _, all) = fetch(&state.intent(Intent::ListTasks { node: None }));
    listed(&mut state, all, vec![task("t1", "command", "succeeded")]);
    let (_, _, on_n1) = fetch(&state.intent(Intent::ListTasks {
        node: Some("n1".into()),
    }));
    listed(&mut state, on_n1, vec![task("t1", "command", "succeeded")]);
    let id = output_shown(&state.intent(Intent::OpenTask("t1".into())));
    state.intent(Intent::CloseOutput(id));
    assert_eq!(state.tasks().unwrap().node.as_deref(), Some("n1"));
    let again = listed(&mut state, on_n1, Vec::new());
    assert_eq!(fetch(&again), (Some("n1".into()), REFRESH, on_n1));
}

fn prompt_task(id: &str, status: &str, session: &str) -> TaskInfo {
    TaskInfo {
        argv: vec!["opencode".into(), "fix the parser".into()],
        cwd: "/src".into(),
        session_id: session.into(),
        ..task(id, "prompt", status)
    }
}

/// The chat an outcome goes to.
fn chat_shown(outcome: &Outcome) -> ChatId {
    match outcome.go {
        Some(Go::Chat(id)) => id,
        ref go => panic!("should show a chat, not {go:?}"),
    }
}

#[test]
fn a_finished_prompt_resumes_its_session() {
    let mut state = state();
    list(&mut state, vec![prompt_task("t1", "succeeded", "ses_a")]);
    let opened = state.intent(Intent::OpenTask("t1".into()));
    let id = chat_shown(&opened);
    assert!(opened.effects.iter().any(|e| matches!(e,
        Effect::FetchHistory { chat, session_id, .. } if *chat == id && session_id == "ses_a")));
    assert!(
        !opened
            .effects
            .iter()
            .any(|e| matches!(e, Effect::WatchTurn { .. }))
    );
    let chat = state.chat(id).unwrap();
    assert_eq!(
        (chat.node.id.as_str(), chat.settings.session_id.as_str()),
        ("n1", "ses_a")
    );
    assert_eq!(chat.settings.cwd, "/src");
    assert!(!chat.activity.working());

    // Opened again, it is the same chat.
    let again = state.intent(Intent::OpenTask("t1".into()));
    assert_eq!(chat_shown(&again), id);
    assert_eq!(state.chats.len(), 1);
}

#[test]
fn a_running_prompt_is_attached_to() {
    let mut state = state();
    list(&mut state, vec![prompt_task("t1", "running", "ses_a")]);
    let opened = state.intent(Intent::OpenTask("t1".into()));
    let id = chat_shown(&opened);
    assert!(opened.effects.iter().any(|e| matches!(e,
        Effect::WatchTurn { chat, task_id } if *chat == id && task_id == "t1")));
    let chat = state.chat(id).unwrap();
    assert_eq!(chat.running_task().as_deref(), Some("t1"));
    // Started elsewhere, it is left running on quitting.
    assert!(state.running_tasks().is_empty());

    // The session's history has the turn under way, which the watch
    // replays: it is said once.
    let history = vec![
        HistoryEntry {
            role: "user".into(),
            text: "earlier".into(),
        },
        HistoryEntry {
            role: "assistant".into(),
            text: "done".into(),
        },
        HistoryEntry {
            role: "user".into(),
            text: "fix the parser".into(),
        },
        HistoryEntry {
            role: "assistant".into(),
            text: "looking".into(),
        },
    ];
    state.update(Update::Chat(id, Message::History(Ok(history))));
    let reply = TaskOutput {
        data: b"looking at it".to_vec(),
        ..Default::default()
    };
    state.update(Update::Chat(
        id,
        Message::Task(task_event::Event::Output(reply)),
    ));
    let said: Vec<_> = state
        .chat(id)
        .unwrap()
        .thread
        .iter()
        .map(|e| e.text.as_str())
        .collect();
    assert_eq!(said, ["earlier", "done", "fix the parser", "looking at it"]);

    // A new session's first turn has no session to open yet.
    list(&mut state, vec![prompt_task("t2", "running", "")]);
    let first = state.intent(Intent::OpenTask("t2".into()));
    assert!(first.go.is_none() && first.effects.is_empty());
    assert!(
        state.notice.text.contains("once it ends"),
        "{}",
        state.notice.text
    );
}

#[test]
fn slash_tasks_shows_every_nodes_tasks() {
    let mut state = state();
    let opened = state.intent(Intent::Open {
        node: "n1".into(),
        chat: None,
    });
    let id = chat_shown(&opened);
    let listing = state.intent(Intent::Submit(id, "/tasks".into()));
    assert_eq!(listing.go, Some(Go::Tasks));
    assert!(
        listing
            .effects
            .iter()
            .any(|e| matches!(e, Effect::FetchTasks { node: None, .. }))
    );
    assert!(state.chat(id).unwrap().thread.is_empty(), "not a prompt");
}

#[test]
fn a_command_run_here_and_closed_while_running_still_ends_on_quitting() {
    let mut state = state();
    let run = Intent::Run {
        node: "n1".into(),
        line: "sleep 60".into(),
    };
    let id = output_shown(&state.intent(run));
    state.update(started(id, "t9"));
    // Closed, it isn't shown, but still followed: it is this client's.
    let closed = state.intent(Intent::CloseOutput(id));
    assert!(closed.effects.is_empty());
    assert!(state.output(id).is_none());
    assert_eq!(state.running_tasks(), ["t9"]);

    // Once it ends, it is forgotten.
    let finished = task_event::Event::Finished(TaskFinished {
        exit_code: Some(0),
        ..Default::default()
    });
    state.update(Update::Output(id, event(finished)));
    assert!(state.running_tasks().is_empty());
    assert!(!state.busy());
}

#[test]
fn a_history_without_the_turn_attached_to_is_kept_whole() {
    let mut state = state();
    let mut task = prompt_task("t1", "running", "ses_a");
    task.argv[1] = "continue".into();
    list(&mut state, vec![task]);
    let id = chat_shown(&state.intent(Intent::OpenTask("t1".into())));
    let entry = |role: &str, text: &str| HistoryEntry {
        role: role.into(),
        text: text.into(),
    };
    // Its turn isn't in it yet: an earlier "continue" is no reason to cut.
    let history = vec![
        entry("user", "continue"),
        entry("assistant", "a"),
        entry("user", "fix it"),
        entry("assistant", "b"),
    ];
    state.update(Update::Chat(id, Message::History(Ok(history))));
    let said: Vec<_> = state
        .chat(id)
        .unwrap()
        .thread
        .iter()
        .map(|e| e.text.as_str())
        .collect();
    assert_eq!(said, ["continue", "a", "fix it", "b", "continue"]);
}
