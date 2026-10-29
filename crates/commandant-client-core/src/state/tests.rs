use super::chat::{Menu, Message, Role, Unseen};
use super::fixtures::{acme, bare, for_run, method, node, output, started};
use super::*;

fn state() -> State {
    let nodes = vec![node("n1", true), node("n2", true), node("n3", false)];
    State::new(nodes, Settings::default())
}

fn open(state: &mut State, node: &str) -> Outcome {
    state.intent(Intent::Open {
        node: node.into(),
        chat: None,
    })
}

/// What the app asks to choose from, if that is what it asks.
fn choices(state: &State) -> Option<&Choices> {
    state.ask(Scope::App).and_then(Ask::choices)
}

/// The chat an outcome goes to.
fn shown(outcome: &Outcome) -> ChatId {
    match outcome.go {
        Some(Go::Chat(id)) => id,
        ref go => panic!("should show a chat, not {go:?}"),
    }
}

/// Opens a chat on `node` and says which.
fn chat_on(state: &mut State, node: &str) -> ChatId {
    shown(&open(state, node))
}

fn another(state: &mut State, id: ChatId) -> ChatId {
    shown(&state.intent(Intent::NewChat(id)))
}

fn submit(state: &mut State, id: ChatId, text: &str) -> Outcome {
    state.intent(Intent::Submit(id, text.into()))
}

/// Sends `text` from a chat and returns the request.
fn send(state: &mut State, id: ChatId, text: &str) -> PromptRequest {
    match submit(state, id, text).effects.pop() {
        Some(Effect::Send(chat, request)) if chat == id => request,
        _ => panic!("{text:?} should be sent"),
    }
}

fn finished(session_id: &str) -> Message {
    Message::Task(task_event::Event::Finished(TaskFinished {
        exit_code: Some(0),
        session_id: session_id.into(),
        ..Default::default()
    }))
}

/// Has the chat send a prompt and finish its turn, so it isn't blank.
fn talked(state: &mut State, id: ChatId) {
    send(state, id, "hi");
    state.update(Update::Chat(id, finished("")));
}

fn chat(state: &State, id: ChatId) -> &Chat {
    state.chat(id).expect("the chat is open")
}

fn fetches(outcome: &Outcome) -> usize {
    let fetch = |e: &&Effect| matches!(e, Effect::FetchOptions { .. });
    outcome.effects.iter().filter(fetch).count()
}

#[test]
fn a_node_opens_on_its_chat_or_a_new_one() {
    let mut state = state();
    let outcome = open(&mut state, "n2");
    // It asks the node for its sessions and options.
    assert!(matches!(&outcome.effects[..],
        [Effect::FetchSessions(a), Effect::FetchOptions { node: b, .. }] if a == "n2" && b == "n2"));
    let id = shown(&outcome);
    assert_eq!(chat(&state, id).node.id, "n2");

    // An offline node with no chat yet can't be opened.
    let offline = open(&mut state, "n3");
    assert!(offline.effects.is_empty() && offline.go.is_none());
    assert_eq!(state.notice.text, "box-n3 is offline");

    // Going back to a node shows its chat again.
    let again = state.intent(Intent::Open {
        node: "n2".into(),
        chat: Some(id),
    });
    assert_eq!(again.go, Some(Go::Chat(id)));
    assert_eq!(state.chats.len(), 1);
    assert!(state.notice.text.is_empty());
}

#[test]
fn a_front_end_starts_on_a_new_chat_on_the_session_asked_for() {
    let mut state = state();
    let open = |state: &mut State, session: Option<&str>| {
        state.intent(Intent::OpenSession {
            node: "n2".into(),
            session: session.map(Into::into),
        })
    };
    let first = shown(&open(&mut state, Some("ses_a")));
    assert_eq!(chat(&state, first).settings.session_id, "ses_a");
    // Each time, beside the chats already open.
    let second = shown(&open(&mut state, None));
    assert_ne!(first, second);
    assert_eq!(chat(&state, second).settings.session_id, "");
    assert!(open(&mut state, Some("ses_a")).go.is_some());
    assert_eq!(state.chats.len(), 3);
}

#[test]
fn sessions_on_a_node_run_side_by_side() {
    let mut state = state();
    let first = chat_on(&mut state, "n1");
    let request = send(&mut state, first, "fix the parser");
    assert_eq!(request.node, "n1");

    // A second session starts while the first is still working.
    let second = another(&mut state, first);
    assert_ne!(first, second);
    assert_eq!(send(&mut state, second, "write the docs").session_id, "");
    assert_eq!(state.chats_on("n1").count(), 2);

    // Each says how it ended until it is seen.
    state.update(Update::Chat(first, finished("ses_a")));
    state.update(Update::Chat(second, finished("ses_b")));
    assert_eq!(chat(&state, first).unseen, Some(Unseen::Done));
    state.intent(Intent::Seen(first));
    assert_eq!(chat(&state, first).unseen, None);
    assert_eq!(chat(&state, second).unseen, Some(Unseen::Done));

    // Each continues its own session.
    let request = send(&mut state, first, "and the lexer");
    assert_eq!(request.session_id, "ses_a");
    assert_eq!(state.running_tasks().len(), 0, "no task id until started");
}

