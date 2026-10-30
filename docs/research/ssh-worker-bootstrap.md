# Getting a worker onto a fresh machine over SSH

Research for [Getting a worker onto a fresh machine over SSH](https://github.com/EwenSellitto/Commandant/issues/77),
part of [Fleet file: declarative node and harness setup](https://github.com/EwenSellitto/Commandant/issues/75).
Throwaway branch, never merged. Researched 2026-10-30.

**Question.** Given an SSH user and host, how does `commandant` (the client, on
Linux or Windows) turn a fresh Linux machine into a joined worker?

## What a worker needs today (from this repo)

- One binary, `commandant-server`, run as `commandant-server worker [LINK]`.
  The link can also come from `COMMANDANT_LINK`; `--name`/`COMMANDANT_NODE_NAME`
  and `--state-dir`/`COMMANDANT_STATE_DIR` are env-settable too
  (`crates/commandant-server/src/cli.rs`).
- It refuses to run as root (`normal_user` in `crates/commandant-worker/src/lib.rs`).
- On first join it saves `node.json` (0600) in its state dir, by default
  `~/.local/share/commandant/worker` (`dirs::worker_state`). After that it needs
  no link: a restart reconnects with the saved node secret.
- It locks its state dir. **Without `--state-dir`** a second worker silently
  takes `<dir>-2` and becomes a *new node* (`pick_free_state_dir`). A service
  should pass `--state-dir` explicitly so an overlapping restart fails loudly
  instead of creating `host-2`.
- If saved credentials are rejected on the first connection *and* a join token
  was given, it forgets them and rejoins with the token. So a permanent admin
  link left in the service's environment would re-add a node after `node rm`;
  a one-time `cmdj_` token (Control `CreateJoinToken`, default TTL 1 h) can't.
- It handles only Ctrl-C (`tokio::signal::ctrl_c`); systemd stops with SIGTERM,
  whose default action kills the process. That is fine (state is on disk
  already, systemd kills the whole cgroup, so harness children go too).
- Linux build deps: libgit2 is vendored, but OpenSSL and libssh2 come from the
  system via pkg-config (`git2` features in the root `Cargo.toml`); SQLite is
  bundled by sqlx's `sqlite` feature ([sqlx Cargo.toml][sqlx-toml]). TLS uses ring.
- No releases and no release workflow: CI only runs tests (`.github/workflows/ci.yml`).
  The repo is public.

## (a) SSH from the client

| | System `ssh` (shell out) | `russh` | `ssh2` (libssh2) | `openssh` crate |
|---|---|---|---|---|
| Windows | Yes, if the OpenSSH client is present | Yes (pure Rust) | Yes (C lib) | **No**: "only works on unix" [openssh] |
| `~/.ssh/config` | All of it | Subset via `russh-config`: Host, HostName, User, Port, IdentityFile, ProxyCommand, ProxyJump, AddKeysToAgent, UserKnownHostsFile, StrictHostKeyChecking; no Include/Match [russh-config-src] | None [ssh2] | All (wraps `ssh`) |
| Agent | All (Unix socket, Windows agent service, FIDO, 1Password...) | `SSH_AUTH_SOCK`; on Windows Pageant and named pipe (`cfg(windows)` in source) [russh-agent-src] | `Agent` [ssh2] | All |
| known_hosts | OpenSSH's own, incl. hashed hosts | helpers `check_known_hosts` / `learn_known_hosts` [russh-kh] | `KnownHosts::check` [ssh2] | OpenSSH's |
| Passwords / passphrase / 2FA prompts | Free (inherits the terminal) | You write the prompts | You write the prompts | Key auth only [openssh] |
| Cost | No dependency | Big dep; default crypto is aws-lc-rs (needs `ring` feature to match our stack) [russh-features] | C build; another libssh2 | n/a |

Facts that decide it:

- Windows OpenSSH is a "feature on demand" since Windows 10 1809 / Server 2019;
  Microsoft's table says "Not installed" on Windows 10 1809+ and Server
  2019/2022, "Installed but not enabled" on Server 2025, and gives no row for
  Windows 11 [ms-overview]. Third-party reports say the *client* is preinstalled
  since 1803, so in practice `ssh.exe` is usually there, but the client must
  check `ssh` is on `PATH` and otherwise point to
  `Add-WindowsCapability -Online -Name OpenSSH.Client~~~~0.0.1.0` [ms-install].
- The Windows `ssh-agent` service "is disabled by default" [ms-keys]; without
  it, each new `ssh` invocation asks for the key passphrase again.
- Windows OpenSSH has no ControlMaster: "ControlMaster is not supported on
  Windows yet", issue still open [win32-405]. So on Windows each `ssh`/`scp`
  call is a fresh connection and a fresh authentication.
- Host keys: `StrictHostKeyChecking=accept-new` adds unknown hosts and refuses
  changed keys; `BatchMode=yes` disables every prompt (passwords and host-key
  questions) [ssh_config].

**Recommendation: shell out to the system `ssh`**, and do the whole bootstrap
in **one** connection: `ssh [user@]host sh -s` with a POSIX script on stdin
(the binary, if pushed, goes through a second `ssh host 'cat > file'`, no `scp`
needed). One connection means one authentication on Windows too, and the user's
config, agent, jump hosts and known_hosts all work unchanged. A library only
pays off if we later need many round-trips per node or a GUI-driven password
prompt.

**Decisions for you** (they overlap the map's open item "SSH authentication and
trusting a new host's key"):
1. Unknown host key: trust on first use (`accept-new`), or refuse until the
   user has connected once by hand (`BatchMode=yes` + default `ask`)?
2. Allow interactive password/passphrase prompts (inherit the terminal), or
   key/agent only (`BatchMode=yes`, fails fast, safer for applying many nodes)?

## (b) Getting `commandant-server` onto the node

Options:

1. **Publish GitHub release binaries, node downloads them.** Asset URL
   `https://github.com/EwenSellitto/Commandant/releases/latest/download/<asset>`
   [gh-links] (or `/releases/download/<tag>/<asset>` to pin a version). The repo
   is public, so no auth. Standard runners, including `ubuntu-24.04-arm`, are
   "free and unlimited on public repositories" [gh-runners], so both arches
   build natively. Static musl builds run on any distro:
   `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` are Tier 2 with
   host tools (musl 1.2.5) [rust-platforms]. Our only blocker is the system
   OpenSSL/libssh2: git2's `vendored-openssl` feature turns on
   `openssl-sys/vendored` [git2-features], and libssh2-sys depends on the same
   `openssl-sys`, so Cargo feature unification vendors it for both
   [libgit2-sys-toml][libssh2-sys-toml]. Building OpenSSL from source needs perl
   and a musl C compiler (e.g. `musl-tools` on the x86_64 runner; same on the
   arm runner). Not yet tried here; verify with a build.
2. **Copy from the client.** Works offline, but the client may be Windows or a
   different arch and has no Linux binary. Only useful combined with (1): the
   client downloads the right asset and pushes it (`ssh host 'cat > ...'`), for
   nodes without internet. Nodes need internet anyway to install a harness and
   reach a model provider, so this is a niche.
3. **Build on the node** (`cargo install --git`). Needs Rust 1.95+, a C
   toolchain, pkg-config and OpenSSL headers (README "Install"), so usually
   sudo, and minutes of compile per node. Fine as a fallback for an
   arch with no release asset.

Arch detection: `uname -m` in the bootstrap script: `x86_64` and `aarch64`
(some systems print `arm64`) map to the two musl assets; anything else fails
with a clear message (or falls back to (3)). Install to
`~/.local/bin/commandant-server` (no sudo), write to a temp name and `mv` so a
running binary is never half-written. Check `commandant-server --version`
(clap `version` is on) to skip the download when it already matches.

**Recommendation:** (1), with a release workflow on tag push building the two
musl assets plus a `SHA256SUMS` the script checks. Fallback to (3) only on an
unknown arch.

**Decisions for you:** whether to cut releases at all (a tag-triggered workflow
and versioned tags); which version a node gets (the client's own version, so
client and node match, vs `latest`); whether build-on-node is worth supporting.
Upgrading on re-apply is the map's separate open item.

## (c) Running the worker as a non-root service

**systemd user unit + lingering**

- Unit at `~/.config/systemd/user/commandant-worker.service`, `WantedBy=default.target`,
  `Restart=always`, `ExecStart=%h/.local/bin/commandant-server worker --state-dir ...`.
  Enabled with `systemctl --user enable --now`.
- Without lingering the user manager stops at the last logout: pam_systemd
  starts `user@.service` on login and "If the last concurrent session of a user
  ends, the user's systemd instance will be terminated too" [pam_systemd].
  With lingering "a user manager is spawned for the user at boot and kept
  around after logouts" [loginctl].
- **No sudo needed upstream:** polkit action `org.freedesktop.login1.set-self-linger`
  defaults to `yes` for any, inactive and active sessions (only lingering
  *another* user, `set-user-linger`, needs `auth_admin_keep`) [login1-policy].
  So `loginctl enable-linger` (no argument) works for the SSH user over SSH.
  Distros can override polkit rules; the script should check
  `loginctl show-user $USER -p Linger` and report if it is still `no`.
- `systemctl --user` over SSH needs `XDG_RUNTIME_DIR`, which pam_systemd sets
  for the session's own user (not after `su`/`sudo -u`) [pam_systemd]. So run it
  as the SSH user directly.
- Runs as the SSH user, with their home: the harness, its logins
  (`claude /login`, `opencode auth login`) and git's SSH keys are that user's,
  matching how the README tells people to sign a node in.

**System unit with a dedicated user**

- `/etc/systemd/system/commandant-worker.service` with `User=commandant`;
  `useradd`, writing the unit and `systemctl enable` all need root (sudo).
- Survives reboot without lingering; no dependence on logind/polkit.
- The worker then lives in a different home from the SSH user, so signing in a
  harness on the node means `sudo -u commandant -i`. Same model as the Docker
  image (`useradd ... commandant`).
- A system unit with `User=<ssh user>` is a middle ground: needs sudo once, no
  linger, same home.

**No systemd** (containers, WSL without systemd, Alpine/OpenRC): neither works;
the script should detect `systemctl` missing and stop with a clear message (or
fall back to `nohup` that won't survive a reboot). Docker nodes already have
`docker/worker.compose.yml`.

**Joining under a service.** The unit should not carry the link forever. Order:
client creates a one-time join token (`CreateJoinToken`); script writes it to
an env file (0600, `COMMANDANT_LINK=...`) read by the unit
(`EnvironmentFile=-...`), starts the unit, waits until `node.json` exists in
the state dir, then deletes the env file. Reboots after that use `node.json`.

**Recommendation:** user unit + `loginctl enable-linger` as the default: no sudo,
one user, works over a plain SSH login. Offer the system unit only if a node has
no lingering (polkit override) and the user has sudo.

**Decisions for you:** worker runs as the SSH user (user unit) or a dedicated
user (system unit, sudo); and what happens if the SSH user is root, since the
worker refuses root: create a `commandant` user (system unit) or refuse.

## Answer in one paragraph

Shell out to the system `ssh` (present on Windows 10/11 as OpenSSH, an optional
feature to enable if missing) and run one POSIX script over a single connection,
so the user's keys, agent, `~/.ssh/config` and known_hosts just work. The script
detects the arch with `uname -m`, downloads a static musl `commandant-server`
from a GitHub release (needs a new tag-triggered workflow and git2's
`vendored-openssl`) into `~/.local/bin`, writes a systemd user unit with an
explicit `--state-dir`, runs `loginctl enable-linger` (no sudo by default
polkit), and starts it with a one-time join token that is removed once
`node.json` exists.

## Sources

- [openssh]: https://docs.rs/openssh/latest/openssh/
- [russh-features]: https://docs.rs/crate/russh/latest/features (russh 0.65.0)
- [russh-kh]: https://docs.rs/russh/latest/russh/keys/known_hosts/index.html
- [russh-agent-src]: https://github.com/Eugeny/russh/blob/main/russh/src/keys/agent/client.rs
- [russh-config-src]: https://github.com/Eugeny/russh/blob/main/russh-config/src/lib.rs
- [ssh2]: https://docs.rs/ssh2/latest/ssh2/ (0.9.6)
- [ms-overview]: https://learn.microsoft.com/en-us/windows-server/administration/openssh/openssh-overview
- [ms-install]: https://learn.microsoft.com/en-us/windows-server/administration/openssh/openssh_install_firstuse
- [ms-keys]: https://learn.microsoft.com/en-us/windows-server/administration/openssh/openssh_keymanagement
- [win32-405]: https://github.com/PowerShell/Win32-OpenSSH/issues/405
- [ssh_config]: https://man.openbsd.org/ssh_config
- [gh-links]: https://docs.github.com/en/repositories/releasing-projects-on-github/linking-to-releases
- [gh-runners]: https://docs.github.com/en/actions/reference/runners/github-hosted-runners
- [rust-platforms]: https://doc.rust-lang.org/nightly/rustc/platform-support.html
- [git2-features]: https://docs.rs/crate/git2/0.21.0/features
- [libgit2-sys-toml]: https://github.com/rust-lang/git2-rs/blob/master/libgit2-sys/Cargo.toml
- [libssh2-sys-toml]: https://github.com/alexcrichton/ssh2-rs/blob/master/libssh2-sys/Cargo.toml
- [sqlx-toml]: https://github.com/launchbadge/sqlx/blob/main/Cargo.toml
- [login1-policy]: https://github.com/systemd/systemd/blob/main/src/login/org.freedesktop.login1.policy
- [loginctl]: https://www.freedesktop.org/software/systemd/man/latest/loginctl.html
- [pam_systemd]: https://www.freedesktop.org/software/systemd/man/latest/pam_systemd.html

[openssh]: https://docs.rs/openssh/latest/openssh/
[russh-features]: https://docs.rs/crate/russh/latest/features
[russh-kh]: https://docs.rs/russh/latest/russh/keys/known_hosts/index.html
[russh-agent-src]: https://github.com/Eugeny/russh/blob/main/russh/src/keys/agent/client.rs
[russh-config-src]: https://github.com/Eugeny/russh/blob/main/russh-config/src/lib.rs
[ssh2]: https://docs.rs/ssh2/latest/ssh2/
[ms-overview]: https://learn.microsoft.com/en-us/windows-server/administration/openssh/openssh-overview
[ms-install]: https://learn.microsoft.com/en-us/windows-server/administration/openssh/openssh_install_firstuse
[ms-keys]: https://learn.microsoft.com/en-us/windows-server/administration/openssh/openssh_keymanagement
[win32-405]: https://github.com/PowerShell/Win32-OpenSSH/issues/405
[ssh_config]: https://man.openbsd.org/ssh_config
[gh-links]: https://docs.github.com/en/repositories/releasing-projects-on-github/linking-to-releases
[gh-runners]: https://docs.github.com/en/actions/reference/runners/github-hosted-runners
[rust-platforms]: https://doc.rust-lang.org/nightly/rustc/platform-support.html
[git2-features]: https://docs.rs/crate/git2/0.21.0/features
[libgit2-sys-toml]: https://github.com/rust-lang/git2-rs/blob/master/libgit2-sys/Cargo.toml
[libssh2-sys-toml]: https://github.com/alexcrichton/ssh2-rs/blob/master/libssh2-sys/Cargo.toml
[sqlx-toml]: https://github.com/launchbadge/sqlx/blob/main/Cargo.toml
[login1-policy]: https://github.com/systemd/systemd/blob/main/src/login/org.freedesktop.login1.policy
[loginctl]: https://www.freedesktop.org/software/systemd/man/latest/loginctl.html
[pam_systemd]: https://www.freedesktop.org/software/systemd/man/latest/pam_systemd.html
