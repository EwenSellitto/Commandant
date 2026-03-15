use std::time::Duration;

mod app;
mod components;
mod terminal;
mod ui;

use app::App;
use clap::{Parser, ValueEnum};
use commandant_core::browser::WorkspaceBrowserData;
use crossterm::event::{self, Event, KeyEventKind};
use ratatui::DefaultTerminal;
use terminal::{restore_terminal, setup_terminal};

const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum BackgroundMode {
    On,
    Off,
}

impl BackgroundMode {
    fn force_background(self) -> bool {
        matches!(self, Self::On)
    }
}

#[derive(Debug, Parser)]
#[command(name = "commandant-cli")]
struct Cli {
    #[arg(long, value_enum, default_value_t = BackgroundMode::Off)]
    background: BackgroundMode,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let mut terminal = setup_terminal()?;
    let app = App::new(
        WorkspaceBrowserData::sample(),
        cli.background.force_background(),
    );
    let result = run_app(&mut terminal, app);
    restore_terminal()?;
    result
}

fn run_app(terminal: &mut DefaultTerminal, mut app: App) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        terminal.draw(|frame| ui::render(frame, &app))?;

        if !event::poll(POLL_INTERVAL)? {
            continue;
        }

        let Event::Key(key) = event::read()? else {
            continue;
        };

        if key.kind != KeyEventKind::Press {
            continue;
        }

        if app.handle_key(key) {
            break;
        }
    }

    Ok(())
}