#[test]
fn saved_sessions_are_resumed_from_the_picker() {
    let mut state = state();
    let first = chat_on(&mut state, "n1");
    state.update(Update::Sessions(
        "n1".into(),
        Ok(vec![AgentSession {
            id: "ses_old".into(),
            title: "refactor the store".into(),
            directory: "/src/app".into(),
            agent: "plan".into(),
            model: "a/smart".into(),
            cost: 0.25,
            ..Default::default()
        }]),
    ));
    let outcome = state.intent(Intent::Sessions(first));
    assert!(matches!(&outcome.effects[..], [Effect::FetchSessions(n)] if n == "n1"));
    let pick = choices(&state).expect("the sessions to pick from");
    // The open chat, the saved one, then a new session.
    assert_eq!(pick.choices.len(), 3);
    assert_eq!(pick.current, Some(Choose::Chat(first)));
    assert_eq!(pick.choices[2].value, Choose::NewSession);

    let saved = pick.choices[1].value.clone();
    assert_eq!(pick.choices[1].label, "refactor the store");
    let outcome = state.intent(Intent::Choose(Scope::App, saved));
    let resumed = shown(&outcome);
    let Some(Effect::FetchHistory {
        chat: asking,
        session_id,
        ..
    }) = outcome.effects.last()
    else {
        panic!("resuming asks for the session's earlier messages");
    };
    assert_eq!((*asking, session_id.as_str()), (resumed, "ses_old"));
    assert!(state.ask(Scope::App).is_none());
    assert!(state.busy(), "loading them shows a spinner");

    // What was said since resuming stays after them.
    state.update(Update::Chat(resumed, started("t1")));
    state.update(Update::Chat(resumed, output(OutputStream::Stdout, b"new")));
    let entry = |role: &str, text: &str| HistoryEntry {
        role: role.into(),
        text: text.into(),
    };
    let history = Ok(vec![
        entry("user", "old question"),
        entry("agent", "old answer"),
    ]);
    state.update(Update::Chat(resumed, Message::History(history)));
    let chat = chat(&state, resumed);
    assert!(!chat.loading_history);
    let thread: Vec<_> = chat
        .thread
        .iter()
        .map(|e| (e.role, e.text.as_str()))
        .collect();
    assert_eq!(
        thread,
        [
            (Role::User, "old question"),
            (Role::Agent, "old answer"),
            (Role::Agent, "new"),
        ]
    );
    assert_eq!(chat.title, "refactor the store");
    assert_eq!(chat.settings.session_id, "ses_old");
    assert_eq!(chat.settings.cwd, "/src/app");
    assert_eq!(
        (chat.settings.agent.as_str(), chat.settings.model.as_str()),
        ("plan", "a/smart")
    );
    assert_eq!(state.chats.len(), 2);

    // An open session isn't offered twice.
    state.intent(Intent::Sessions(resumed));
    assert_eq!(choices(&state).unwrap().choices.len(), 3);
}

/// What the app asks to confirm, if that is what it asks.
fn confirm(state: &State) -> Option<(&str, &Choose)> {
    match state.ask(Scope::App)? {
        Ask::Confirm { text, yes } => Some((text, yes)),
        _ => None,
    }
}

#[test]
fn closing_a_working_chat_asks_first() {
    let mut state = state();
    let first = chat_on(&mut state, "n1");
    send(&mut state, first, "hi");
    state.update(Update::Chat(first, started("t1")));
    let asked = state.intent(Intent::Close(first));
    assert!(asked.go.is_none() && asked.effects.is_empty());
    assert_eq!(
        confirm(&state),
        Some(("Cancel the turn and close?", &Choose::CancelAndClose(first)))
    );
    // Saying no leaves it at work.
    state.intent(Intent::Dismiss(Scope::App));
    assert!(chat(&state, first).activity.working());

    state.intent(Intent::Close(first));
    let closed = state.intent(Intent::Confirm(Scope::App));
    assert!(
        matches!(&closed.effects[..], [Effect::Cancel(id, task)] if *id == first && task == "t1")
    );
    assert_eq!(closed.go, Some(Go::Nodes));
    assert!(state.chats.is_empty() && state.ask(Scope::App).is_none());
}

#[test]
fn a_turn_closed_before_it_starts_is_cancelled_once_it_does() {
    let mut state = state();
    let id = chat_on(&mut state, "n1");
    send(&mut state, id, "hi");
    state.intent(Intent::Close(id));
    let closed = state.intent(Intent::Confirm(Scope::App));
    assert!(closed.effects.is_empty() && state.chats.is_empty());
    let late = state.update(Update::Chat(id, started("t1")));
    assert!(
        matches!(&late.effects[..], [Effect::Cancel(chat, task)] if *chat == id && task == "t1")
    );
}

