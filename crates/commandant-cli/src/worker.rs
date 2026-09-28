//! `commandant worker`: joins an orchestrator and runs what it's told.

use anyhow::{Result, bail};
use commandant_common::dirs;
use commandant_worker::WorkerConfig;

use crate::cli::WorkerArgs;

pub async fn run(args: WorkerArgs) -> Result<()> {
    let state_dir = match args.state_dir {
        Some(dir) => dir,
        None => dirs::worker_state()?,
    };
    let remembered_server = commandant_worker::state::load(&state_dir)?.and_then(|c| c.server);
    let (server, join_token) = match (args.link, args.server.or(remembered_server)) {
        (Some(link), _) => (link.addr(), Some(link.token)),
        (None, Some(server)) => (server, args.join_token),
        (None, None) => bail!(
            "pass the connection link printed by `commandant server`: commandant worker commandant://..."
        ),
    };
    let config = WorkerConfig {
        server,
        join_token,
        name: args.name,
        state_dir,
    };
    tokio::select! {
        result = commandant_worker::run(config) => result,
        _ = tokio::signal::ctrl_c() => Ok(()),
    }
}
