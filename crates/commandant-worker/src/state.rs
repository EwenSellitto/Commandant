//! Credentials persisted in the worker's state directory.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use commandant_common::fs::{read_optional, write_private};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub node_id: String,
    pub secret: String,
    /// Orchestrator URL, so a restart needs no arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
}

fn path(state_dir: &Path) -> PathBuf {
    state_dir.join("node.json")
}

pub fn load(state_dir: &Path) -> Result<Option<Credentials>> {
    let path = path(state_dir);
    read_optional(&path)?
        .map(|json| serde_json::from_str(&json))
        .transpose()
        .with_context(|| format!("parsing {}", path.display()))
}

pub fn save(state_dir: &Path, creds: &Credentials) -> Result<()> {
    write_private(&path(state_dir), &serde_json::to_string_pretty(creds)?)
}
