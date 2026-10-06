//! `commandant-server worker`: joins an orchestrator and runs what it's told.

use anyhow::Result;
use commandant_common::dirs;
use commandant_worker::WorkerConfig;

use crate::cli::WorkerArgs;

pub async fn run(args: WorkerArgs) -> Result<()> {
    // Without a state dir of its own, a worker sits beside any already running.
    let pick_free_state_dir = args.state_dir.is_none();
    let state_dir = match args.state_dir {
        Some(dir) => dir,
        None => dirs::worker_state()?,
    };
    let (server, join_token) = match args.link {
        Some(link) => (Some(link.addr()), Some(link.token)),
        None => (args.server, args.join_token),
    };
    let config = WorkerConfig {
        server,
        join_token,
        name: args.name,
        state_dir,
        pick_free_state_dir,
        harness: args.harness,
        harness_bin: args.harness_bin,
    };
    tokio::select! {
        result = commandant_worker::run(config) => result,
        _ = tokio::signal::ctrl_c() => Ok(()),
    }
}
