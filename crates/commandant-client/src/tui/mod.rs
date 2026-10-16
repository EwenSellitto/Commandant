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
mod login;
mod picker;
mod text;
mod ui;

use std::io::stdout;
use std::time::Duration;

use anyhow::Result;
use commandant_client_core::config::{self, Client};
use commandant_client_core::{Core, Intent, Updates};
use crossterm::clipboard::CopyToClipboard;
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste, EventStream};
use crossterm::execute;
use ratatui::DefaultTerminal;
use tokio::runtime::Handle;
use tokio_stream::StreamExt;

use self::app::{App, Leave};
use self::login::{LogIn, Step};
use crate::cli::UiArgs;

/// How often the screen refreshes on its own, for the busy spinner.
const TICK: Duration = Duration::from_millis(100);

/// Runs on the `saved` server, or first asks for a link when there is none.
pub async fn run(saved: Option<Client>, args: UiArgs, handle: Handle) -> Result<()> {
    // A saved server that can't be reached ends the TUI with the error.
    let on = match saved {
        Some(client) => {
            let (core, updates) = Core::start(&client, args.defaults(), handle.clone()).await?;
            let first = args.first_open(core.nodes())?;
            Some(On {
                client,
                app: open(core, first),
                updates,
            })
        }
        None => None,
    };
    let mut terminal = ratatui::init();
    execute!(stdout(), EnableBracketedPaste)?;
    let result = run_until_left(&mut terminal, on, &args, &handle).await;
    execute!(stdout(), DisableBracketedPaste)?;
    ratatui::restore();
    let Some(app) = result? else {
        return Ok(());
    };

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

/// The app on a server, and what its core's calls report.
struct On {
    client: Client,
    app: App,
    updates: Updates,
}

/// The app on `core`, showing the `first` chat if one is named.
fn open(core: Core, first: Option<Intent>) -> App {
    let mut app = App::new(core);
    if let Some(open) = first {
        app.act(open);
    }
    app
}

/// Runs the app on a server, logging in first when there is none, and
/// again after each log out; the app last left, unless it was by logging
/// out, or leaving before logging in.
async fn run_until_left(
    terminal: &mut DefaultTerminal,
    mut on: Option<On>,
    args: &UiArgs,
    handle: &Handle,
) -> Result<Option<App>> {
    loop {
        let mut on = match on.take() {
            Some(on) => on,
            None => match log_in(terminal, args, handle).await? {
                Some((client, core, updates)) => {
                    // The node named may well be on another server.
                    let first = args.first_open(core.nodes()).ok().flatten();
                    On {
                        client,
                        app: open(core, first),
                        updates,
                    }
                }
                None => return Ok(None),
            },
        };
        event_loop(terminal, &mut on.app, &mut on.updates).await?;
        if on.app.leaving != Some(Leave::LogOut) {
            return Ok(Some(on.app));
        }
        // A server flags or the environment named keeps the saved one.
        config::forget_if_saved(&on.client)?;
    }
}

/// The first-run screen, until a pasted link is connected to, and saved;
/// none if the person leaves instead.
async fn log_in(
    terminal: &mut DefaultTerminal,
    args: &UiArgs,
    handle: &Handle,
) -> Result<Option<(Client, Core, Updates)>> {
    let mut screen = LogIn::default();
    let mut input = EventStream::new();
    loop {
        terminal.draw(|frame| ui::draw_log_in(frame, &screen))?;
        let Some(event) = input.next().await else {
            return Ok(None);
        };
        let client = match screen.on_input(event?) {
            Step::Connect(client) => client,
            Step::Leave => return Ok(None),
            Step::Wait | Step::Cancel => continue,
        };
        terminal.draw(|frame| ui::draw_log_in(frame, &screen))?;
        let started = {
            let connecting = Core::start(&client, args.defaults(), handle.clone());
            tokio::pin!(connecting);
            // The keys still work, to give up.
            loop {
                tokio::select! {
                    started = &mut connecting => break Some(started),
                    event = input.next() => match event {
                        Some(event) => match screen.on_input(event?) {
                            Step::Leave => return Ok(None),
                            Step::Cancel => break None,
                            _ => {}
                        },
                        None => return Ok(None),
                    },
                }
            }
        };
        match started {
            Some(Ok((core, updates))) => match config::remember(&client) {
                Ok(_) => return Ok(Some((client, core, updates))),
                Err(e) => screen.failed(&e.context("saving the link")),
            },
            Some(Err(e)) => screen.failed(&e),
            None => {}
        }
    }
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
                None => app.leaving = Some(Leave::Quit),
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
        if app.leaving.is_some() {
            app.core.shutdown().await;
            return Ok(());
        }
    }
}
