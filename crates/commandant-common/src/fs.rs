//! Small file helpers for configuration and secrets.

use std::path::Path;

use anyhow::{Context, Result};

/// Writes a file only the current user can read, creating parent directories.
/// The new contents replace the old in one step, so a reader (or a crash)
/// never meets a half-written file.
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

/// Reads a file, or `None` if it doesn't exist.
pub fn read_optional(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Reads a single-value file (a token, a host...) without surrounding whitespace.
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
        std::fs::remove_dir_all(tmp).unwrap();
    }
}
