# Code layout

Seven crates, building two binaries: `commandant-server` (server and worker)
and `commandant` (the client). For how they work together at runtime, see
[architecture.md](architecture.md).

```mermaid
flowchart TD
    SERVER["commandant-server<br/>the commandant-server binary"]
    CLIENT["commandant-client<br/>the commandant binary"]
    CORE["commandant-client-core<br/>the client core"]
    ORCH["commandant-orchestrator"]
    WORK["commandant-worker"]
    PROTO["commandant-proto<br/>gRPC contract"]
    COMMON["commandant-common"]
    SERVER --> ORCH & WORK
    CLIENT --> CORE & PROTO
    CORE --> PROTO
    ORCH --> PROTO
    WORK --> PROTO
    PROTO --> COMMON
```

The server and the worker are libraries, so `commandant-server` and the
end-to-end tests both run them in-process. The client depends on neither: no
SQLite or libgit2 in `commandant`. The client core has no terminal or window
code, so every front end shares it. Code used by several crates goes in
`commandant-proto` (anything gRPC) or `commandant-common` (everything else).

## `commandant-common`

| File | |
|---|---|
| `lib.rs` | `DEFAULT_PORT`, `VERSION`, `or`, `random_hex` for ids and secrets |
| `link.rs` | The `commandant://` link: encoding, parsing, addresses, the pin of the server's certificate, loopback for the local machine |
| `harness.rs` | `HarnessKind`: the coding agents a worker can host, by name and description |
| `fs.rs` | Private (0600) files, written atomically; reading optional and single-value files; deleting a file or directory |
| `dirs.rs` | Default data, state and config directories |
| `lookup.rs` | Finding a node by name, id or id prefix, and a task by id or id prefix |
| `time.rs` | Unix time, `12s ago` for tables, and lifetimes as typed (`30m`, `1h`, `7d`) |

## `commandant-proto`

| File | |
|---|---|
| `proto/commandant.proto` | The contract: `NodeLink` (worker ⇄ server) and `Control` (client → server) |
| `src/lib.rs` | Generated types, `.into()` for message envelopes, the `Reply` trait for answers to questions, and connecting (`channel` tries every address of a link) |
| `src/tls.rs` | TLS to the server a link pins: only its certificate is accepted |

`protoc` is vendored: editing the `.proto` only needs a rebuild.

## `commandant-orchestrator`

| File | |
|---|---|
| `lib.rs` | Opens the data directory, sets up the admin token, serves both services over TLS |
| `tls.rs` | The server's self-signed certificate, made once and kept in the database |
| `link.rs` | `NodeLink`: admits workers (join or reconnect) and handles their messages |
| `control.rs` | `Control`: the admin API; sends tasks and questions to workers |
| `registry.rs` | Which node is connected, on which connection, with which harnesses |
| `tasks.rs` | Fans live task output out to watching clients, keeping its tail |
| `output.rs` | The last 1 MiB of a task's output, chunks tagged with their stream |
| `queries.rs` | Matches workers' answers to the questions asked, by request id and the connection asked |
| `store.rs` | SQLite: tokens, nodes, tasks and their output (migrations in `migrations/`) |
| `auth.rs` | Token generation, hashing, constant-time checks |

## `commandant-worker`

| File | |
|---|---|
| `lib.rs` | Connects and reconnects, then runs tasks and answers questions |
| `state.rs` | `node.json` (credentials), and the state-directory lock |
| `exec.rs` | Runs a command in its own process group and streams its output |
| `process.rs` | Signals a child's whole process group |
| `project.rs` | Clones and lists projects and their copies (git2) |
| `harness.rs` | The `Harness` trait, the `Host` of the running harness, and what harnesses share (session lock, installer, output writer) |
| `opencode/` | OpenCode: the server process (`mod.rs`), its HTTP API (`api.rs`), prompts (`prompt.rs`), options, sessions and sign-in |
| `claude/` | Claude Code: the CLI (`mod.rs`), one prompt's stream (`stream.rs`), sessions from its transcripts (`sessions.rs`) |

## `commandant-server`

| File | |
|---|---|
| `main.rs`, `cli.rs` | Logging, and the command line (clap) |
| `serve.rs`, `worker.rs` | `serve` (link, reset, local worker) and `worker` |
| `tests/e2e.rs` | End-to-end tests with a real server and workers in-process, and a real client core against them |
| `tests/fake_opencode/` | A fake OpenCode the tests drive (`hold`, `fail`, `permission` in a prompt) |