#[test]
fn a_chat_closes_where_it_is_typed() {
    let mut state = state();
    let first = chat_on(&mut state, "n1");
    send(&mut state, first, "hi");

    // `/close` closes the chat it is typed in.
    let second = another(&mut state, first);
    let closed = submit(&mut state, second, "/close");
    assert_eq!(closed.go, Some(Go::Chat(first)));
    assert_eq!(closed.prompt, Some((second, Edit::Clear)));
    assert_eq!(state.chats.len(), 1);

    state.update(Update::Chat(first, finished("ses_a")));
    assert_eq!(state.intent(Intent::Close(first)).go, Some(Go::Nodes));
    assert!(state.chats.is_empty());
}

#[test]
fn options_reach_every_chat_on_the_node() {
    let mut state = state();
    let first = chat_on(&mut state, "n1");
    another(&mut state, first);
    let options = AgentOptions {
        default_agent: "build".into(),
        ..Default::default()
    };
    state.update(Update::Options("n1".into(), Ok(options)));
    assert!(state.chats.iter().all(|c| c.agent() == "build"));
    // A later chat starts with them, without asking again.
    let later = state.intent(Intent::NewChat(first));
    assert!(later.effects.is_empty());
    assert_eq!(chat(&state, shown(&later)).agent(), "build");
}

#[test]
fn a_chip_opens_its_menu_and_leaves_the_prompt_alone() {
    let mut state = state();
    let id = chat_on(&mut state, "n1");
    let options = AgentOptions {
        agents: vec![AgentChoice {
            name: "build".into(),
            ..Default::default()
        }],
        default_agent: "build".into(),
        ..Default::default()
    };
    state.update(Update::Options("n1".into(), Ok(options)));
    let opened = state.intent(Intent::Menu(id, Menu::Agent));
    assert_eq!(opened.prompt, None, "what is typed stays");
    let pick = chat(&state, id).ask().and_then(Ask::choices).unwrap();
    assert_eq!(pick.title, "Agent");
    assert_eq!(pick.current, Some(Choose::Agent("build".into())));
}

#[test]
fn a_new_chat_on_a_node_with_a_blank_one_is_that_one() {
    let mut state = state();
    let blank = chat_on(&mut state, "n1");
    assert_eq!(another(&mut state, blank), blank, "nothing to start afresh");
    let session = Intent::OpenSession {
        node: "n1".into(),
        session: None,
    };
    assert_eq!(shown(&state.intent(session)), blank);
    assert_eq!(state.chats.len(), 1);

    // Once something is said in it, a new chat is another.
    send(&mut state, blank, "hi");
    assert_ne!(another(&mut state, blank), blank);
}

#[test]
fn leaving_a_blank_chat_for_one_with_history_closes_it() {
    let mut state = state();
    let used = chat_on(&mut state, "n1");
    send(&mut state, used, "hi");
    let blank = another(&mut state, used);
    let elsewhere = chat_on(&mut state, "n2");

    // Another blank one isn't worth dropping it for.
    state.intent(Intent::Leave {
        chat: blank,
        to: elsewhere,
    });
    assert!(state.chat(blank).is_some());
    let left = state.intent(Intent::Leave {
        chat: blank,
        to: used,
    });
    assert!(state.chat(blank).is_none());
    assert_eq!(left.go, None, "the front end shows what it goes to");

    // One with history stays.
    state.intent(Intent::Leave {
        chat: used,
        to: elsewhere,
    });
    assert!(state.chat(used).is_some());
}

#[test]
fn closing_a_chat_shows_its_neighbour() {
    let mut state = state();
    let first = chat_on(&mut state, "n1");
    talked(&mut state, first);
    let middle = another(&mut state, first);
    talked(&mut state, middle);
    let last = another(&mut state, middle);

    // The middle one gives way to the one after it...
    assert_eq!(state.intent(Intent::Close(middle)).go, Some(Go::Chat(last)));
    // ...the last one to the one before.
    assert_eq!(state.intent(Intent::Close(last)).go, Some(Go::Chat(first)));
    // Updates for closed chats are dropped.
    let late = state.update(Update::Chat(middle, started("t1")));
    assert!(late.effects.is_empty());
    assert!(state.running_tasks().is_empty());
}

#[test]
fn quitting_leaves_the_tasks_on_every_node_to_cancel() {
    let mut state = state();
    let one = chat_on(&mut state, "n1");
    send(&mut state, one, "one");
    let two = chat_on(&mut state, "n2");
    send(&mut state, two, "two");
    state.update(Update::Chat(one, started("t1")));
    state.update(Update::Chat(two, started("t2")));
    let mut running = state.running_tasks();
    running.sort();
    assert_eq!(running, ["t1", "t2"]);
    assert_eq!(submit(&mut state, one, "/quit").go, Some(Go::Quit));
    assert_eq!(submit(&mut state, two, "/exit").go, Some(Go::Quit));
}

#[test]
fn logging_out_leaves_the_tasks_to_cancel() {
    let mut state = state();
    let id = chat_on(&mut state, "n1");
    send(&mut state, id, "one");
    state.update(Update::Chat(id, started("t1")));
    let outcome = submit(&mut state, id, "/logout");
    assert_eq!(outcome.go, Some(Go::LogOut));
    assert!(outcome.effects.is_empty(), "no providers are listed");
    assert_eq!(state.running_tasks(), ["t1"]);
}

