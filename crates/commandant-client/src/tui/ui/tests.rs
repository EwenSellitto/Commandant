use commandant_client_core::chat::Message;
use commandant_client_core::state::fixtures::{bare, for_run, output, started};
use commandant_client_core::{Core, Effect, Scope, Update};
use commandant_proto::{OutputStream, TaskFinished, task_event};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Modifier;

use super::*;
use crate::tui::app::tests::{app, ask_for_a_key, press};

/// Where `text` isn't.
fn absent(buf: &Buffer, text: &str) -> bool {
    let area = buf.area;
    (0..area.height).all(|y| {
        let row: String = (0..area.width).map(|x| buf[(x, y)].symbol()).collect();
        !row.contains(text)
    })
}

fn render(app: &mut App) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
    terminal.draw(|frame| draw(frame, app)).unwrap();
    terminal.backend().buffer().clone()
}

/// Where `text` starts on screen.
fn find(buf: &Buffer, text: &str) -> (u16, u16) {
    let area = buf.area;
    for y in 0..area.height {
        let row: String = (0..area.width).map(|x| buf[(x, y)].symbol()).collect();
        if let Some(at) = row.find(text) {
            let x = row[..at].chars().count() as u16;
            return (x, y);
        }
    }
    panic!("{text:?} isn't on screen");
}

#[test]
fn secondary_text_stays_readable() {
    let mut app = app();
    app.on_key(KeyEvent::from(KeyCode::Enter));
    let id = app.core.chats()[0].id;
    app.on_update(Update::Chat(
        id,
        output(OutputStream::Reasoning, b"pondering"),
    ));
    let buf = render(&mut app);

    let hint = &buf[find(&buf, "Message")];
    assert_eq!((hint.fg, hint.bg), (HINT, SURFACE));
    assert_eq!(buf[find(&buf, "pondering")].fg, THOUGHT);
    assert_eq!(buf[find(&buf, "opencode")].fg, MUTED);
    // Some palettes make dark gray unreadable, so nothing uses it.
    assert!(buf.content.iter().all(|cell| cell.fg != Color::DarkGray));
}

#[test]
fn a_node_says_what_it_runs_on_and_hosts() {
    let box1 = commandant_proto::NodeInfo {
        os: "linux".into(),
        arch: "x86_64".into(),
        version: "0.4.0".into(),
        harnesses: vec!["opencode".into(), "claude-code".into()],
        ..commandant_client_core::state::fixtures::node("n1", true)
    };
    let mut app = App::new(Core::offline(vec![box1], Default::default()));
    let buf = render(&mut app);
    let (_, row) = find(&buf, "box-n1");
    for fact in ["opencode, claude-code", "linux/x86_64", "v0.4.0"] {
        assert_eq!(find(&buf, fact).1, row, "{fact}");
    }
    assert!(!absent(&buf, "a add") && !absent(&buf, "d remove"));

    let mut nobody = App::new(Core::offline(Vec::new(), Default::default()));
    let buf = render(&mut nobody);
    find(&buf, "press a to add one");
}

#[test]
fn the_worker_command_is_shown_plainly_and_says_once_copied() {
    let mut app = app();
    app.on_key(KeyEvent::from(KeyCode::Char('a')));
    let buf = render(&mut app);
    find(&buf, "make the token");
    app.on_key(KeyEvent::from(KeyCode::Enter));
    app.on_update(Update::TokenCreated {
        ttl: std::time::Duration::from_secs(3600),
        link: Ok("commandant://abc".into()),
    });
    let buf = render(&mut app);
    find(&buf, "commandant-server worker commandant://abc");
    find(&buf, "c copy");
    app.on_key(KeyEvent::from(KeyCode::Char('c')));
    let buf = render(&mut app);
    find(&buf, "copied ✓");
}

#[test]
fn nodes_list_their_open_chats() {
    let mut app = app();
    let buf = render(&mut app);
    assert_eq!(buf[find(&buf, "box-n1")].modifier, Modifier::BOLD);
    assert!(
        find(&buf, "seen ").1 > find(&buf, "box-n2").1,
        "offline says when last seen"
    );
    assert!(absent(&buf, "1 open"));

    app.on_key(KeyEvent::from(KeyCode::Enter));
    app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
    let buf = render(&mut app);
    // Both chats have a tab; the shown one is the second.
    let (_, row) = find(&buf, " 1 new session");
    assert_eq!(find(&buf, " 2 new session").1, row);
    assert_eq!(buf[find(&buf, " 2 new session")].bg, SELECTED);
    assert_ne!(buf[find(&buf, " 1 new session")].bg, SELECTED);

    app.on_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
    let buf = render(&mut app);
    assert_eq!(find(&buf, "2 open").1, find(&buf, "box-n1").1);
}