## `commandant-client`

| File | |
|---|---|
| `main.rs`, `cli.rs` | Logging, the tokio runtime, and the command line (clap); a bare `commandant` prints help in a terminal and opens the GUI elsewhere |
| `admin.rs`, `run.rs` | `login`, `token`, `node`, `task`; `run`, `prompt` and `task watch`: plain gRPC calls. `logout` forgets the saved server, and says if flags or the environment still name one; `link` prints the link, with no call |
| `start.rs` | What `tui` and `gui` give the core: new chats' defaults, and the chat `--node` and `--session` open first |
| `picker.rs` | A list narrowed down by typing, and what answers a choose-ask with one: the TUI drives it with keys, the GUI with its filter field |
| `tui/mod.rs` | The TUI's event loop: draws, reads keys, hands the core's updates back to it; the first-run screen before it when no server is saved, and again after `/logout` |
| `tui/app.rs` | The screen shown (nodes, a chat, the tasks or a task's output) and where Esc goes back to, the highlighted node and task, the last chat per node, the app's own line (`r` and `a` on the nodes), what to copy to the clipboard (OSC 52, `c` on a show-ask); routes keys to intents |
| `tui/login.rs` | The first-run screen: a link pasted when no server is saved, or after `/logout`; saved once it connects. Esc gives up connecting |
| `tui/chat.rs` | A chat's prompt (editing, history recall, completion highlight) and scroll |
| `tui/ui/`, `tui/text.rs` | Drawing (`tasks.rs`: the task list, and an output filling the screen; `login.rs`: the first-run screen), and markdown styling |
| `tui/asks.rs` | What keys say to an ask: the floating list, narrowed down by typing, for a choose-ask, a panel for a confirm- or show-ask; a chat's enter-ask is typed in its prompt, the app's in a box of its own. The app's and each chat's picker are shown from one map |
| `tui/help.rs` | The help (F1, `?` on the nodes, `/help`): every command, `!` to run one on the node, the agent's commands and skills, and the terminal's keys in one such list, searched by name, key, alias or what it does; choosing one runs it, starts it in the prompt, or presses the key |
| `gui/mod.rs` | The GUI (the `gui` feature, off by default): GPUI on the main thread, one window, dark; the login card, then the connected view |
| `gui/login.rs` | The login card: a link pasted on a first run (saved once it connects), or the saved server tried at start-up, with Retry and "Use a different link" when it can't be reached; after a confirmed log out, running tasks cancelled and the saved server forgotten, back to a blank card |
| `gui/connected.rs` | Once connected: the core, redrawn as a GPUI task hands it its updates; which chat is shown and the last one per node; where the keys go as questions come and go; the app's question, over the whole window; the Nodes view, the Tasks view or a task's output in place of the chat, closed (no longer refreshed, no longer watched) once a chat or another view is shown, and back from an output to the nodes it was run from; the node the Nodes view marks, opened when its agent starts; a chat closed from the sidebar moves the view only if it was the one shown; a node whose agent started opens if nothing else was; the sidebar's width and whether Ctrl-B folded it away; the core's notices as toasts at the bottom right (errors stay until dismissed, the rest fade); a top banner while the server is out of reach |
| `gui/chords.rs` | The TUI's chords, doing the same: Ctrl-N, Ctrl-O, Ctrl-W, Ctrl-G (the Nodes view, on the shown chat's node), Ctrl-H (the Tasks view, or back to it from an output with its filter kept), Alt-←/→ among the node's chats, Ctrl-T; and Ctrl-B. A question takes the keys instead: over the window, all of them; over the chat shown, all but Ctrl-B |
| `gui/asks.rs` | Every ask: choose-asks a filter-as-you-type list (↑↓, Enter, Esc) on the shared picker, enter-asks one field (masked when secret) that leaves the thread usable, confirm-asks Yes and No (Enter, Esc), show-asks Copy. A chat's question covers only its pane and stays with it |
| `gui/sidebar.rs` | The sidebar: every node (status dot, name, a spinner while it starts its agent, a `[+]` menu: New chat, or Sessions… to resume one) and its open chats beneath it, each with a spinner while working, a dot or cross for how it ended unseen, a mark while it needs input, and a hover ×; a click on an offline node with no chats opens the Nodes view on its row; then the Nodes and Tasks entries; its edge is dragged to resize it; footer: Link… (a show-ask to copy) and Log out (a confirm-ask) |
| `gui/chat.rs` | A chat in the main pane: header (a click on the directory browses the node's projects), thread, a strip while the node gets something ready (a project's copy, a resumed session's turns), and prompt (Enter sends, Shift+Enter breaks the line, Stop and Esc cancel, Tab completes or else switches agent, Shift+Tab back), the `/` completions above it (↑↓, Tab, a click), and the agent · model · effort chips under it with the session's usage |
| `gui/thread.rs` | The thread, a view of its own kept as last drawn while only the prompt changes, so typing doesn't lay it out again; its rows as each role is drawn: markdown replies with copy buttons, thinking folded once the turn ends, tool calls grouped, a command's block for each `!command`; raw HTML and images in replies shown as their text, never drawn or fetched |
| `gui/command.rs` | A command's block: the command as its header with a Stop that stops it alone while it runs, its output with error output tinted, and how it ended; header and output also apart, so an output view keeps the header in sight |
| `gui/nodes.rs` | The Nodes view: status dot, name, os/arch, version, agents and when an offline one was last seen; Open chat, Start agent on one hosting none (a spinner while it starts), Run command… (asked for, then its Output view), Remove (confirmed); Add node over them (asks the token's lifetime, then shows the worker's command) |
| `gui/tasks.rs` | The Tasks view: node, kind, status, age and what ran, newest first, filtered by a node dropdown, a running task's × cancels it, a pruned one greyed; a command's Output view with a back arrow, the command and Stop, node · directory · duration, how it ended, Copy-all, and what it printed kept at the end unless scrolled up |
| `gui/rows.rs` | The thread as rows, and what the scroller is told as it grows: the rows added, and the rows written to, such as a command's block still printing above later rows; nothing when it hasn't, so its scrollbar stays put; rows added or written to while scrolled up are the pill's "N new" |
| `gui/placement.rs` | Where the window was and how wide its sidebar when it last closed, stopped moving or was resized, so it reopens that way |

GPUI (`gpui-pre`, `gpui-pre-platform`), `gpui-component` and its icons
(`gpui-kit-assets`) are pinned with `=` and bumped together: their weekly
snapshots break freely.

Tests that open a window run on GPUI's test platform, with no display, under
the `gui-tests` feature: `cargo test --features commandant-client/gui-tests`.

## `commandant-client-core`

What any front end knows and does, apart from how it is shown. It depends on
the gRPC contract, common code, tokio and tonic, never on a terminal or window
library. Its `test-support` feature gives front ends' tests a core with no
server, which keeps the calls it would make.

| File | |
|---|---|
| `lib.rs` | `Core`: starts on a working connection, takes intents and updates, makes the calls they ask for on the tokio runtime it is given, and is read through `&` accessors; `shutdown` cancels running tasks; `server_reachable` is false while the node poll fails, true once it answers again |
| `calls.rs` | The connection to the server and every call made on it: prompt and command streams, questions to nodes, cancels, the 5-second node poll, task listings, the watches that feed output views and attached turns, join tokens (turned into a worker's link to the server as logged in to) and removing nodes |
| `config.rs` | Where clients find the server and token (none yet before the first log in), reading a pasted link, saving and forgetting the server, and connecting |
| `state/mod.rs` | What the client knows: the nodes, every chat, and the server's link for `/link` |
| `state/tasks/` | The task list, refreshed while open and filtered by node; output views fed by `WatchTask` (or by a command run on a node), followed while their task runs; opening a prompt task resumes its session, attached to its turn while that runs |
| `state/nodes/` | Managing the nodes: adding one (an enter-ask for its token's lifetime, then a show-ask with the worker's command), removing one once confirmed (its chats close), starting an agent on one that hosts none |
| `state/intent.rs`, `state/ask.rs` | What goes in (intents, updates) and comes out (effects, where to go, prompt edits); the asks (choose, enter, confirm, show), one per chat and one for the app |
| `state/chat/` | One chat: its session, settings, thread and line language (`/model`, `/project`, the agent's `/skill`, `!command`); what it offers to choose, provider sign-in, projects, and the commands run with `!` (`run.rs`: a block each, beside the agent's turn) in their own files |

## Other files

| Path | |
|---|---|
| `docker/Dockerfile` | The image: `commandant-server`, `git`, `curl`, run as user `commandant` |
| `docker/*.compose.yml` | Orchestrator and worker deployments |
| `docker-compose.yml` | A server and two workers on one host, for development |
| `rust-toolchain.toml` | The Rust version CI and local builds use |