#[test]
fn the_link_is_shown_to_copy() {
    let mut state = state();
    state.link = "commandant://abc".into();
    let id = chat_on(&mut state, "n1");
    let outcome = submit(&mut state, id, "/link");
    assert!(outcome.effects.is_empty(), "the server isn't asked");
    assert_eq!(outcome.prompt, Some((id, Edit::Clear)));
    assert_eq!(
        state.ask(Scope::App),
        Some(&Ask::Show {
            title: "Link".into(),
            text: "commandant://abc".into(),
        })
    );
}

#[test]
fn the_link_is_shown_with_no_chat_open() {
    let mut state = state();
    state.link = "commandant://abc".into();
    let outcome = state.intent(Intent::ShowLink);
    assert!(outcome.effects.is_empty(), "the server isn't asked");
    assert_eq!(
        state.ask(Scope::App),
        Some(&Ask::Show {
            title: "Link".into(),
            text: "commandant://abc".into(),
        })
    );
}

#[test]
fn logging_out_is_confirmed_first() {
    let mut state = state();
    assert_eq!(state.intent(Intent::LogOut).go, None);
    assert!(matches!(state.ask(Scope::App), Some(Ask::Confirm { .. })));
    state.intent(Intent::Dismiss(Scope::App));
    assert_eq!(state.ask(Scope::App), None);
    state.intent(Intent::LogOut);
    assert_eq!(
        state.intent(Intent::Confirm(Scope::App)).go,
        Some(Go::LogOut)
    );
    assert_eq!(state.ask(Scope::App), None);
}

#[test]
fn the_server_is_out_of_reach_until_it_lists_the_nodes_again() {
    let mut state = state();
    assert!(state.server_reachable());
    state.update(Update::Unreachable);
    assert!(!state.server_reachable());
    // The same nodes as before still say it answers.
    let nodes = state.nodes.clone();
    state.update(Update::Nodes(nodes));
    assert!(state.server_reachable());
}

#[test]
fn a_notice_says_if_it_is_an_error_and_counts_each_time_said() {
    let mut state = state();
    open(&mut state, "n3");
    assert_eq!(state.notice.text, "box-n3 is offline");
    assert!(!state.notice.error);
    let said = state.notice.said;
    open(&mut state, "n3");
    assert_eq!(state.notice.said, said + 1, "said again is news again");
    state.update(Update::CancelFailed {
        task_id: "t1".into(),
        error: "gone".into(),
    });
    assert_eq!(state.notice.text, "couldn't cancel task t1: gone");
    assert!(state.notice.error);
}

#[test]
fn logging_in_is_not_signing_in() {
    let mut state = state();
    let id = chat_on(&mut state, "n1");
    let outcome = submit(&mut state, id, "/login");
    assert!(outcome.effects.is_empty(), "no providers are listed");
    assert_eq!(outcome.go, None);
    assert!(state.ask(Scope::Chat(id)).is_none());
    let said = &chat(&state, id).thread.last().expect("a line is said").text;
    assert!(said.contains("/providers"), "{said}");
}

#[test]
fn a_running_command_keeps_its_chat_busy_and_open() {
    let mut state = state();
    let id = chat_on(&mut state, "n1");
    state.update(Update::Options("n1".into(), Ok(AgentOptions::default())));
    assert!(!state.busy());
    let run = match submit(&mut state, id, "!sleep 9").effects.pop() {
        Some(Effect::RunCommand { chat, run, .. }) if chat == id => run,
        _ => panic!("it should run"),
    };
    assert!(state.busy(), "its block shows a spinner");
    assert!(chat(&state, id).at_work());
    state.update(Update::Chat(id, for_run(run, started("t9"))));
    let asked = state.intent(Intent::Close(id));
    assert!(asked.go.is_none());
    assert_eq!(
        confirm(&state).map(|(text, _)| text),
        Some("Stop the command and close?")
    );

    // Quitting stops it.
    assert_eq!(state.running_tasks(), ["t9"]);
    let closed = state.intent(Intent::Confirm(Scope::App));
    assert!(matches!(&closed.effects[..], [Effect::Cancel(_, task)] if task == "t9"));
    assert!(state.chats.is_empty());
}

