//! `commandant tui`: chat with a node's coding agent in a terminal UI.
//!
//! It opens on the list of nodes. Each node can have several chats, each its
//! own agent session, and chats on any node work at the same time. Each
//! prompt is an ordinary prompt task; a chat keeps its session id from one
//! reply to the next so the conversation continues.

mod app;
mod asks;
mod chat;
mod help;
mod text;
mod ui;

use std::io::stdout;
use std::time::Duration;

use anyhow::Result;
use commandant_client_core::config::Client;
use commandant_client_core::{Core, Updates};
use crossterm::clipboard::CopyToClipboard;
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste, EventStream};
use crossterm::execute;
use ratatui::DefaultTerminal;
use tokio::runtime::Handle;
use tokio_stream::StreamExt;

use self::app::App;
use crate::cli::UiArgs;

/// How often the screen refreshes on its own, for the busy spinner.
const TICK: Duration = Duration::from_millis(100);

pub async fn run(client: &Client, args: UiArgs, handle: Handle) -> Result<()> {
    let (core, mut updates) = Core::start(client, args.defaults(), handle).await?;
    let first = args.first_open(core.nodes())?;
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
        // OSC 52: the terminal puts it on the clipboard, even over SSH.
        if let Some(text) = app.copying.take() {
            execute!(stdout(), CopyToClipboard::to_clipboard_from(text))?;
        }
        if app.quit {
            app.core.shutdown().await;
            return Ok(());
        }
    }
}
