//! Default locations, following the platform conventions
//! (e.g. `~/.local/share/commandant` and `~/.config/commandant` on Linux).

use std::path::PathBuf;

use anyhow::{Context, Result};

fn project_dirs() -> Result<directories::ProjectDirs> {
    directories::ProjectDirs::from("", "", "commandant").context("cannot determine home directory")
}

/// Orchestrator data: database, admin token, link.
///
/// # Errors
///
/// If the home directory can't be determined.
pub fn server_data() -> Result<PathBuf> {
    Ok(project_dirs()?.data_dir().join("server"))
}

/// The link the orchestrator writes in its data directory.
pub const LINK_FILE: &str = "link";

/// Worker state: node credentials.
///
/// # Errors
///
/// If the home directory can't be determined.
pub fn worker_state() -> Result<PathBuf> {
    Ok(project_dirs()?.data_dir().join("worker"))
}

/// Where the GUI's window was when it last closed.
///
/// # Errors
///
/// If the home directory can't be determined.
pub fn gui_window() -> Result<PathBuf> {
    Ok(project_dirs()?.data_dir().join("gui").join("window.toml"))
}

/// Client configuration written by `commandant login`.
///
/// # Errors
///
/// If the home directory can't be determined.
pub fn client_config() -> Result<PathBuf> {
    Ok(project_dirs()?.config_dir().join("config.toml"))
}
