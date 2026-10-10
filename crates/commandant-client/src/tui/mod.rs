//! `commandant tui`: chat with a node's coding agent in a terminal UI.
//!
//! It opens on the list of nodes. Each node can have several chats, each its
//! own agent session, and chats on any node work at the same time. Each
//! prompt is an ordinary prompt task; a chat keeps its session id from one
//! reply to the next so the conversation continues.

mod app;
mod asks;
mod chat;
mod picker;
mod text;
mod ui;

use std::io::stdout;
use std::time::Duration;

use anyhow::{Result, bail};
use commandant_client_core::config::Client;
use commandant_client_core::{Core, Intent, Settings, Updates};
use commandant_common::lookup::{self, Match};
use commandant_proto::NodeInfo;
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste, EventStream};
use crossterm::execute;
use ratatui::DefaultTerminal;
use tokio::runtime::Handle;
use tokio_stream::StreamExt;

use self::app::App;
use crate::cli::TuiArgs;

/// How often the screen refreshes on its own, for the busy spinner.
const TICK: Duration = Duration::from_millis(100);

pub async fn run(client: &Client, args: TuiArgs, handle: Handle) -> Result<()> {
    let defaults = Settings {
        session_id: String::new(),
        cwd: args.cwd.unwrap_or_default(),
        model: args.model.unwrap_or_default(),
        agent: args.agent.unwrap_or_default(),
        effort: args.effort.unwrap_or_default(),
    };
    let (core, mut updates) = Core::start(client, defaults, handle).await?;
    let first = match (args.node.as_deref(), args.session) {
        (Some(needle), session) => Some(Intent::Open {
            node: find_node(core.nodes(), needle)?.id,
            chat: None,
            session: Some(session.unwrap_or_default()),
        }),
        (None, Some(_)) => bail!("--session needs the node it is on"),
        (None, None) => None,
    };
    let mut app = App::new(core);
    if let Some(open) = first {
        app.act(open);
    }
    let mut terminal = ratatui::init();
    execute!(stdout(), EnableBracketedPaste)?;
    let result = event_loop(&mut terminal, &mut app, &mut updates).await;
    execute!(stdout(), DisableBracketedPaste)?;
    ratatui::restore();
    result?;

    for chat in app.core.chats() {
        if !chat.settings.session_id.is_empty() {
            eprintln!(
                "commandant: continue with: commandant tui {} -s {}",
                chat.node.name, chat.settings.session_id
            );
        }
    }
    Ok(())
}

async fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    updates: &mut Updates,
) -> Result<()> {
    let mut input = EventStream::new();
    let mut tick = tokio::time::interval(TICK);
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;
        tokio::select! {
            event = input.next() => match event {
                Some(event) => app.on_input(event?),
                None => app.quit = true,
            },
            Some(update) = updates.next() => app.on_update(update),
            // Only the spinners need redrawing on their own.
            _ = tick.tick(), if app.core.busy() => {}
        }
        // A reply streams in many small pieces: take what has come before
        // drawing again, or before quitting, so a task that has just
        // started is known and cancelled.
        while let Some(update) = updates.ready() {
            app.on_update(update);
        }
        if app.quit {
            app.core.shutdown().await;
            return Ok(());
        }
    }
}

/// The node named by `needle`: a name, an id or an id prefix.
fn find_node(nodes: &[NodeInfo], needle: &str) -> Result<NodeInfo> {
    match lookup::find_named(nodes.to_vec(), needle, |n| &n.name, |n| &n.id) {
        Match::One(node) => Ok(node),
        Match::Ambiguous => bail!("{needle:?} is ambiguous"),
        Match::None => bail!("no node matches {needle:?}"),
    }
}