#[test]
fn a_blocks_stop_stops_that_command_alone() {
    let mut state = state();
    let id = chat_on(&mut state, "n1");
    let ran = |state: &mut State, line: &str| match submit(state, id, line).effects.pop() {
        Some(Effect::RunCommand { run, .. }) => run,
        _ => panic!("{line:?} should run"),
    };
    let one = ran(&mut state, "!sleep 9");
    let two = ran(&mut state, "!sleep 8");
    state.update(Update::Chat(id, for_run(one, started("t1"))));
    state.update(Update::Chat(id, for_run(two, started("t2"))));
    send(&mut state, id, "hi");
    state.update(Update::Chat(id, started("t3")));

    let stop = |state: &mut State, run| state.intent(Intent::StopRun(id, run)).effects;
    assert!(matches!(&stop(&mut state, two)[..], [Effect::Cancel(_, t)] if t == "t2"));
    assert!(stop(&mut state, two).is_empty(), "it is stopping");
    let stopping: Vec<_> = chat(&state, id).runs().map(|r| r.stopping()).collect();
    assert_eq!(stopping, [false, true], "the other runs on");
    assert!(chat(&state, id).activity.working(), "and so does the turn");

    // One not started yet is stopped once it is.
    let three = ran(&mut state, "!sleep 7");
    assert!(stop(&mut state, three).is_empty());
    let late = state.update(Update::Chat(id, for_run(three, started("t4"))));
    assert!(matches!(&late.effects[..], [Effect::Cancel(_, t)] if t == "t4"));
}

#[test]
fn cancelling_before_a_background_task_starts_still_cancels_it() {
    let mut state = state();
    let id = chat_on(&mut state, "n1");
    send(&mut state, id, "hi");
    assert!(state.intent(Intent::Cancel(id)).effects.is_empty());
    // Even if another chat is shown when the task starts.
    another(&mut state, id);
    let outcome = state.update(Update::Chat(id, started("t1")));
    assert!(matches!(&outcome.effects[..], [Effect::Cancel(_, t)] if t == "t1"));
}

#[test]
fn chats_follow_their_node() {
    let mut state = state();
    let id = chat_on(&mut state, "n1");
    let mut offline = node("n1", false);
    offline.version = "0.2.0".into();
    state.update(Update::Nodes(vec![node("n2", true), offline]));
    let chat = chat(&state, id);
    assert!(!chat.node.online);
    assert_eq!(chat.node.version, "0.2.0");

    // An offline node's chat is still there to show.
    let reopened = state.intent(Intent::Open {
        node: "n1".into(),
        chat: None,
    });
    assert_eq!(reopened.go, Some(Go::Chat(id)));
    assert!(
        reopened.effects.is_empty(),
        "nothing to ask an offline node"
    );
    // A node that's gone opens nothing.
    state.update(Update::Nodes(Vec::new()));
    let gone = open(&mut state, "n1");
    assert!(gone.go.is_none() && gone.effects.is_empty());
}

#[test]
fn an_old_worker_without_an_agent_isnt_opened() {
    let mut old = bare("n9");
    old.can_host.clear();
    let mut state = State::new(vec![old], Settings::default());
    let outcome = open(&mut state, "n9");
    assert!(outcome.effects.is_empty() && outcome.go.is_none());
    assert!(state.ask(Scope::App).is_none());
    assert!(
        state.notice.text.contains("can't start an agent"),
        "{}",
        state.notice.text
    );
}

#[test]
fn the_session_picker_keeps_up_with_the_node() {
    let mut state = state();
    let id = chat_on(&mut state, "n1");
    let saved = |id: &str| AgentSession {
        id: id.into(),
        title: format!("about {id}"),
        ..Default::default()
    };
    // Failing to list sessions isn't news unless someone is looking.
    state.update(Update::Sessions("n1".into(), Err("offline".into())));
    assert!(chat(&state, id).thread.is_empty());

    state.intent(Intent::Sessions(id));
    let asked = choices(&state).unwrap().clone();
    assert!(asked.loading);
    state.update(Update::Sessions(
        "n1".into(),
        Ok(vec![saved("a"), saved("b")]),
    ));
    // The same picker, revised, done loading.
    let listed = choices(&state).unwrap().clone();
    assert_eq!(listed.id, asked.id);
    assert_ne!(listed.revision, asked.revision);
    assert!(!listed.loading);
    assert_eq!(listed.choices.len(), 4);
    // Another node's sessions don't land in it.
    state.update(Update::Sessions("n2".into(), Ok(vec![saved("z")])));
    assert_eq!(choices(&state).unwrap().choices.len(), 4);

    state.update(Update::Sessions("n1".into(), Err("offline".into())));
    let last = &chat(&state, id).thread.last().unwrap().text;
    assert!(last.contains("couldn't list the node's sessions"), "{last}");

    // "+ New session" is a new chat.
    talked(&mut state, id);
    let new = state.intent(Intent::Choose(Scope::App, Choose::NewSession));
    assert_ne!(shown(&new), id);
    assert_eq!(state.chats.len(), 2);
}

#[test]
fn a_node_is_asked_for_its_options_once_at_a_time() {
    let mut state = state();
    let first = open(&mut state, "n1");
    assert_eq!(fetches(&first), 1);
    let first = shown(&first);
    // A second chat waits for the same answer.
    let second = state.intent(Intent::NewChat(first));
    assert_eq!(fetches(&second), 0);
    let second = shown(&second);
    state.update(Update::Options("n1".into(), Err("timed out".into())));

    // Both were waiting, so both hear it failed, once each.
    let errors = |state: &State, id| {
        let thread = &chat(state, id).thread;
        thread
            .iter()
            .filter(|e| e.text.contains("timed out"))
            .count()
    };
    assert_eq!((errors(&state, first), errors(&state, second)), (1, 1));
    // Once answered, it can be asked again, but not twice at once.
    assert_eq!(fetches(&state.intent(Intent::CycleAgent(second, 1))), 1);
    assert_eq!(fetches(&state.intent(Intent::CycleAgent(first, 1))), 0);
}

