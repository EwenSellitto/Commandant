//! `commandant server`: the orchestrator, plus the link to reach it.

use std::io::{BufRead, IsTerminal, Write};
use std::net::SocketAddr;
use std::path::Path;

use anyhow::{Context, Result, bail};
use commandant_common::link::{self, Link};
use commandant_common::{dirs, fs};
use commandant_orchestrator::{ADMIN_TOKEN_FILE, DB_FILE, Orchestrator};
use commandant_worker::WorkerConfig;
use tokio::task::JoinHandle;

use crate::cli::ServerArgs;

const LINK_FILE: &str = "link";
const ADVERTISE_FILE: &str = "advertise";
const LOCAL_WORKER_DIR: &str = "local-worker";

pub async fn run(args: ServerArgs) -> Result<()> {
    let data_dir = match args.data_dir {
        Some(dir) => dir,
        None => dirs::server_data()?,
    };
    // Bound first, so a reset never pulls the database from under a server
    // that is already running here.
    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("binding {}", args.listen))?;
    if args.reset {
        reset(&data_dir)?;
    }
    let orchestrator = Orchestrator::open(&data_dir).await?;

    let host = advertised_host(&data_dir, args.advertise, args.listen)?;
    let link = Link::new(orchestrator.admin_token(), &host);
    fs::write_private(&data_dir.join(LINK_FILE), &format!("{link}\n"))?;
    print_welcome(args.listen, &link);

    let local_worker = args.local_worker.then(|| {
        spawn_local_worker(WorkerConfig {
            server: format!("http://{}", reachable_locally(args.listen)),
            join_token: Some(orchestrator.admin_token().to_string()),
            name: None,
            state_dir: data_dir.join(LOCAL_WORKER_DIR),
            harness: args.harness,
        })
    });

    let served = orchestrator
        .serve(listener, async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;
    // Dropping the worker stops its harness; exiting the process would not.
    if let Some(worker) = local_worker {
        worker.abort();
        let _ = worker.await;
    }
    served
}

/// Deletes the database and what only works with it: the admin token, the
/// link and the local worker's credentials. The advertised host stays.
fn reset(data_dir: &Path) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        bail!("--reset asks for confirmation, so it needs a terminal");
    }
    eprintln!(
        "\nThis deletes every node, join token and task recorded in {}.\n\
         The admin token changes: the current link stops working, clients must\n\
         log in again and every worker must join again.\n",
        data_dir.display()
    );
    if !ask("Reset the database? [y/N] ")?.eq_ignore_ascii_case("y") {
        bail!("reset cancelled");
    }
    if ask("Type \"reset\" to confirm: ")? != "reset" {
        bail!("reset cancelled");
    }
    for file in [
        DB_FILE.into(),
        format!("{DB_FILE}-wal"),
        format!("{DB_FILE}-shm"),
        ADMIN_TOKEN_FILE.into(),
        LINK_FILE.into(),
    ] {
        let path = data_dir.join(file);
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(e).with_context(|| format!("deleting {}", path.display()));
            }
            _ => {}
        }
    }
    let worker = data_dir.join(LOCAL_WORKER_DIR);
    match std::fs::remove_dir_all(&worker) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(e).with_context(|| format!("deleting {}", worker.display()));
        }
        _ => {}
    }
    eprintln!("Database reset.");
    Ok(())
}

/// Prints `question` and reads one line, trimmed.
fn ask(question: &str) -> Result<String> {
    eprint!("{question}");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(answer.trim().to_string())
}

fn print_welcome(listen: SocketAddr, link: &Link) {
    eprintln!("\nCommandant is listening on {listen}. Connection link:\n\n    {link}\n");
    eprintln!("  Add a worker:    commandant worker {link}");
    eprintln!("  Control it:      commandant login {link}\n");
    eprintln!("Anyone with this link controls the cluster; keep it private.\n");
}

/// `host:port` for the link, in order of preference: `--advertise` (and
/// remember it), the remembered value, a specific listen address, this
/// machine's primary IP.
fn advertised_host(
    data_dir: &Path,
    advertise: Option<String>,
    listen: SocketAddr,
) -> Result<String> {
    let remembered = data_dir.join(ADVERTISE_FILE);
    let host = match non_blank(advertise) {
        Some(host) => {
            fs::write_private(&remembered, &format!("{host}\n"))?;
            host
        }
        None => match non_blank(fs::read_optional(&remembered)?) {
            Some(host) => host,
            None => detected_host(listen),
        },
    };
    Ok(link::with_port(&host, listen.port()))
}

fn detected_host(listen: SocketAddr) -> String {
    if !listen.ip().is_unspecified() {
        return listen.ip().to_string();
    }
    match link::primary_ip() {
        Some(ip) => ip.to_string(),
        None => {
            tracing::warn!("no network address found; the link only works locally");
            "127.0.0.1".into()
        }
    }
}

fn non_blank(text: Option<String>) -> Option<String> {
    text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

/// `listen`, with a wildcard address replaced by loopback.
fn reachable_locally(listen: SocketAddr) -> SocketAddr {
    let ip = listen.ip();
    let ip = if ip.is_unspecified() {
        link::loopback_of_same_family(ip)
    } else {
        ip
    };
    SocketAddr::new(ip, listen.port())
}

fn spawn_local_worker(config: WorkerConfig) -> JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(e) = commandant_worker::run(config).await {
            tracing::error!("local worker stopped: {e:#}");
        }
    })
}
