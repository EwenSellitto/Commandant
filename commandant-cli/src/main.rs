use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
mod app;
mod components;
mod terminal;
mod ui;

use app::App;
use clap::{Args, Parser, Subcommand, ValueEnum};
use commandant_core::browser::WorkspaceBrowserData;
use commandant_docker_compose::{ComposeExecutor, ComposeProject};
use crossterm::event::{self, Event, KeyEventKind};
use ratatui::DefaultTerminal;
use terminal::{restore_terminal, setup_terminal};
use tokio::runtime::Runtime;

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

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    DockerCompose(DockerComposeCommand),
}

#[derive(Debug, Args)]
struct DockerComposeCommand {
    #[command(subcommand)]
    command: DockerComposeSubcommand,
}

#[derive(Debug, Subcommand)]
enum DockerComposeSubcommand {
    Up(DockerComposeUpCommand),
}

#[derive(Debug, Args)]
struct DockerComposeUpCommand {
    file: PathBuf,

    #[arg(long = "profile")]
    profiles: Vec<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::DockerCompose(command)) => run_docker_compose(command),
        None => run_tui(cli.background),
    }
}

fn run_tui(background: BackgroundMode) -> Result<()> {
    let mut terminal = setup_terminal()?;
    let app = App::new(
        WorkspaceBrowserData::sample(),
        background.force_background(),
    );
    let result = run_app(&mut terminal, app);
    restore_terminal()?;
    result
}

fn run_docker_compose(command: DockerComposeCommand) -> Result<()> {
    let runtime = Runtime::new().context("failed to create async runtime")?;

    match command.command {
        DockerComposeSubcommand::Up(command) => runtime.block_on(run_docker_compose_up(command)),
    }
}

async fn run_docker_compose_up(command: DockerComposeUpCommand) -> Result<()> {
    let project = ComposeProject::from_path(&command.file)
        .with_context(|| format!("failed to read compose file {}", command.file.display()))?;
    configure_docker_host();
    let executor = ComposeExecutor::new().with_context(docker_connection_context)?;
    let running = executor
        .up(&project, &command.profiles)
        .await
        .with_context(|| {
            format!(
                "failed to start compose project from {}",
                command.file.display()
            )
        })?;

    println!(
        "Started {} container(s). Press Ctrl+C to stop.",
        running.containers.len()
    );

    if let Some(proxy) = running.plan.proxy.as_ref() {
        for route in &proxy.routes {
            println!("http://{}", route.hostname);
        }
    }

    tokio::signal::ctrl_c()
        .await
        .context("failed while waiting for Ctrl+C")?;

    println!("Stopping containers...");
    executor
        .down(running)
        .await
        .context("failed to stop compose project")?;
    println!("Containers stopped.");

    Ok(())
}

fn configure_docker_host() {
    if std::env::var_os("DOCKER_HOST").is_some() {
        return;
    }

    if let Some(socket) = detect_docker_socket() {
        unsafe {
            std::env::set_var("DOCKER_HOST", format!("unix://{}", socket.display()));
        }
    }
}

fn detect_docker_socket() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let candidates = [
        home.join(".docker/run/docker.sock"),
        home.join(".colima/default/docker.sock"),
        home.join(".rd/docker.sock"),
        home.join(".orbstack/run/docker.sock"),
        PathBuf::from("/var/run/docker.sock"),
    ];

    candidates.into_iter().find(|path| path.exists())
}

fn docker_connection_context() -> String {
    let configured = std::env::var("DOCKER_HOST").ok();
    let detected = detect_docker_socket()
        .map(|path| format!("unix://{}", path.display()))
        .unwrap_or_else(|| "none detected".to_string());

    match configured {
        Some(host) => format!(
            "failed to connect to Docker using DOCKER_HOST={host}. On macOS, ensure Docker Desktop/Colima/OrbStack is running and the Docker socket is reachable"
        ),
        None => format!(
            "failed to connect to Docker. On macOS, ensure Docker Desktop/Colima/OrbStack is running. Common sockets checked: {detected}, /var/run/docker.sock"
        ),
    }
}

fn run_app(terminal: &mut DefaultTerminal, mut app: App) -> Result<()> {
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