#[test]
fn a_node_without_an_agent_offers_to_start_one() {
    let mut state = State::new(vec![bare("n1")], Settings::default());
    assert!(open(&mut state, "n1").effects.is_empty());
    let pick = choices(&state).expect("a harness to choose");
    let harness = Choose::Harness {
        node: "n1".into(),
        harness: "opencode".into(),
    };
    assert_eq!(pick.choices[0].value, harness);
    assert!(!pick.choices[0].detail.is_empty());

    // Backing out starts nothing.
    state.intent(Intent::Dismiss(Scope::App));
    assert!(state.ask(Scope::App).is_none());
    assert!(state.starting.is_empty());

    open(&mut state, "n1");
    let outcome = state.intent(Intent::Choose(Scope::App, harness));
    assert!(matches!(&outcome.effects[..],
        [Effect::StartHarness { node, harness }] if node == "n1" && harness == "opencode"));
    assert!(state.starting.contains("n1"));
    assert!(
        state.notice.text.contains("starting opencode"),
        "{}",
        state.notice.text
    );
    // Asking again while it starts doesn't start another.
    assert!(open(&mut state, "n1").effects.is_empty());
    assert!(state.ask(Scope::App).is_none());
    assert!(
        state.notice.text.contains("still starting"),
        "{}",
        state.notice.text
    );

    // Once it hosts it, the node is worth opening.
    let ready = NodeInfo {
        harnesses: vec!["opencode".into()],
        ..bare("n1")
    };
    let outcome = state.update(Update::HarnessStarted("n1".into(), Ok(ready)));
    assert!(outcome.effects.is_empty());
    assert_eq!(outcome.go, Some(Go::Ready("n1".into())));
    assert!(state.starting.is_empty());
    assert_eq!(state.notice.text, "box-n1 now hosts opencode");
    assert_eq!(state.nodes[0].harnesses, ["opencode"]);
    let opened = open(&mut state, "n1");
    assert!(matches!(
        &opened.effects[..],
        [Effect::FetchSessions(_), Effect::FetchOptions { .. }]
    ));
    assert_eq!(chat(&state, shown(&opened)).node.harnesses, ["opencode"]);
}

#[test]
fn a_harness_that_fails_to_start_is_reported() {
    let mut state = State::new(vec![bare("n1")], Settings::default());
    open(&mut state, "n1");
    let harness = choices(&state).unwrap().choices[0].value.clone();
    state.intent(Intent::Choose(Scope::App, harness));
    let failed = Update::HarnessStarted("n1".into(), Err("no curl on the node".into()));
    let outcome = state.update(failed);
    assert!(outcome.effects.is_empty() && outcome.go.is_none());
    assert!(
        state
            .notice
            .text
            .contains("couldn't start an agent on box-n1: no curl"),
        "{}",
        state.notice.text
    );
    assert!(state.starting.is_empty(), "it can be tried again");
}