#[test]
fn many_tabs_scroll_round_the_shown_one() {
    let mut app = app();
    app.on_key(KeyEvent::from(KeyCode::Enter));
    for _ in 0..7 {
        app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
    }
    // The last of eight is shown; earlier ones are out of sight to the left.
    let buf = render(&mut app);
    let (_, row) = find(&buf, " 8 new session");
    assert_eq!(buf[find(&buf, " 8 new session")].bg, SELECTED);
    assert_eq!(find(&buf, "‹").1, row);
    assert!(absent(&buf, " 1 new session"));
    assert!(absent(&buf, "›"));
    assert!(absent(&buf, "ctrl-o"), "no room left for the hint");

    // In the middle, both sides are cut.
    for _ in 0..4 {
        app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
    }
    let buf = render(&mut app);
    assert_eq!(buf[find(&buf, " 4 new session")].bg, SELECTED);
    assert_eq!(find(&buf, "‹").1, row);
    assert_eq!(find(&buf, "›").1, row);

    // A long title (its first prompt) is cut short rather than pushing the
    // others out.
    for _ in 0..80 {
        app.on_key(KeyEvent::from(KeyCode::Char('a')));
    }
    app.on_key(KeyEvent::from(KeyCode::Enter));
    let buf = render(&mut app);
    let (x, _) = find(&buf, " 4 aaaa");
    let row_text: String = (0..buf.area.width)
        .map(|c| buf[(c, row)].symbol())
        .collect();
    assert!(row_text.contains('…'), "{row_text}");
    assert!(x < 30);
}

#[test]
fn the_status_line_says_what_is_loading() {
    let mut app = app();
    app.on_key(KeyEvent::from(KeyCode::Enter));
    let buf = render(&mut app);
    find(&buf, "asking box-n1 what its agent offers…");

    let options = commandant_proto::AgentOptions {
        loading: true,
        ..Default::default()
    };
    app.on_update(Update::Options("n1".into(), Ok(options)));
    let buf = render(&mut app);
    find(&buf, "connecting MCP servers");
    assert!(app.core.busy(), "its spinner turns");

    let mcp = |name: &str, status: &str| commandant_proto::McpServer {
        name: name.into(),
        status: status.into(),
        ..Default::default()
    };
    let options = commandant_proto::AgentOptions {
        mcp_servers: vec![mcp("docs", "connected"), mcp("x", "failed")],
        ..Default::default()
    };
    app.on_update(Update::Options("n1".into(), Ok(options)));
    let buf = render(&mut app);
    find(&buf, "mcp 1/2 connected · 1 failed");
    assert!(!app.core.busy());

    app.on_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
    let buf = render(&mut app);
    find(&buf, "loading · ");
}

#[test]
fn only_a_known_commands_name_is_colored() {
    let mut app = app();
    app.on_key(KeyEvent::from(KeyCode::Enter));
    let typed = |app: &mut App, text: &str| {
        app.shown().unwrap().1.input = Default::default();
        for c in text.chars() {
            app.on_key(KeyEvent::from(KeyCode::Char(c)));
        }
        render(app)
    };
    let accent = Color::Cyan;
    let buf = typed(&mut app, "/model smart");
    assert_eq!(buf[find(&buf, "/model smart")].fg, accent);
    let (x, y) = find(&buf, " smart");
    assert_eq!(buf[(x + 1, y)].fg, Color::Reset, "its argument isn't");
    let buf = typed(&mut app, "/nope x");
    assert_eq!(buf[find(&buf, "/nope x")].fg, Color::Reset, "unknown");
}

