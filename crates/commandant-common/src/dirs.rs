//! Default locations, following the platform conventions
//! (e.g. `~/.local/share/commandant` and `~/.config/commandant` on Linux).

use std::path::PathBuf;

use anyhow::{Context, Result};

fn project_dirs() -> Result<directories::ProjectDirs> {
    directories::ProjectDirs::from("", "", "commandant").context("cannot determine home directory")
}

/// Orchestrator data: database, admin token, link.
pub fn server_data() -> Result<PathBuf> {
    Ok(project_dirs()?.data_dir().join("server"))
}

/// Worker state: node credentials.
pub fn worker_state() -> Result<PathBuf> {
    Ok(project_dirs()?.data_dir().join("worker"))
}

/// Client configuration written by `commandant login`.
pub fn client_config() -> Result<PathBuf> {
    Ok(project_dirs()?.config_dir().join("config.toml"))
}
