use std::time::Duration;

use super::super::fixtures::{bare, node, started};
use crate::state::{Ask, Choose, Effect, Go, Intent, Scope, Settings, State, Update, Wanted};

const HOUR: Duration = Duration::from_secs(3600);

fn state() -> State {
    let nodes = vec![node("n1", true), node("n2", true), node("n3", false)];
    State::new(nodes, Settings::default())
}

fn enter(state: &mut State, line: &str) -> Vec<Effect> {
    state.intent(Intent::Enter(Scope::App, line.into())).effects
}

#[test]
fn a_node_is_added_with_a_token_that_lasts_as_long_as_asked() {
    let mut state = state();
    // It asks how long the token lasts: an hour, unless told.
    assert!(state.intent(Intent::AddNode).effects.is_empty());
    assert!(matches!(
        state.ask(Scope::App),
        Some(Ask::Enter {
            wanted: Wanted::Lifetime,
            ..
        })
    ));
    assert_eq!(enter(&mut state, ""), [Effect::CreateToken { ttl: HOUR }]);
    assert!(state.ask(Scope::App).is_none());

    // Then the command that enrols the machine it runs on.
    state.update(Update::TokenCreated {
        ttl: HOUR,
        link: Ok("commandant://abc".into()),
    });
    let Some(Ask::Show { title, text }) = state.ask(Scope::App) else {
        panic!("should show the worker's command");
    };
    assert_eq!(text, "commandant-server worker commandant://abc");
    assert!(title.contains("within 1h"), "{title}");

    // A lifetime it can't read, or none at all, is asked for again.
    state.intent(Intent::AddNode);
    for typed in ["soon", "0"] {
        assert!(enter(&mut state, typed).is_empty());
        assert!(state.ask(Scope::App).is_some_and(Ask::wants_line));
        assert!(!state.notice.text.is_empty(), "says why {typed:?} won't do");
    }
    let half = Duration::from_secs(1800);
    assert_eq!(
        enter(&mut state, "30m"),
        [Effect::CreateToken { ttl: half }]
    );
    assert!(state.notice.text.is_empty());
    state.update(Update::TokenCreated {
        ttl: half,
        link: Err("denied".into()),
    });
    assert_eq!(state.notice.text, "couldn't make a token: denied");
    assert!(state.ask(Scope::App).is_none());
}

#[test]
fn an_agent_is_started_on_a_node_that_hosts_none() {
    let mut state = state();
    state.nodes.push(bare("n4"));
    // Chosen among those it can host.
    state.intent(Intent::StartAgent("n4".into()));
    let choices = state.ask(Scope::App).and_then(Ask::choices).unwrap();
    let opencode = Choose::Harness {
        node: "n4".into(),
        harness: "opencode".into(),
    };
    assert_eq!(choices.choices[0].value, opencode);
    let effects = state.intent(Intent::Choose(Scope::App, opencode)).effects;
    assert!(matches!(&effects[..], [Effect::StartHarness { node, .. }] if node == "n4"));

    // One with an agent, or offline, says so instead.
    for (node, said) in [
        ("n1", "box-n1 already hosts opencode"),
        ("n3", "box-n3 is offline"),
    ] {
        assert!(
            state
                .intent(Intent::StartAgent(node.into()))
                .effects
                .is_empty()
        );
        assert!(state.ask(Scope::App).is_none());
        assert_eq!(state.notice.text, said);
    }
}

#[test]
fn a_node_is_removed_once_confirmed_and_its_chats_close() {
    let mut state = state();
    let open = Intent::Open {
        node: "n1".into(),
        chat: None,
    };
    let Some(Go::Chat(chat)) = state.intent(open).go else {
        panic!("should open a chat");
    };
    // Declining changes nothing.
    assert!(
        state
            .intent(Intent::RemoveNode("n1".into()))
            .effects
            .is_empty()
    );
    let Some(Ask::Confirm { text, .. }) = state.ask(Scope::App) else {
        panic!("should ask first");
    };
    assert!(
        text.contains("box-n1") && text.contains("open chat"),
        "{text}"
    );
    state.intent(Intent::Dismiss(Scope::App));
    assert_eq!(state.nodes.len(), 3);
    assert!(state.chat(chat).is_some());

    // Confirmed, it goes with its chats, once the server has dropped it.
    state.intent(Intent::RemoveNode("n1".into()));
    let removing = state.intent(Intent::Confirm(Scope::App)).effects;
    assert_eq!(removing, [Effect::RemoveNode("n1".into())]);
    assert_eq!(state.nodes.len(), 3);
    state.update(Update::NodeRemoved("n1".into(), Ok(())));
    assert!(state.node("n1").is_none());
    assert!(state.chat(chat).is_none());
    assert_eq!(state.notice.text, "removed box-n1");

    // Refused, it stays, and says why.
    state.intent(Intent::RemoveNode("n2".into()));
    state.intent(Intent::Confirm(Scope::App));
    state.update(Update::NodeRemoved("n2".into(), Err("not found".into())));
    assert_eq!(state.notice.text, "couldn't remove box-n2: not found");
    assert!(state.node("n2").is_some());
}

#[test]
fn removing_a_node_stops_what_its_chats_are_at() {
    let mut state = state();
    let open = Intent::Open {
        node: "n1".into(),
        chat: None,
    };
    let Some(Go::Chat(chat)) = state.intent(open).go else {
        panic!("should open a chat");
    };
    state.intent(Intent::Submit(chat, "hold it".into()));
    state.update(Update::Chat(chat, started("t1")));

    // Asking says so; once confirmed, its turn is cancelled.
    state.intent(Intent::RemoveNode("n1".into()));
    let Some(Ask::Confirm { text, .. }) = state.ask(Scope::App) else {
        panic!("should ask first");
    };
    assert!(text.contains("still at work"), "{text}");
    let effects = state.intent(Intent::Confirm(Scope::App)).effects;
    assert!(
        effects.contains(&Effect::Cancel(chat, "t1".into())),
        "{effects:?}"
    );
    state.update(Update::NodeRemoved("n1".into(), Ok(())));
    assert!(state.chat(chat).is_none());
}

#[test]
fn a_node_removed_stays_gone_from_a_listing_made_before() {
    let mut state = state();
    state.intent(Intent::RemoveNode("n1".into()));
    state.intent(Intent::Confirm(Scope::App));
    state.update(Update::NodeRemoved("n1".into(), Ok(())));
    // A poll the server answered just before forgetting it.
    let stale = vec![node("n1", true), node("n2", true), node("n3", false)];
    state.update(Update::Nodes(stale));
    assert!(state.node("n1").is_none());
    assert_eq!(state.nodes.len(), 2);
}
