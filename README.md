# Commandant

Orchestrate AI coding agents across worker nodes.

One binary, `commandant`, is at once:

- the **orchestrator** (`commandant server`): the control plane, holding the node
  registry, task history and auth;
- the **worker** (`commandant worker`): runs on each machine that executes work,
  optionally hosting a coding agent (`--harness opencode`);
- the **CLI** (`commandant node|run|prompt|tui|task|token|login`): how you drive it all.

Workers *dial into* the orchestrator and keep one gRPC stream open, so they work
behind NAT, from laptops and from Docker without opening any port.

A task is either a shell command (`commandant run`) or a prompt for the node's
coding agent (`commandant prompt`). The agent harness supported today is
[OpenCode](https://opencode.ai), which the worker installs and runs for you.

Documentation: [architecture and diagrams](docs/architecture.md) ·
[crates and modules](docs/crates.md)

---

## Install

Requires Rust 1.88+ (or just Docker, see below).

```sh
cargo install --path crates/commandant-cli     # installs `commandant` into ~/.cargo/bin
```

## Quick start

**1. Start the orchestrator** on the machine that will coordinate everything:

```sh
commandant server --local-worker
```

It prints a **connection link**:

```
Commandant is listening on 0.0.0.0:7400. Connection link:

    commandant://AQAdN1FCkeiI9FArDH_E6d…

  Add a worker:    commandant worker commandant://AQAdN1FCkeiI9FArDH_E6d…
  Control it:      commandant login commandant://AQAdN1FCkeiI9FArDH_E6d…
```

`--local-worker` also runs a worker inside the server process, so this machine
can execute tasks too. Leave it out for a coordinator-only server.

**2. Add workers**: on every other machine, paste the link:

```sh
commandant worker commandant://AQAdN1FCkeiI9FArDH_E6d…
```

The worker saves its own credentials, so afterwards `commandant worker` with no
arguments is enough.

**3. Connect your client** (laptop or anywhere), once:

```sh
commandant login commandant://AQAdN1FCkeiI9FArDH_E6d…
```

**4. Use it:**

```sh
commandant node ls
commandant run my-box -- git status
commandant task ls
```

**5. Add a coding agent** (optional): start a worker with `--harness opencode`
(or the server with `--local-worker --harness opencode`), then:

```sh
commandant prompt my-box --cwd ~/src/app "why does the build fail?"
commandant tui my-box --cwd ~/src/app    # or chat with it
```

See [Coding agents](#coding-agents) below.

### About the link

- **Stable.** The token is created on first start and kept in the data dir, so the
  link survives restarts. It is also written to `<data-dir>/link`.
- **Host.** It is the machine's primary IP by default. If that isn't what others
  reach it on (a WSL VM, a cloud box behind NAT, a Tailscale network), pass
  `--advertise <host or host:port>` once. It is remembered.
- **Same machine.** When a worker or client runs on the orchestrator's own
  machine, it notices and connects via `127.0.0.1`.
- **Obfuscated, not encrypted.** The token and address are scrambled so they
  aren't readable at a glance, and a checksum catches copy/paste mistakes. But
  anyone holding the link can decode it. Treat it like a password.
  Hand-written links like `commandant://<token>@<host>:<port>` are accepted too.
- **Powerful.** The link holds the admin token: whoever has it controls the
  cluster and every worker. Keep it private. For a machine you trust less, give
  it a worker-only link instead (see `token create` below).

---

## CLI reference

Every command has `--help`. Client commands find the orchestrator from, in order:
`--addr`/`--token`/`--token-file` flags, `COMMANDANT_ADDR`/`COMMANDANT_TOKEN`/
`COMMANDANT_TOKEN_FILE`, then the file written by `login`
(`~/.config/commandant/config.toml`).

### `commandant server`

| Option | Env | Default | |
|---|---|---|---|
| `--listen` | `COMMANDANT_LISTEN` | `0.0.0.0:7400` | Address to listen on |
| `--data-dir` | `COMMANDANT_DATA_DIR` | `~/.local/share/commandant/server` | Database, token, link |
| `--advertise` | `COMMANDANT_ADVERTISE` | primary IP | Host put in the link (remembered) |
| `--local-worker` | `COMMANDANT_LOCAL_WORKER` | off | Also run a worker in-process |
| `--harness` | `COMMANDANT_HARNESS` | none | Coding agent for the local worker (`opencode`) |

### `commandant worker [LINK]`

| Option | Env | |
|---|---|---|
| `LINK` | `COMMANDANT_LINK` | Link from `commandant server`; only needed the first time |
| `--name` | `COMMANDANT_NODE_NAME` | Node name (defaults to the hostname; must be unique) |
| `--state-dir` | `COMMANDANT_STATE_DIR` | Where credentials live (default `~/.local/share/commandant/worker`) |
| `--server` / `--join-token` | `COMMANDANT_SERVER` / `COMMANDANT_JOIN_TOKEN` | URL + token, as an alternative to a link |
| `--harness` | `COMMANDANT_HARNESS` | Coding agent to host (`opencode`); installed if missing |

To run several workers on one machine, give each its own `--state-dir` and `--name`.
The worker reconnects on its own (backoff up to 30 s) if the orchestrator restarts.

### Client commands

| Command | |
|---|---|
| `commandant login <LINK>` | Save address and admin token (`login <URL> --with-token cmda_…` also works) |
| `commandant node ls` | List nodes with online status, hostname, platform, harness, last seen |
| `commandant node rm <node>` | Forget a node (it must rejoin with a token) |
| `commandant run <node> [--cwd DIR] [-e K=V]… -- <cmd> [args…]` | Run a command and stream its output |
| `commandant prompt <node> [-s SESSION] [--cwd DIR] [-m PROVIDER/MODEL] [--agent NAME] [--effort E] <prompt>…` | Ask the node's coding agent and stream its reply |
| `commandant tui [node] [-s SESSION] [--cwd DIR] [-m PROVIDER/MODEL] [--agent NAME] [--effort E]` | Chat with the node's coding agent in a terminal UI |
| `commandant task ls [--limit N]` | Recent tasks and their status |
| `commandant task cancel <id>` | Cancel a running task |
| `commandant token create [--ttl 1h] [--reusable]` | Worker-only join token and link (`--ttl 0` = never expires) |

`<node>` can be a name, a full id or an unambiguous id prefix; task ids accept
prefixes too.

About `run`:
- The command runs directly (no shell). Use `sh -c '…'` for pipes and variables.
- stdout and stderr stream back separately, and `commandant` exits with the
  remote exit code. Scripts can rely on it.
- The first **Ctrl-C** cancels the remote task, killing its whole process group.
  A second one detaches.

Task statuses: `running`, `succeeded` (exit 0), `failed`, `cancelled`, and
`lost` (the node disconnected or the orchestrator restarted mid-task).

---

## Coding agents

A worker started with `--harness opencode`:

1. finds `opencode` on its `PATH` or in `~/.opencode/bin`. If it's missing, the
   worker installs it with the official script (`curl` and `bash` needed),
   leaving shell profiles untouched;
2. runs `opencode serve` on a random loopback port, behind a random password,
   and restarts it if it dies. It is stopped with the worker;
3. announces the harness, so `node ls` shows it.

`commandant prompt` then works like `run`:

```sh
commandant prompt my-box --cwd ~/src/app "add a test for the parser"
# …the agent's reply streams on stdout, its tool calls on stderr:
# [opencode] edit src/parser_test.rs
commandant: continue with --session ses_1f3a…
commandant prompt my-box -s ses_1f3a… "now make it pass"
```

- **Sessions.** Each prompt starts a new OpenCode session, unless `--session`
  continues one (in that session's directory). The id is printed at the end.
- **Directory.** `--cwd` on the node. It defaults to the session's directory,
  else the worker's.
- **Model, agent and effort.** `--model provider/model`, `--agent build|plan|…`
  and `--effort low|high|…` (the model's thinking effort, which OpenCode calls a
  variant) override OpenCode's defaults. Providers are configured on the node the usual
  OpenCode way: API keys in the environment, `opencode auth login`, or
  `~/.config/opencode`. With none, OpenCode's free models are used.
- **Ctrl-C** aborts the agent (status `cancelled`); a second one detaches.
  The exit code is 0 when the agent finished its turn, 1 on error.
- **Permissions.** Nobody is there to answer OpenCode's permission prompts, so
  the worker grants them once and notes it on stderr. That gives nothing an
  admin can't already do with `run`.

### Chatting in the terminal

`commandant tui` holds the same conversation in a terminal UI, keeping the
session from one prompt to the next:

```sh
commandant tui my-box --cwd ~/src/app
```

```
┌ opencode on my-box ──────────────────────────────────────────────────────┐
│● online   host my-box   os linux/x86_64   worker 0.1.0      ⠋ working 4s│
│session ses_1f3a…   cwd ~/src/app                                         │
└──────────────────────────────────────────────────────────────────────────┘
┌ Thread ──────────────────────────────────────────────────────────────────┐
│› add a test for the parser                                               │
│I'll add a round-trip test.                                               │
│  ⚙ edit src/parser_test.rs                                               │
└──────────────────────────────────────────────────────────────────────────┘
┌ build · anthropic/claude-sonnet-5 · high effort ─────────────────────────┐
│› now make it pass                                                        │
└──────────────── Esc cancel · PgUp/PgDn scroll · Ctrl-C quit ─────────────┘
```

The prompt box's title shows the agent, model and effort the next prompt uses.

The node defaults to the only online one with a harness.

| Key or command | Does |
|---|---|
| **Enter** | Send the prompt |
| **Tab** / **Shift-Tab** | Next / previous agent (`build`, `plan`, …) |
| `/model [filter]` | Choose a model in a floating window |
| `/effort [filter]` | Choose the thinking effort the model offers |
| **Ctrl-T** | Next thinking effort |
| `/agent [filter]` | Choose an agent in a floating window |
| `/new` | Start a new session |
| **Esc** | Cancel the agent's turn |
| **PgUp** / **PgDn** | Scroll the thread |
| **Ctrl-C**, `/quit` | Quit, cancelling a running turn, and print the session id for `prompt -s` or `tui -s` |

In the floating window, typing narrows the list (every word must match),
**↑/↓** move, **Enter** chooses and **Esc** closes. The agents, models and
efforts come from the node's OpenCode: subagents and deprecated models are
left out. Changing the model drops an effort it doesn't offer.

---

## Docker

The image (`docker/Dockerfile`) contains the `commandant` binary plus `git` and
`curl` (for installing a harness).

### Orchestrator

One command, which prints the link at the end:

```sh
docker compose -f docker/orchestrator.compose.yml up -d --build && \
  docker compose -f docker/orchestrator.compose.yml exec server cat /data/link
```

Settings (export them, or put them in a `.env` file):

| Variable | Default | |
|---|---|---|
| `COMMANDANT_LOCAL_WORKER` | `false` | Also run a worker in the container |
| `COMMANDANT_ADVERTISE` | primary IP | Host put in the link |
| `COMMANDANT_BIND` / `COMMANDANT_PORT` | `0.0.0.0` / `7400` | Listen address |
| `COMMANDANT_IMAGE` | `commandant:dev` | Use a prebuilt image instead of building |

It uses host networking, so the link carries the machine's real IP (on Docker
Desktop, enable host networking in its settings). Admin commands work from
inside the container: `docker compose -f docker/orchestrator.compose.yml exec server commandant node ls`.

### Workers

On each worker machine:

```sh
COMMANDANT_LINK='commandant://…' docker compose -f docker/worker.compose.yml up -d --build
```

Credentials are kept in a volume, so later starts need no link. Add
`COMMANDANT_HARNESS=opencode` to host a coding agent: it is installed on first
start and kept, along with its sessions and logins, in the `home` volume. Pass
your provider's API key through `environment` in the compose file. For several
workers on one host, give each a project name:
`COMMANDANT_LINK='…' COMMANDANT_NODE_NAME=box-2b docker compose -p box-2b -f docker/worker.compose.yml up -d`.

### Single-host dev cluster

`docker-compose.yml` at the root runs an orchestrator and two workers together:

```sh
docker compose up -d server
export COMMANDANT_JOIN_TOKEN=$(docker compose exec server commandant token create --reusable)
docker compose --profile workers up -d
docker compose exec server commandant node ls
```

---

## Security

- All traffic is **plaintext gRPC** (no TLS yet). Keep port 7400 on a private
  network (VPN, Tailscale, WireGuard) or behind an SSH tunnel. Never expose it
  on the public internet.
- Tokens are 256-bit random values. The orchestrator stores only their SHA-256
  hashes, and local secrets are written `0600`.
- Workers run commands as the user they run as, with no sandbox. Run them under
  a dedicated user or in a container. The same goes for a hosted coding agent,
  whose permission prompts are granted automatically.
- A harness server listens on loopback only and requires a random password.

## Files on disk

| Where | What |
|---|---|
| `~/.local/share/commandant/server/commandant.db` | SQLite: tokens (hashed), nodes, task history |
| `~/.local/share/commandant/server/admin.token` | The admin token |
| `~/.local/share/commandant/server/link` · `advertise` | Current link · remembered `--advertise` |
| `~/.local/share/commandant/server/local-worker/` | State of the `--local-worker` |
| `~/.local/share/commandant/worker/node.json` | Worker node id, secret and orchestrator URL |
| `~/.config/commandant/config.toml` | Client address and token (from `login`) |

## Development

```sh
cargo test --workspace                     # unit tests + in-process orchestrator/worker e2e tests
cargo clippy --workspace --all-targets
RUST_LOG=debug commandant server           # verbose logs (any command)
```

The gRPC contract is in `crates/commandant-proto/proto/commandant.proto`. See
[docs/](docs/architecture.md) for how the pieces fit together.