#[test]
fn commands_that_are_still_loading_are_asked_for_again() {
    let mut state = state();
    let id = chat_on(&mut state, "n1");
    let loading = AgentOptions {
        default_agent: "build".into(),
        loading: true,
        ..Default::default()
    };
    let outcome = state.update(Update::Options("n1".into(), Ok(loading)));
    assert!(matches!(&outcome.effects[..],
        [Effect::FetchOptions { node, after }] if node == "n1" && !after.is_zero()));
    // The rest is usable already.
    assert_eq!(chat(&state, id).agent(), "build");

    // Meanwhile a command waits in the prompt, rather than going out as text.
    let waits = submit(&mut state, id, "/review the parser");
    assert!(waits.effects.is_empty());
    assert_eq!(waits.prompt, None);
    let said = &chat(&state, id).thread.last().unwrap().text;
    assert!(said.contains("still loading"), "{said}");

    let loaded = AgentOptions {
        default_agent: "build".into(),
        commands: vec![AgentCommand {
            name: "review".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let outcome = state.update(Update::Options("n1".into(), Ok(loaded)));
    assert!(outcome.effects.is_empty());
    let request = send(&mut state, id, "/review the parser");
    assert_eq!(
        (request.command.as_str(), request.prompt.as_str()),
        ("review", "the parser")
    );
}

#[test]
fn an_mcp_switch_answers_its_chat_and_leaves_other_requests_alone() {
    let mut state = state();
    // Opening the node asks for its options; that request is open.
    let asking = chat_on(&mut state, "n1");
    talked(&mut state, asking);
    let other = another(&mut state, asking);
    let switched = AgentOptions {
        default_agent: "plan".into(),
        ..Default::default()
    };
    let update = Update::McpSwitched {
        chat: asking,
        node: "n1".into(),
        name: "docs".into(),
        options: Ok(switched),
    };
    assert!(state.update(update).effects.is_empty());
    // Every chat on the node has the new options...
    assert!(state.chats.iter().all(|c| c.agent() == "plan"));
    // ...the open request is still open...
    assert!(state.fetching.contains("n1"));
    // ...and only the chat that switched hears how it went.
    let failed = Update::McpSwitched {
        chat: asking,
        node: "n1".into(),
        name: "docs".into(),
        options: Err("timed out".into()),
    };
    state.update(failed);
    let said = |state: &State, id| {
        let thread = &chat(state, id).thread;
        thread
            .iter()
            .any(|e| e.text.contains("couldn't switch docs"))
    };
    assert!(said(&state, asking));
    assert!(!said(&state, other));
}

#[test]
fn busy_while_something_moves() {
    let mut state = state();
    assert!(!state.busy());
    let id = chat_on(&mut state, "n1");
    assert!(state.busy(), "asking for the options shows a spinner");
    state.update(Update::Options("n1".into(), Ok(AgentOptions::default())));
    send(&mut state, id, "hi");
    assert!(state.busy(), "a working chat's spinner turns");
    state.update(Update::Chat(id, finished("ses_a")));
    assert!(!state.busy());
    state.starting.insert("n2".into());
    assert!(state.busy(), "so does a node starting its agent");
}

#[test]
fn a_node_that_lost_its_agent_offers_one_again_then_refreshes_its_chats() {
    let mut state = state();
    let id = chat_on(&mut state, "n1");
    state.update(Update::Options("n1".into(), Ok(AgentOptions::default())));

    // Its worker restarted without --harness.
    let mut nodes = state.nodes.clone();
    nodes[0] = bare("n1");
    state.update(Update::Nodes(nodes));
    let open_it = Intent::Open {
        node: "n1".into(),
        chat: Some(id),
    };
    let outcome = state.intent(open_it.clone());
    assert!(outcome.go.is_none(), "not the stale chat");
    let harness = choices(&state).expect("the agent picker");
    let harness = harness.choices[0].value.clone();
    state.intent(Intent::Choose(Scope::App, harness));

    let ready = NodeInfo {
        harnesses: vec!["opencode".into()],
        ..bare("n1")
    };
    let outcome = state.update(Update::HarnessStarted("n1".into(), Ok(ready)));
    // Its chat asks the new agent what it offers, and can be shown again.
    let asks = |e: &Effect| matches!(e, Effect::FetchOptions { node, .. } if node == "n1");
    assert!(outcome.effects.iter().any(asks));
    assert_eq!(state.intent(open_it).go, Some(Go::Chat(id)));
}

#[test]
fn a_node_that_may_answer_in_a_moment_is_asked_again_quietly() {
    let mut state = state();
    let id = chat_on(&mut state, "n1");
    let timed_out = || {
        let failure = Failure {
            message: "node box-n1 didn't say what its agent offers".into(),
            transient: true,
        };
        Update::Options("n1".into(), Err(failure))
    };
    let errors = |state: &State| {
        let thread = &chat(state, id).thread;
        thread.iter().filter(|e| e.role == Role::Error).count()
    };
    // Twice it asks again, saying nothing...
    for _ in 0..OPTIONS_RETRIES {
        let outcome = state.update(timed_out());
        assert!(
            matches!(&outcome.effects[..], [Effect::FetchOptions { after, .. }] if !after.is_zero())
        );
        assert_eq!(errors(&state), 0);
    }
    // ...then it says why, and stops asking.
    assert!(state.update(timed_out()).effects.is_empty());
    assert_eq!(errors(&state), 1);
    assert!(!state.fetching.contains("n1"));

    // An answer resets the count; a refusal is shown at once.
    state.update(Update::Options("n1".into(), Ok(AgentOptions::default())));
    assert!(!state.retries.contains_key("n1"));
    another(&mut state, id);
    let refused = Update::Options("n1".into(), Err("node box-n1 runs no agent harness".into()));
    assert!(state.update(refused).effects.is_empty());
}

#[test]
fn a_chat_asks_for_a_key_on_its_own_and_one_thing_at_a_time() {
    let mut state = state();
    let id = chat_on(&mut state, "n1");
    let other = chat_on(&mut state, "n2");
    let providers = || {
        let acme = acme(vec![method("API key", false, 0)]);
        Update::Chat(id, Message::Providers(Ok(vec![acme])))
    };
    state.intent(Intent::Submit(id, "/providers".into()));
    state.update(providers());
    assert!(state.ask(Scope::Chat(id)).and_then(Ask::choices).is_some());
    state.intent(Intent::Choose(
        Scope::Chat(id),
        Choose::Provider("acme".into()),
    ));
    let key = Choose::SignIn {
        provider: "acme".into(),
        oauth: None,
    };
    state.intent(Intent::Choose(Scope::Chat(id), key));
    assert!(state.ask(Scope::Chat(id)).is_some_and(Ask::secret));
    assert!(
        state.ask(Scope::Chat(other)).is_none(),
        "only that chat asks"
    );

    // The app asks its own question alongside.
    state.intent(Intent::Sessions(other));
    assert!(choices(&state).is_some());
    assert!(state.ask(Scope::Chat(id)).is_some_and(Ask::secret));
    state.intent(Intent::Dismiss(Scope::App));

    // A question coming late waits: the key is still asked for, and isn't
    // chosen over.
    state.update(providers());
    assert!(state.ask(Scope::Chat(id)).is_some_and(Ask::secret));
    let said = &chat(&state, id).thread.last().unwrap().text;
    assert!(said.contains("finish signing in first"), "{said}");
    let over = Intent::Choose(Scope::Chat(id), Choose::Provider("acme".into()));
    assert!(state.intent(over).effects.is_empty());
    assert!(state.ask(Scope::Chat(id)).is_some_and(Ask::secret));

    let outcome = state.intent(Intent::Enter(Scope::Chat(id), "sk-secret".into()));
    let [Effect::Authenticate { action, .. }] = &outcome.effects[..] else {
        panic!("the key is sent");
    };
    let key = auth_action::Action::ApiKey("sk-secret".into());
    assert_eq!(action.action, Some(key));
    assert!(state.ask(Scope::Chat(id)).is_none());
    let thread = &chat(&state, id).thread;
    assert!(thread.iter().all(|e| !e.text.contains("sk-secret")));
    assert!(state.busy(), "signing in shows a spinner");
}

#[test]
fn a_confirmation_does_what_it_asked_once_said_yes() {
    let mut state = state();
    state.ask = Some(Ask::Confirm {
        text: "Start opencode on box-n1?".into(),
        yes: Choose::Harness {
            node: "n1".into(),
            harness: "opencode".into(),
        },
    });
    let outcome = state.intent(Intent::Confirm(Scope::App));
    assert!(matches!(
        &outcome.effects[..],
        [Effect::StartHarness { .. }]
    ));
    assert!(state.ask(Scope::App).is_none());
    // Nothing asked, nothing done.
    assert!(state.intent(Intent::Confirm(Scope::App)).effects.is_empty());

    state.ask = Some(Ask::Show {
        title: "Link".into(),
        text: "commandant://…".into(),
    });
    state.intent(Intent::Dismiss(Scope::App));
    assert!(state.ask(Scope::App).is_none());
}

#[test]
fn projects_are_browsed_without_typing_the_command() {
    let mut state = state();
    let id = chat_on(&mut state, "n1");
    let outcome = state.intent(Intent::Projects(id));
    assert!(matches!(&outcome.effects[..],
        [Effect::FetchProjects { chat, node }] if *chat == id && node == "n1"));
    // What is typed is left alone, and nothing joins the prompt's history.
    assert_eq!(outcome.prompt, None);
    assert!(chat(&state, id).sent().is_empty());
    assert!(chat(&state, id).listing_projects);

    // A session that has started stays in its directory.
    let started = chat_on(&mut state, "n2");
    talked(&mut state, started);
    state.update(Update::Chat(started, finished("ses_1")));
    assert!(state.intent(Intent::Projects(started)).effects.is_empty());
}

#[test]
fn a_nodes_sessions_are_offered_over_the_chat_it_opens() {
    let mut state = state();
    // With no chat yet, a blank one opens under the sessions, asked for once.
    let outcome = state.intent(Intent::NodeSessions {
        node: "n1".into(),
        chat: None,
    });
    let id = shown(&outcome);
    let asked = |e: &&Effect| matches!(e, Effect::FetchSessions(n) if n == "n1");
    assert_eq!(outcome.effects.iter().filter(asked).count(), 1);
    let pick = choices(&state).expect("the sessions to pick from");
    assert_eq!(pick.current, Some(Choose::Chat(id)));

    // Over the chat last shown on it, when there is one.
    state.intent(Intent::Dismiss(Scope::App));
    talked(&mut state, id);
    let other = another(&mut state, id);
    let outcome = state.intent(Intent::NodeSessions {
        node: "n1".into(),
        chat: Some(id),
    });
    assert_eq!(shown(&outcome), id);
    assert_ne!(id, other);
    assert!(choices(&state).is_some());

    // An offline node with no chat says why nothing opens.
    state.intent(Intent::Dismiss(Scope::App));
    let offline = state.intent(Intent::NodeSessions {
        node: "n3".into(),
        chat: None,
    });
    assert!(offline.go.is_none() && choices(&state).is_none());
    assert_eq!(state.notice.text, "box-n3 is offline");
}

#[test]
fn the_chat_beside_is_on_the_same_node_and_wraps_around() {
    let mut state = state();
    let first = chat_on(&mut state, "n1");
    talked(&mut state, first);
    let second = another(&mut state, first);
    talked(&mut state, second);
    let elsewhere = chat_on(&mut state, "n2");
    assert_eq!(state.chat_beside(first, 1), Some(second));
    assert_eq!(state.chat_beside(first, -1), Some(second));
    assert_eq!(state.chat_beside(second, 1), Some(first));
    // Alone on its node, a chat has none beside it but itself.
    assert_eq!(state.chat_beside(elsewhere, 1), Some(elsewhere));
}
