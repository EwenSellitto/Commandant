//! Projects: repositories a node clones to work in. Each clone is a copy of
//! the project with an id of its own, `<state>/projects/<name>/<id>`, which
//! any number of sessions can join. Worktrees and branches are left to the
//! harnesses.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, ensure};
use commandant_proto::{AgentSession, Project, ProjectCopy};
use git2::build::RepoBuilder;
use git2::{Cred, CredentialType, FetchOptions, RemoteCallbacks, Repository};
use tracing::info;

const NO_CREDENTIALS: &str =
    "none of this node's credentials work for it: set up its SSH key or git credential helper";

/// Clones `repository` (a URL, or the name of a project already cloned here)
/// as a new copy of the project, and returns its id and where it is.
pub async fn prepare(state_dir: &Path, repository: &str) -> Result<(String, PathBuf)> {
    let repository = repository.trim();
    let name = name(repository)?;
    let project = state_dir.join("projects").join(&name);
    let url = if is_name(repository) {
        copies(&project)
            .iter()
            .find_map(|copy| origin(copy))
            .with_context(|| {
                format!("this node has no project {name}; give its repository's URL")
            })?
    } else {
        repository.to_string()
    };
    std::fs::create_dir_all(&project).with_context(|| format!("creating {}", project.display()))?;
    let (id, copy) = std::iter::repeat_with(new_id)
        .map(|id| (id.clone(), project.join(id)))
        .find(|(_, dir)| !dir.exists())
        .expect("a free id");
    info!(%url, copy = %copy.display(), "cloning");
    let target = copy.clone();
    tokio::task::spawn_blocking(move || clone(&url, &target)).await??;
    Ok((id, copy))
}

/// Clones `url` into `dir`, through a hidden directory so a failed clone
/// leaves nothing that looks like a copy.
fn clone(url: &str, dir: &Path) -> Result<()> {
    let name = dir.file_name().unwrap_or_default().to_string_lossy();
    let partial = dir.with_file_name(format!(".cloning-{name}"));
    let cloned = RepoBuilder::new()
        .fetch_options(fetch_options())
        .clone(url, &partial);
    if let Err(e) = cloned {
        let _ = std::fs::remove_dir_all(&partial);
        let why = match e.code() {
            git2::ErrorCode::Auth => NO_CREDENTIALS,
            _ => e.message(),
        };
        anyhow::bail!("cloning {url} failed: {why}");
    }
    std::fs::rename(&partial, dir).with_context(|| format!("moving the clone to {}", dir.display()))
}

/// Signs in the way git on this node would: the SSH agent, then the usual
/// key files, or the credential helper for HTTPS. Never asks anyone.
fn fetch_options() -> FetchOptions<'static> {
    let config = git2::Config::open_default().ok();
    let keys: Vec<PathBuf> = std::env::home_dir()
        .map(|home| home.join(".ssh"))
        .into_iter()
        .flat_map(|ssh| ["id_ed25519", "id_ecdsa", "id_rsa"].map(|key| ssh.join(key)))
        .filter(|key| key.is_file())
        .collect();
    // libgit2 asks again after each refusal: each way is tried once.
    let (mut ssh_tries, mut helper_tried, mut default_tried) = (0, false, false);
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(move |url, username, allowed| {
        let user = username.unwrap_or("git");
        if allowed.contains(CredentialType::USERNAME) {
            return Cred::username(user);
        }
        if allowed.contains(CredentialType::SSH_KEY) {
            ssh_tries += 1;
            if ssh_tries == 1 {
                return Cred::ssh_key_from_agent(user);
            }
            if let Some(key) = keys.get(ssh_tries - 2) {
                return Cred::ssh_key(user, None, key, None);
            }
        }
        if allowed.contains(CredentialType::USER_PASS_PLAINTEXT)
            && !std::mem::replace(&mut helper_tried, true)
            && let Some(config) = &config
        {
            return Cred::credential_helper(config, url, username);
        }
        if allowed.contains(CredentialType::DEFAULT) && !std::mem::replace(&mut default_tried, true)
        {
            return Cred::default();
        }
        Err(git2::Error::from_str(NO_CREDENTIALS))
    });
    let mut options = FetchOptions::new();
    options.remote_callbacks(callbacks);
    options
}

/// The projects cloned here, with their copies, latest first. `sessions`
/// (the harness's, latest first) say what each copy is being used for.
pub fn list(state_dir: &Path, sessions: &[AgentSession]) -> Vec<Project> {
    let mut projects: Vec<Project> = subdirs(&state_dir.join("projects"))
        .into_iter()
        .filter_map(|dir| {
            let copies = copies(&dir);
            let repository = copies.iter().find_map(|copy| origin(copy))?;
            Some(Project {
                name: dir.file_name()?.to_string_lossy().into_owned(),
                repository,
                copies: copies.iter().map(|copy| describe(copy, sessions)).collect(),
            })
        })
        .collect();
    projects.sort_by_key(|p| p.name.to_lowercase());
    projects
}

fn describe(path: &Path, sessions: &[AgentSession]) -> ProjectCopy {
    let branch = Repository::open(path)
        .ok()
        .and_then(|repo| repo.head().ok()?.shorthand().ok().map(str::to_string))
        .filter(|b| b != "HEAD")
        .unwrap_or_default();
    let here = canonical(path);
    ProjectCopy {
        id: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        path: path.to_string_lossy().into_owned(),
        branch,
        sessions: sessions
            .iter()
            .filter(|s| canonical(Path::new(&s.directory)) == here)
            .map(|s| commandant_common::or(&s.title, &s.id).to_string())
            .collect(),
    }
}

/// A project's copies, latest first.
fn copies(project: &Path) -> Vec<PathBuf> {
    let mut copies = subdirs(project);
    copies.sort_by_key(|dir| {
        let modified = dir.metadata().and_then(|m| m.modified()).ok();
        std::cmp::Reverse(modified.unwrap_or(SystemTime::UNIX_EPOCH))
    });
    copies
}

/// Visible subdirectories: a clone under way is hidden.
fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let hidden = |p: &Path| {
        p.file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with('.'))
    };
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && !hidden(p))
        .collect()
}

/// Where a copy was cloned from.
fn origin(copy: &Path) -> Option<String> {
    let repo = Repository::open(copy).ok()?;
    let remote = repo.find_remote("origin").ok()?;
    remote.url().ok().map(str::to_string)
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// A short random id for a copy, e.g. `3fa9c1d2`.
fn new_id() -> String {
    commandant_common::random_hex(4)
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
        assert_eq!(new_id().len(), 8);
    }
}
