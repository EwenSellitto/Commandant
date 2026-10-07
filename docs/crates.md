# Code layout

Six crates, building two binaries: `commandant-server` (server and worker)
and `commandant` (the client). For how they work together at runtime, see
[architecture.md](architecture.md).

```mermaid
flowchart TD
    SERVER["commandant-server<br/>the commandant-server binary"]
    CLIENT["commandant-client<br/>the commandant binary"]
    ORCH["commandant-orchestrator"]
    WORK["commandant-worker"]
    PROTO["commandant-proto<br/>gRPC contract"]
    COMMON["commandant-common"]
    SERVER --> ORCH & WORK
    CLIENT --> PROTO
    ORCH --> PROTO
    WORK --> PROTO
    PROTO --> COMMON
```

The server and the worker are libraries, so `commandant-server` and the
end-to-end tests both run them in-process. The client depends on neither: no
SQLite or libgit2 in `commandant`. Code used by several crates goes in
`commandant-proto` (anything gRPC) or `commandant-common` (everything else).

## `commandant-common`

| File | |
|---|---|
| `lib.rs` | `DEFAULT_PORT`, `VERSION`, `or`, `random_hex` for ids and secrets |
| `link.rs` | The `commandant://` link: encoding, parsing, addresses, loopback for the local machine |
| `harness.rs` | `HarnessKind`: the coding agents a worker can host, by name and description |
| `fs.rs` | Private (0600) files, written atomically; reading optional and single-value files; deleting a file or directory |
| `dirs.rs` | Default data, state and config directories |
| `lookup.rs` | Finding a node by name, id or id prefix, and a task by id or id prefix |
| `time.rs` | Unix time, and `12s ago` for tables |

## `commandant-proto`

| File | |
|---|---|
| `proto/commandant.proto` | The contract: `NodeLink` (worker ⇄ server) and `Control` (client → server) |
| `src/lib.rs` | Generated types, `.into()` for message envelopes, the `Reply` trait for answers to questions, and connecting (`channel` tries every address of a link) |

`protoc` is vendored: editing the `.proto` only needs a rebuild.

## `commandant-orchestrator`

| File | |
|---|---|
| `lib.rs` | Opens the data directory, sets up the admin token, serves both services |
| `link.rs` | `NodeLink`: admits workers (join or reconnect) and handles their messages |
| `control.rs` | `Control`: the admin API; sends tasks and questions to workers |
| `registry.rs` | Which node is connected, on which connection, with which harnesses |
| `tasks.rs` | Fans live task output out to watching clients, keeping its tail |
| `output.rs` | The last 1 MiB of a task's output, chunks tagged with their stream |
| `queries.rs` | Matches workers' answers to the questions asked, by request id |
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
| `tests/e2e.rs` | End-to-end tests with a real server and workers in-process |
| `tests/fake_opencode/` | A fake OpenCode the tests drive (`hold`, `fail`, `permission` in a prompt) |

## `commandant-client`

| File | |
|---|---|
| `main.rs`, `cli.rs` | Logging, and the command line (clap) |
| `admin.rs`, `run.rs` | `login`, `token`, `node`, `task`; `run`, `prompt` and `task watch` |
| `config.rs` | Where clients find the server and token |
| `state/mod.rs` | What the client knows and does, apart from how it is shown: the nodes and every chat. No terminal code |
| `state/intent.rs`, `state/pick.rs` | What goes in (intents, updates) and comes out (effects, where to go, prompt edits); pickers as data |
| `state/chat/` | One chat: its session, settings, thread and line language (`/model`, `/project`, the agent's `/skill`); its pickers, provider sign-in and projects in their own files |
| `tui/mod.rs` | The TUI's event loop: draws, reads keys, makes the calls effects ask for |
| `tui/app.rs` | The screen shown, the highlighted node, the last chat per node; routes keys to intents |
| `tui/chat.rs` | A chat's prompt (editing, history recall, completion highlight), scroll and picker as shown |
| `tui/ui/`, `tui/text.rs` | Drawing, and markdown styling |
| `tui/picker.rs` | The floating list to choose from |

## Other files

| Path | |
|---|---|
| `docker/Dockerfile` | The image: `commandant-server`, `git`, `curl`, run as user `commandant` |
| `docker/*.compose.yml` | Orchestrator and worker deployments |
| `docker-compose.yml` | A server and two workers on one host, for development |
