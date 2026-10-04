//! What the worker keeps in its state directory: its credentials, and the
//! harness a client had it start.

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

fn harness_path(state_dir: &Path) -> PathBuf {
    state_dir.join("harness")
}

/// The harness started from a client, to host again after a restart.
pub fn load_harness(state_dir: &Path) -> Result<Option<crate::HarnessKind>> {
    let path = harness_path(state_dir);
    read_optional(&path)?
        .map(|name| name.trim().parse().map_err(anyhow::Error::msg))
        .transpose()
        .with_context(|| format!("parsing {}", path.display()))
}

pub fn save_harness(state_dir: &Path, kind: crate::HarnessKind) -> Result<()> {
    write_private(&harness_path(state_dir), &format!("{kind}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_the_started_harness() {
        let dir = tempfile_dir();
        assert!(load_harness(&dir).unwrap().is_none());
        save_harness(&dir, crate::HarnessKind::Opencode).unwrap();
        assert_eq!(
            load_harness(&dir).unwrap(),
            Some(crate::HarnessKind::Opencode)
        );
        std::fs::write(harness_path(&dir), "claude\n").unwrap();
        assert!(
            load_harness(&dir).is_err(),
            "a harness this worker can't host"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn tempfile_dir() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("commandant-state-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
