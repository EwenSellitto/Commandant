//! Small file helpers for configuration and secrets.

use std::path::Path;

use anyhow::{Context, Result};

/// Writes a file only the current user can read, creating parent directories.
/// The new contents replace the old in one step, so a reader (or a crash)
/// never meets a half-written file.
///
/// # Errors
///
/// If the directories can't be created or the file can't be written.
pub fn write_private(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let staging = path.with_file_name(name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
        .open(&staging)
        .and_then(|mut f| f.write_all(contents.as_bytes()))
        .and_then(|()| std::fs::rename(&staging, path))
        .with_context(|| format!("writing {}", path.display()))
}

/// Deletes a file, or a directory and all it holds; nothing there is fine.
/// Whether there was something.
///
/// # Errors
///
/// If something is there but can't be deleted.
pub fn remove(path: &Path) -> Result<bool> {
    let removed = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match removed {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("deleting {}", path.display())),
    }
}

/// Reads a file, or `None` if it doesn't exist.
///
/// # Errors
///
/// If it exists but can't be read as UTF-8 text.
pub fn read_optional(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// A single-value setting kept in a file, trimmed; `None` if the file is
/// missing or blank.
///
/// # Errors
///
/// As [`read_optional`].
pub fn read_setting(path: &Path) -> Result<Option<String>> {
    Ok(non_blank(read_optional(path)?))
}

/// `text` trimmed, unless that leaves nothing.
pub fn non_blank(text: Option<String>) -> Option<String> {
    text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

/// Reads a single-value file (a token, a host...) without surrounding whitespace.
///
/// # Errors
///
/// If it doesn't exist or can't be read as UTF-8 text.
pub fn read_trimmed(path: &Path) -> Result<String> {
    Ok(std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?
        .trim()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_private_files() {
        let tmp = std::env::temp_dir().join(format!("commandant-fs-{}", std::process::id()));
        let path = tmp.join("nested/secret");
        assert_eq!(read_optional(&path).unwrap(), None);
        write_private(&path, " value\n").unwrap();
        assert_eq!(read_trimmed(&path).unwrap(), "value");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(remove(&path).unwrap(), "it was there");
        assert!(!remove(&path).unwrap(), "nothing is left to remove");
        std::fs::remove_dir_all(tmp).unwrap();
    }
}
