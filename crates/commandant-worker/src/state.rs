//! What the worker keeps in its state directory: its credentials, and the
//! harness a client had it start. A running worker locks its directory, so
//! two workers never share a node's identity.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
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

/// How many workers can run side by side from one default state directory.
const MAX_INSTANCES: u32 = 64;

/// A state directory this worker holds until it is dropped (or exits).
#[derive(Debug)]
pub struct Claim {
    pub dir: PathBuf,
    /// 1 for the directory asked for, 2 for `<dir>-2`, and so on.
    pub instance: u32,
    _lock: File,
}

/// Locks `dir` for this worker. With `pick_free`, a directory another worker
/// holds gives way to the first free `<dir>-2`, `<dir>-3`…, each its own node.
pub fn claim(dir: &Path, pick_free: bool) -> Result<Claim> {
    for instance in 1..=MAX_INSTANCES {
        let candidate = match instance {
            1 => dir.to_path_buf(),
            n => {
                let mut name = dir.file_name().unwrap_or_default().to_os_string();
                name.push(format!("-{n}"));
                dir.with_file_name(name)
            }
        };
        match lock(&candidate)? {
            Some(lock) => {
                return Ok(Claim {
                    dir: candidate,
                    instance,
                    _lock: lock,
                });
            }
            None if pick_free => continue,
            None => bail!(
                "another worker is running from {}; give this one its own --state-dir",
                candidate.display()
            ),
        }
    }
    bail!(
        "{MAX_INSTANCES} workers are already running from {}",
        dir.display()
    )
}

/// The directory's lock, or `None` if another worker holds it.
fn lock(dir: &Path) -> Result<Option<File>> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join("lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => {
            Err(e).with_context(|| format!("locking {}", path.display()))
        }
    }
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
    fn a_state_dir_holds_one_worker_at_a_time() {
        let base = tempfile_dir().join("worker");
        let first = claim(&base, true).unwrap();
        assert_eq!((first.dir.as_path(), first.instance), (base.as_path(), 1));
        let err = claim(&base, false).unwrap_err();
        assert!(err.to_string().contains("its own --state-dir"), "{err}");

        // The next free one, and the one after.
        let second = claim(&base, true).unwrap();
        assert_eq!(second.instance, 2);
        assert_eq!(second.dir.file_name().unwrap(), "worker-2");
        let third = claim(&base, true).unwrap();
        assert_eq!(third.instance, 3);

        // A stopped worker frees its directory for the next.
        drop(second);
        assert_eq!(claim(&base, true).unwrap().instance, 2);
        drop(first);
        assert_eq!(claim(&base, false).unwrap().instance, 1);
        drop(third);
        std::fs::remove_dir_all(base.parent().unwrap()).unwrap();
    }

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