#[test]
fn completions_float_over_the_prompt() {
    let mut app = app();
    app.on_key(KeyEvent::from(KeyCode::Enter));
    for c in "/prov".chars() {
        app.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
    let buf = render(&mut app);
    // The empty chat's welcome lists it too: the completion is the lowest.
    let row = (0..buf.area.height)
        .rev()
        .find(|&y| {
            let text: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
            text.contains("/providers ")
        })
        .unwrap();
    assert_eq!(buf[(2, row)].bg, SELECTED);
    let input = (0..buf.area.height)
        .find(|&y| buf[(1, y)].bg == SURFACE)
        .unwrap();
    assert_eq!(row + 2, input, "just over the prompt, past the status line");
}

#[test]
fn an_api_key_is_masked_while_typed() {
    let mut app = app();
    app.on_key(KeyEvent::from(KeyCode::Enter));
    ask_for_a_key(&mut app);
    let buf = render(&mut app);
    find(&buf, "Paste the API key for Acme (hidden)");
    for c in "sk-abc".chars() {
        app.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
    let buf = render(&mut app);
    find(&buf, "••••••");
    assert!(absent(&buf, "sk-abc"));
}

#[test]
fn a_list_coming_while_a_key_is_typed_leaves_it_hidden() {
    let mut app = app();
    app.on_key(KeyEvent::from(KeyCode::Enter));
    let id = app.core.chats()[0].id;
    // Asked for before the key, the projects come while it is typed.
    for c in "/project".chars() {
        app.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
    app.on_key(KeyEvent::from(KeyCode::Enter));
    ask_for_a_key(&mut app);
    for c in "sk-abc".chars() {
        app.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
    let project = commandant_proto::Project {
        name: "app".into(),
        ..Default::default()
    };
    let listed = commandant_client_core::chat::Message::Projects(Ok(vec![project]));
    app.on_update(Update::Chat(id, listed));
    let buf = render(&mut app);
    find(&buf, "••••••");
    assert!(absent(&buf, "sk-abc"));
    assert!(absent(&buf, "Projects"), "it waits");

    // Enter still sends it as the key.
    let effects = press(&mut app, KeyEvent::from(KeyCode::Enter));
    assert!(matches!(
        &effects[..],
        [commandant_client_core::Effect::Authenticate { .. }]
    ));
}

#[test]
fn a_command_runs_in_a_block_of_its_own_until_esc_stops_it() {
    let mut app = app();
    app.on_key(KeyEvent::from(KeyCode::Enter));
    for c in "!ls b".chars() {
        app.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
    let effects = press(&mut app, KeyEvent::from(KeyCode::Enter));
    let [Effect::RunCommand { chat, run, .. }] = effects[..] else {
        panic!("it should run: {effects:?}");
    };
    let buf = render(&mut app);
    find(&buf, "$ ls b");
    let (_, status) = find(&buf, "running ls b  esc to stop");
    assert!(status > find(&buf, "$ ls b").1, "under the thread");

    let ran = |message| Update::Chat(chat, for_run(run, message));
    app.on_update(ran(started("t2")));
    app.on_update(ran(output(OutputStream::Stdout, b"a.txt\n")));
    app.on_update(ran(output(OutputStream::Stderr, b"ls: b: not found\n")));
    let effects = press(&mut app, KeyEvent::from(KeyCode::Esc));
    assert_eq!(effects, [Effect::Cancel(chat, "t2".into())]);
    find(&render(&mut app), "stopping…");

    let finished = TaskFinished {
        exit_code: Some(2),
        ..Default::default()
    };
    app.on_update(ran(Message::Task(task_event::Event::Finished(finished))));
    let buf = render(&mut app);
    assert_eq!(buf[find(&buf, "a.txt")].fg, Color::Reset);
    assert_eq!(buf[find(&buf, "ls: b: not found")].fg, Color::Red);
    let (x, y) = find(&buf, "exit 2");
    assert_eq!(buf[(x, y)].fg, Color::Red);
    assert_eq!(find(&buf, "└").1, y);
    assert!(absent(&buf, "esc to stop"));
}

#[test]
fn the_harness_picker_floats_over_the_nodes() {
    let mut app = App::new(Core::offline(vec![bare("n1")], Default::default()));
    let buf = render(&mut app);
    find(&buf, "enter to start an agent");
    app.on_key(KeyEvent::from(KeyCode::Enter));
    let buf = render(&mut app);
    find(&buf, "Start an agent on this node");
    find(&buf, "opencode");
    app.on_key(KeyEvent::from(KeyCode::Enter));
    let buf = render(&mut app);
    assert!(absent(&buf, "Start an agent on this node"));
    find(&buf, "starting its agent…");
}

#[test]
fn a_question_to_confirm_or_read_floats_over_the_rest() {
    let mut app = app();
    app.on_key(KeyEvent::from(KeyCode::Enter));
    let id = app.core.chats()[0].id;
    let show = Ask::Show {
        title: "Link".into(),
        text: "commandant://abc".into(),
    };
    app.core.set_ask(Scope::Chat(id), show);
    let buf = render(&mut app);
    find(&buf, "Link");
    find(&buf, "commandant://abc");
    find(&buf, "esc close");

    app.on_key(KeyEvent::from(KeyCode::Esc));
    let confirm = Ask::Confirm {
        text: "Remove box-n1?".into(),
        yes: commandant_client_core::Choose::NewSession,
    };
    app.core.set_ask(Scope::App, confirm);
    let buf = render(&mut app);
    assert!(absent(&buf, "commandant://abc"));
    find(&buf, "Remove box-n1?");
    find(&buf, "enter yes");
}

#[test]
fn the_help_floats_over_the_rest_and_narrows_as_typed() {
    let mut app = app();
    app.on_key(KeyEvent::from(KeyCode::Enter));
    app.on_key(KeyEvent::from(KeyCode::F(1)));
    let buf = render(&mut app);
    find(&buf, "Help");
    find(&buf, "what every command and key does");
    find(&buf, "enter do it");
    for c in "scroll".chars() {
        app.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
    let buf = render(&mut app);
    find(&buf, "PgUp PgDn");
    find(&buf, "1 of ");
    assert!(absent(&buf, "what every command and key does"));
}

#[test]
fn the_tasks_say_what_ran_where_and_how_it_ended() {
    let mut app = app();
    let mut tasks = crate::tui::app::tests::listed_tasks();
    tasks[1].output_pruned = true;
    tasks.push(commandant_proto::TaskInfo {
        status: "failed".into(),
        exit_code: Some(3),
        ..tasks[0].clone()
    });
    crate::tui::app::tests::tasks_listed(&mut app, tasks);
    let buf = render(&mut app);
    assert!(find(&buf, "every node").1 < find(&buf, "echo t2").1);
    let (_, running) = find(&buf, "running");
    assert_eq!(find(&buf, "echo t2").1, running);
    assert_eq!(buf[find(&buf, "exit 3")].fg, Color::Red);
    // What a pruned task ran is greyed.
    assert_eq!(buf[find(&buf, "echo t1")].fg, MUTED);
    assert_eq!(
        buf[find(&buf, "box-n1")].bg,
        SELECTED,
        "the newest is highlighted"
    );
}

#[test]
fn an_output_fills_the_screen_and_says_when_it_was_pruned() {
    let mut app = app();
    crate::tui::app::tests::tasks_listed(&mut app, crate::tui::app::tests::listed_tasks());
    app.on_key(KeyEvent::from(KeyCode::Down));
    app.on_key(KeyEvent::from(KeyCode::Up));
    let effects = press(&mut app, KeyEvent::from(KeyCode::Enter));
    let [Effect::Watch { output, .. }] = effects[..] else {
        panic!("should watch it, not {effects:?}");
    };
    let started = task_event::Event::Started(commandant_proto::TaskStarted {
        task_id: "t2".into(),
        output_pruned: true,
        ..Default::default()
    });
    app.on_update(Update::Output(output, Ok(started)));
    let printed = task_event::Event::Output(commandant_proto::TaskOutput {
        stream: OutputStream::Stderr as i32,
        data: b"oops\n".to_vec(),
        ..Default::default()
    });
    app.on_update(Update::Output(output, Ok(printed)));
    let buf = render(&mut app);
    assert_eq!(find(&buf, "$ echo t2").1, 0);
    assert!(find(&buf, "worker's directory").1 == 1);
    find(&buf, "output pruned");
    assert_eq!(buf[find(&buf, "oops")].fg, Color::Red);
    find(&buf, "running");
    find(&buf, "x stop");

    let finished = task_event::Event::Finished(TaskFinished {
        exit_code: Some(0),
        ..Default::default()
    });
    app.on_update(Update::Output(output, Ok(finished)));
    let buf = render(&mut app);
    find(&buf, "exit 0");
    assert!(absent(&buf, "x stop"));
}

#[test]
fn the_line_to_run_is_typed_in_a_box_over_the_nodes() {
    let mut app = app();
    app.on_key(KeyEvent::from(KeyCode::Char('r')));
    for c in "uptime".chars() {
        app.on_key(KeyEvent::from(KeyCode::Char(c)));
    }
    let buf = render(&mut app);
    let (_, label) = find(&buf, "A shell command to run on box-n1");
    assert_eq!(find(&buf, "› uptime").1, label + 2);
}
