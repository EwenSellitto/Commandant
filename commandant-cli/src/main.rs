use std::time::Duration;

use anyhow::{Context, Result};
mod app;
mod components;
mod terminal;
mod ui;

use app::App;
use clap::{Args, Parser, Subcommand, ValueEnum};
use commandant_core::browser::WorkspaceBrowserData;
use commandant_docker_compose::{ComposeExecutor, ComposeProject, docker_paths};
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
    file: std::path::PathBuf,

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
    docker_paths::configure_docker_host();
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
        if proxy.routes.is_empty() {
            println!("No exposed services found for Traefik.");
            println!(
                "Publish a port, add `expose:`, or set `com.commandant.expose: \"true\"` on a service to generate a subdomain."
            );
        } else {
            for route in &proxy.routes {
                println!("{} -> http://{}", route.service_name, route.hostname);
            }
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

fn docker_connection_context() -> String {
    let configured = std::env::var("DOCKER_HOST").ok();
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let checked = docker_paths::docker_socket_candidates(home.as_deref())
        .into_iter()
        .map(|path| format!("unix://{}", path.display()))
        .collect::<Vec<_>>()
        .join(", ");

    match configured {
        Some(host) => format!(
            "failed to connect to Docker using DOCKER_HOST={host}. {}",
            docker_platform_hint()
        ),
        None => format!(
            "failed to connect to Docker. {} Checked sockets: {}",
            docker_platform_hint(),
            checked
        ),
    }
}

#[cfg(target_os = "macos")]
fn docker_platform_hint() -> &'static str {
    "On macOS, ensure Docker Desktop, Colima, Rancher Desktop, or OrbStack is running and its Docker socket is reachable"
}

#[cfg(target_os = "linux")]
fn docker_platform_hint() -> &'static str {
    "On Linux, ensure Docker Engine or rootless Docker is running and your user can access the Docker socket"
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn docker_platform_hint() -> &'static str {
    "Ensure the Docker daemon is running and the Docker socket is reachable"
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
