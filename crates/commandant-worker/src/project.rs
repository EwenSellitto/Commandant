//! Projects: repositories a node clones to work in. Each has a shared clone,
//! `<state>/projects/<name>`, which every session given it works in, and any
//! number of copies of its own, `<state>/copies/<name>/<n>`, for sessions
//! that shouldn't step on the others. Worktrees are left to the harnesses.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail, ensure};
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::info;

// ponytail: one clone at a time per worker; per-project locks if nodes clone
// many projects at once.
static PREPARING: Mutex<()> = Mutex::const_new(());

/// Where to work on `repository` (a URL, or the name of a project already
/// cloned here): its shared clone, made if need be, or a new copy of it.
pub async fn prepare(state_dir: &Path, repository: &str, separate: bool) -> Result<PathBuf> {
    let repository = repository.trim();
    let name = name(repository)?;
    let _one_at_a_time = PREPARING.lock().await;
    let shared = state_dir.join("projects").join(&name);
    if !shared.exists() {
        ensure!(
            !is_name(repository),
            "this node has no project {name}; give its repository's URL"
        );
        clone(repository, &shared).await?;
    }
    if !separate {
        return Ok(shared);
    }
    let copies = state_dir.join("copies").join(&name);
    std::fs::create_dir_all(&copies).with_context(|| format!("creating {}", copies.display()))?;
    let copy = (2..)
        .map(|n| copies.join(n.to_string()))
        .find(|dir| std::fs::create_dir(dir).is_ok())
        .expect("a free number");
    // From the shared clone, which is quick, then pointed at the real origin.
    let local = shared.to_string_lossy();
    if let Err(e) = git(
        &["clone", "--quiet", "--", &local, &copy.to_string_lossy()],
        None,
    )
    .await
    {
        let _ = std::fs::remove_dir_all(&copy);
        return Err(e);
    }
    let origin = git(&["remote", "get-url", "origin"], Some(&shared)).await?;
    git(&["remote", "set-url", "origin", origin.trim()], Some(&copy)).await?;
    info!(copy = %copy.display(), "made another copy of {name}");
    Ok(copy)
}

/// Clones `url` into `dir`, through a temporary directory so a failed clone
/// leaves nothing that looks like a project.
async fn clone(url: &str, dir: &Path) -> Result<()> {
    let parent = dir.parent().expect("under projects/");
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let partial = parent.join(format!(
        ".cloning-{}",
        dir.file_name().unwrap_or_default().to_string_lossy()
    ));
    let _ = std::fs::remove_dir_all(&partial);
    info!(%url, "cloning");
    // `--` keeps a URL like `--upload-pack=…` from being taken as an option.
    if let Err(e) = git(
        &["clone", "--quiet", "--", url, &partial.to_string_lossy()],
        None,
    )
    .await
    {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(e);
    }
    std::fs::rename(&partial, dir).with_context(|| format!("moving the clone to {}", dir.display()))
}

/// Runs git, without ever waiting for a password nobody can type, and
/// returns what it printed.
async fn git(args: &[&str], dir: Option<&Path>) -> Result<String> {
    let mut command = Command::new("git");
    command
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let output = command
        .output()
        .await
        .context("running git (is it installed?)")?;
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr);
        let why = said
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        bail!("git {} failed: {why}", args[0]);
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Whether `repository` names a project rather than where to clone it from.
fn is_name(repository: &str) -> bool {
    !repository.contains(['/', ':', '\\'])
}

/// The project's name: the repository's last path segment, without `.git`.
fn name(repository: &str) -> Result<String> {
    let last = repository
        .trim_end_matches('/')
        .rsplit(['/', ':', '\\'])
        .next()
        .unwrap_or_default();
    let name = last.strip_suffix(".git").unwrap_or(last);
    let valid = !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    ensure!(valid, "can't tell a project name from {repository:?}");
    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_come_from_the_repository() {
        for (repository, expected) in [
            (
                "https://github.com/EwenSellitto/Commandant.git",
                "Commandant",
            ),
            ("git@github.com:me/my_app.git", "my_app"),
            ("https://example.com/a/tool/", "tool"),
            ("/srv/git/site.v2", "site.v2"),
            ("Commandant", "Commandant"),
        ] {
            assert_eq!(name(repository).unwrap(), expected, "{repository}");
        }
        for bad in ["", "../..", "a b", "x/.hidden"] {
            assert!(name(bad).is_err(), "{bad:?}");
        }
        assert!(is_name("Commandant") && !is_name("git@host:x"));
    }
}
