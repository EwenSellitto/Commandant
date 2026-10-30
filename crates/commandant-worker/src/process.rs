//! The processes a worker starts: kept from the worker's secrets, and
//! signalled each in its own process group so a signal also reaches
//! whatever they started.

use std::ffi::OsStr;

use tokio::process::{Child, Command};

/// Variables that may hold the worker's link or a token. Its commands and
/// agents never inherit them: an agent running `env` must not find them.
const SECRETS: [&str; 3] = [
    "COMMANDANT_JOIN_TOKEN",
    "COMMANDANT_LINK",
    "COMMANDANT_TOKEN",
];

/// Every process the worker starts begins here: `program`, without the
/// worker's secrets in its environment.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    for secret in SECRETS {
        command.env_remove(secret);
    }
    command
}

/// What to ask of a process group: to stop, or to die now.
pub enum Signal {
    Term,
    Kill,
}

/// Sends `signal` to the child's process group. Returns false where there is
/// none to signal: the child has exited, or this isn't Unix.
pub fn signal_group(child: &Child, signal: Signal) -> bool {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        let signal = match signal {
            Signal::Term => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
        };
        // SAFETY: plain syscall; the group id equals the child's pid.
        unsafe { libc::kill(-(pid as libc::pid_t), signal) };
        return true;
    }
    // Used only on Unix.
    let _ = (child, signal);
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn started_processes_inherit_no_secret() {
        let command = command("env");
        let removed: Vec<_> = command
            .as_std()
            .get_envs()
            .filter_map(|(key, value)| value.is_none().then_some(key))
            .collect();
        // `get_envs` lists them sorted, as `SECRETS` is.
        assert_eq!(removed, SECRETS.map(OsStr::new));
    }
}
