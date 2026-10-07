use super::chat::{Role, Unseen};
use super::*;

pub(crate) fn node(id: &str, online: bool) -> NodeInfo {
    NodeInfo {
        id: id.into(),
        name: format!("box-{id}"),
        online,
        harnesses: vec!["opencode".into()],
        ..Default::default()
    }
}

/// An online node hosting no agent, which can start one.
pub(crate) fn bare(id: &str) -> NodeInfo {
    NodeInfo {
        harnesses: Vec::new(),
        can_host: vec!["opencode".into()],
        ..node(id, true)
    }
}

pub(crate) fn state() -> State {
    let nodes = vec![node("n1", true), node("n2", true), node("n3", false)];
    State::new(nodes, Settings::default())
}

fn open(state: &mut State, node: &str) -> Outcome {
    state.intent(Intent::Open {
        node: node.into(),
        chat: None,
    })
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

pub(crate) fn started(task_id: &str) -> Message {
    Message::Task(task_event::Event::Started(TaskStarted {
        task_id: task_id.into(),
        ..Default::default()
    }))
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
    assert_eq!(state.notice, "box-n3 is offline");

    // Going back to a node shows its chat again.
    let again = state.intent(Intent::Open {
        node: "n2".into(),
        chat: Some(id),
    });
    assert_eq!(again.go, Some(Go::Chat(id)));
    assert_eq!(state.chats.len(), 1);
    assert!(state.notice.is_empty());
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
    let pick = state.pick(Scope::App).expect("the sessions to pick from");
    // New session, the open chat, the saved one.
    assert_eq!(pick.choices.len(), 3);
    assert_eq!(pick.current, Some(Choose::Chat(first)));

    let saved = pick.choices[2].value.clone();
    assert_eq!(pick.choices[2].label, "refactor the store");
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
    assert!(state.pick(Scope::App).is_none());
    assert!(state.busy(), "loading them shows a spinner");

    // What was said since resuming stays after them.
    state.update(Update::Chat(resumed, started("t1")));
    state.update(Update::Chat(
        resumed,
        chat::tests::output(OutputStream::Stdout, b"new"),
    ));
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
    assert_eq!(state.pick(Scope::App).unwrap().choices.len(), 3);
}

#[test]
fn a_working_chat_stays_open() {
    let mut state = state();
    let first = chat_on(&mut state, "n1");
    send(&mut state, first, "hi");
    let refused = state.intent(Intent::Close(first));
    assert!(refused.go.is_none());
    assert_eq!(chat(&state, first).thread.last().unwrap().role, Role::Info);

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
fn closing_a_chat_shows_its_neighbour() {
    let mut state = state();
    let first = chat_on(&mut state, "n1");
    let middle = another(&mut state, first);
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
fn cancelling_before_a_background_task_starts_still_cancels_it() {
    let mut state = state();
    let id = chat_on(&mut state, "n1");
    send(&mut state, id, "hi");
    assert!(state.intent(Intent::Cancel(id)).effects.is_empty());
    // Even if another chat is shown when the task starts.
    another(&mut state, id);
    let outcome = state.update(Update::Chat(id, started("t1")));
    assert!(matches!(&outcome.effects[..], [Effect::Cancel(t)] if t == "t1"));
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
    assert!(state.pick(Scope::App).is_none());
    assert!(
        state.notice.contains("can't start an agent"),
        "{}",
        state.notice
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
    let asked = state.pick(Scope::App).unwrap().clone();
    assert!(asked.loading);
    state.update(Update::Sessions(
        "n1".into(),
        Ok(vec![saved("a"), saved("b")]),
    ));
    // The same picker, revised, done loading.
    let listed = state.pick(Scope::App).unwrap().clone();
    assert_eq!(listed.id, asked.id);
    assert_ne!(listed.revision, asked.revision);
    assert!(!listed.loading);
    assert_eq!(listed.choices.len(), 4);
    // Another node's sessions don't land in it.
    state.update(Update::Sessions("n2".into(), Ok(vec![saved("z")])));
    assert_eq!(state.pick(Scope::App).unwrap().choices.len(), 4);

    state.update(Update::Sessions("n1".into(), Err("offline".into())));
    let last = &chat(&state, id).thread.last().unwrap().text;
    assert!(last.contains("couldn't list the node's sessions"), "{last}");

    // "+ New session" is a new chat.
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
    let pick = state.pick(Scope::App).expect("a harness to choose");
    let harness = Choose::Harness {
        node: "n1".into(),
        harness: "opencode".into(),
    };
    assert_eq!(pick.choices[0].value, harness);
    assert!(!pick.choices[0].detail.is_empty());

    // Backing out starts nothing.
    state.intent(Intent::Dismiss(Scope::App));
    assert!(state.pick(Scope::App).is_none());
    assert!(state.starting.is_empty());

    open(&mut state, "n1");
    let outcome = state.intent(Intent::Choose(Scope::App, harness));
    assert!(matches!(&outcome.effects[..],
        [Effect::StartHarness { node, harness }] if node == "n1" && harness == "opencode"));
    assert!(state.starting.contains("n1"));
    assert!(
        state.notice.contains("starting opencode"),
        "{}",
        state.notice
    );
    // Asking again while it starts doesn't start another.
    assert!(open(&mut state, "n1").effects.is_empty());
    assert!(state.pick(Scope::App).is_none());
    assert!(state.notice.contains("still starting"), "{}", state.notice);

    // Once it hosts it, the node is worth opening.
    let ready = NodeInfo {
        harnesses: vec!["opencode".into()],
        ..bare("n1")
    };
    let outcome = state.update(Update::HarnessStarted("n1".into(), Ok(ready)));
    assert!(outcome.effects.is_empty());
    assert_eq!(outcome.go, Some(Go::Ready("n1".into())));
    assert!(state.starting.is_empty());
    assert_eq!(state.notice, "box-n1 now hosts opencode");
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
    let harness = state.pick(Scope::App).unwrap().choices[0].value.clone();
    state.intent(Intent::Choose(Scope::App, harness));
    let failed = Update::HarnessStarted("n1".into(), Err("no curl on the node".into()));
    let outcome = state.update(failed);
    assert!(outcome.effects.is_empty() && outcome.go.is_none());
    assert!(
        state
            .notice
            .contains("couldn't start an agent on box-n1: no curl"),
        "{}",
        state.notice
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
    let harness = state.pick(Scope::App).expect("the agent picker");
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
