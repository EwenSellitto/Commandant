use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

use ratatui::style::Modifier;

use super::*;
use crate::state::Update;
use crate::state::fixtures::{bare, output};
use crate::tui::app::tests::{app, ask_for_a_key};
use commandant_proto::OutputStream;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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
    let id = app.state.chats[0].id;
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

    // A long title is cut short rather than pushing the others out.
    let long = "a".repeat(80);
    let id = app.chat().unwrap().id;
    let chat = app.state.chats.iter_mut().find(|c| c.id == id).unwrap();
    chat.title = long;
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
    assert!(app.state.busy(), "its spinner turns");

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
    assert!(!app.state.busy());

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
fn the_harness_picker_floats_over_the_nodes() {
    let mut app = App::new(vec![bare("n1")], Default::default());
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
    let id = app.state.chats[0].id;
    let show = Ask::Show {
        title: "Link".into(),
        text: "commandant://abc".into(),
    };
    app.state.set_ask(Scope::Chat(id), show);
    let buf = render(&mut app);
    find(&buf, "Link");
    find(&buf, "commandant://abc");
    find(&buf, "esc close");

    app.on_key(KeyEvent::from(KeyCode::Esc));
    let confirm = Ask::Confirm {
        text: "Remove box-n1?".into(),
        yes: crate::state::Choose::NewSession,
    };
    app.state.set_ask(Scope::App, confirm);
    let buf = render(&mut app);
    assert!(absent(&buf, "commandant://abc"));
    find(&buf, "Remove box-n1?");
    find(&buf, "enter yes");
}
