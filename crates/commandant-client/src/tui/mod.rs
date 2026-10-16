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

use anyhow::{Context as _, Result};
use commandant_client_core::config::{self, Client};
use commandant_client_core::{Core, Settings, Updates};
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
    // A saved server that can't be reached, or that hasn't the node named,
    // ends the TUI with the error.
    let started = match saved {
        Some(client) => {
            let (core, updates) = Core::start(&client, args.defaults(), handle.clone()).await?;
            args.first_open(core.nodes())?;
            Some((client, core, updates))
        }
        None => None,
    };
    let mut terminal = ratatui::init();
    execute!(stdout(), EnableBracketedPaste)?;
    let result = run_until_left(&mut terminal, started, &args, &handle).await;
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

/// A core on a server, and what its calls report.
type Started = (Client, Core, Updates);

/// Runs the app on a server, logging in first when there is none, and
/// again after each log out; the app last left, unless it was by logging
/// out, or leaving before logging in.
async fn run_until_left(
    terminal: &mut DefaultTerminal,
    mut started: Option<Started>,
    args: &UiArgs,
    handle: &Handle,
) -> Result<Option<App>> {
    loop {
        let started = match started.take() {
            Some(started) => Some(started),
            None => log_in(terminal, args, handle).await?,
        };
        let Some((client, core, mut updates)) = started else {
            return Ok(None);
        };
        let mut app = App::new(core);
        // After logging in, the node named may well be on another server.
        if let Ok(Some(open)) = args.first_open(app.core.nodes()) {
            app.act(open);
        }
        event_loop(terminal, &mut app, &mut updates).await?;
        if app.leaving != Some(Leave::LogOut) {
            return Ok(Some(app));
        }
        // A server flags or the environment named keeps the saved one.
        config::forget_if_saved(&client)?;
    }
}

/// The first-run screen, until a pasted link is connected to, and saved;
/// none if the person leaves instead. The keys still work while
/// connecting, to give up.
async fn log_in(
    terminal: &mut DefaultTerminal,
    args: &UiArgs,
    handle: &Handle,
) -> Result<Option<Started>> {
    let mut screen = LogIn::default();
    let mut input = EventStream::new();
    let mut connecting = None;
    loop {
        terminal.draw(|frame| ui::draw_log_in(frame, &screen))?;
        tokio::select! {
            started = async { connecting.as_mut().expect("connecting").await }, if connecting.is_some() => {
                connecting = None;
                match started {
                    Ok(started) => return Ok(Some(started)),
                    Err(e) => screen.failed(&e),
                }
            }
            event = input.next() => {
                let Some(event) = event else {
                    return Ok(None);
                };
                match screen.on_input(event?) {
                    Step::Connect(client) => {
                        connecting = Some(Box::pin(connect(client, args.defaults(), handle.clone())));
                    }
                    Step::Cancel => connecting = None,
                    Step::Leave => return Ok(None),
                    Step::Wait => {}
                }
            }
        }
    }
}

/// Connects to `client`'s server, and saves it once that works.
async fn connect(client: Client, defaults: Settings, handle: Handle) -> Result<Started> {
    let (core, updates) = Core::start(&client, defaults, handle).await?;
    config::remember(&client).context("saving the link")?;
    Ok((client, core, updates))
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
