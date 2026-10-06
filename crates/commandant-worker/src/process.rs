//! Signalling the processes a worker starts, each in its own process group
//! so a signal also reaches whatever they started.

use tokio::process::Child;

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
