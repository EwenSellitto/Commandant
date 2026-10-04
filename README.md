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
coding agent (`commandant prompt`). The agent harnesses supported today are
[OpenCode](https://opencode.ai) and [Claude Code](https://code.claude.com),
which the worker installs and runs for you. Claude Code always runs on your
Claude subscription, never an API key (see below).

Documentation: [architecture and diagrams](docs/architecture.md) ·
[crates and modules](docs/crates.md)

---

## Install

Requires Rust 1.88+, `pkg-config` and OpenSSL's headers (`libssl-dev` on
Debian, `openssl-devel` on Fedora) for git; or just Docker, see below.

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

    commandant://AuMPUzAc6IS5ocQ1a_7AWy_ocXe-CvHX8jw

  Add a worker:    commandant worker commandant://AuMPUzAc6IS5ocQ1a_7AWy_ocXe-CvHX8jw
  Control it:      commandant login commandant://AuMPUzAc6IS5ocQ1a_7AWy_ocXe-CvHX8jw
```

`--local-worker` also runs a worker inside the server process, so this machine
can execute tasks too. Leave it out for a coordinator-only server.

**2. Add workers**: on every other machine, paste the link:

```sh
commandant worker commandant://AuMPUzAc6IS5ocQ1a_7AWy_ocXe-CvHX8jw
```

The worker saves its own credentials, so afterwards `commandant worker` with no
arguments is enough.

**3. Connect your client** (laptop or anywhere), once:

```sh
commandant login commandant://AuMPUzAc6IS5ocQ1a_7AWy_ocXe-CvHX8jw
```

**4. Use it:**

```sh
commandant node ls
commandant run my-box -- git status
commandant task ls
```

**5. Add a coding agent** (optional): start a worker with `--harness opencode`
(or the server with `--local-worker --harness opencode`). Or start one on a
running node: `commandant node start-agent my-box opencode`, or open the node in
`commandant tui`, which offers the agents it can host. Then:

```sh
commandant prompt my-box --cwd ~/src/app "why does the build fail?"
commandant tui my-box --cwd ~/src/app    # or chat with it
```

See [Coding agents](#coding-agents) below.

### About the link

- **Stable.** The token is created on first start and kept in the database, so
  the link survives restarts. It is also written to `<data-dir>/link`, and the
  token to `<data-dir>/admin.token`.
- **Reset.** `commandant server --reset` deletes the database (nodes, tokens,
  task history) and starts afresh with a new link. It asks twice (`y`, then
  typing `reset`), needs a terminal, and keeps the `--advertise` host.
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
| `--reset` | | off | Delete the database and start afresh, after confirming twice |
| `--advertise` | `COMMANDANT_ADVERTISE` | primary IP | Host put in the link (remembered) |
| `--local-worker` | `COMMANDANT_LOCAL_WORKER` | off | Also run a worker in-process |
| `--harness` | `COMMANDANT_HARNESS` | none | Coding agent for the local worker (`opencode`, `claude-code`) |
| `--harness-bin` | `COMMANDANT_HARNESS_BIN` | on `PATH` | The agent's binary for the local worker (`--opencode-bin` still works) |

### `commandant worker [LINK]`

A worker refuses to run as root, and so does the server's `--local-worker`: it
runs whatever it is told, so it gets one ordinary user's rights. The Docker
image runs as its own `commandant` user (uid 1000).

| Option | Env | |
|---|---|---|
| `LINK` | `COMMANDANT_LINK` | Link from `commandant server`; only needed the first time |
| `--name` | `COMMANDANT_NODE_NAME` | Node name (defaults to the hostname; must be unique) |
| `--state-dir` | `COMMANDANT_STATE_DIR` | Where credentials live (default `~/.local/share/commandant/worker`) |
| `--server` / `--join-token` | `COMMANDANT_SERVER` / `COMMANDANT_JOIN_TOKEN` | URL + token, as an alternative to a link |
| `--harness` | `COMMANDANT_HARNESS` | Coding agent to host (`opencode`, `claude-code`); installed if missing |
| `--harness-bin` | `COMMANDANT_HARNESS_BIN` | The agent's binary (`opencode`, `claude`) to run, instead of the one on `PATH` (or installed); `--opencode-bin` still works |

Several workers can run on one machine. Each locks its state directory while it
runs, so no two are ever the same node. Started without `--state-dir`, a worker
takes the first free one (`worker`, `worker-2`, …) and, without `--name`, the
name `<hostname>`, `<hostname>-2`, …: just start as many as you like. An explicit
`--state-dir` that another worker holds is refused. (A `server --local-worker`
already uses this machine's hostname as its name.)

A worker reconnects with the credentials it saved. If the orchestrator no longer
knows them (it was reset, say) and a link is given, it joins again as a new node
and saves the new credentials. A node removed with `node rm` while running is
disconnected at once and stops. If the same credentials are used by two
workers (a state directory copied to another machine, say), the one that
connected first is told so and stops, rather than the two taking turns.
The worker reconnects on its own (backoff up to 30 s) if the orchestrator restarts.

### Client commands

| Command | |
|---|---|
| `commandant login <LINK>` | Save address and admin token (`login <URL> --with-token cmda_…` also works) |
| `commandant node ls` | List nodes with online status, hostname, platform, harness, last seen |
| `commandant node rm <node>` | Forget a node (it must rejoin with a token) |
| `commandant node start-agent <node> <harness>` | Start a coding agent on a node that has none, installing it if missing; it runs until the worker stops |
| `commandant node sessions <node>` | The agent sessions saved on the node, latest first, and which are working |
| `commandant node commands <node>` | The commands and skills of the node's coding agent |
| `commandant node mcp <node> [--connect NAME \| --disconnect NAME]` | The agent's MCP servers and their status; connects or disconnects one first |
| `commandant run <node> [--cwd DIR] [-e K=V]… -- <cmd> [args…]` | Run a command and stream its output |
| `commandant prompt <node> [-s SESSION] [--cwd DIR] [-m PROVIDER/MODEL] [--agent NAME] [--effort E] [-c COMMAND] <prompt>…` | Ask the node's coding agent and stream its reply |
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

### Working on a project

A node clones repositories to work in, each clone a **copy** of the project
with an id of its own: `<state-dir>/projects/<name>/<id>`. Sessions can share
a copy or each have their own.

In a TUI chat:

- `/project` browses the node's projects: for each, **+ new copy**, then its
  copies with their branch and what the sessions in them are about. Choosing
  a copy joins it; the session works there alongside the others.
- `/project <repository>` clones a new copy (a URL, or the name of a project
  the node already has).

Only a session that hasn't started yet can move into a project; Ctrl-N starts
a new one in the same folder. The node clones with git built in (libgit2),
signing in like git would: the SSH agent, `~/.ssh/id_ed25519`, `id_ecdsa` or
`id_rsa`, or the credential helper for HTTPS. It never asks for a password,
so a private repository needs one of those set up on the node. Branches and
worktrees are left to the agent.

### Claude Code, on your subscription

`--harness claude-code` (or `node start-agent my-box claude-code`, or the TUI)
runs the real `claude` CLI, installed with Anthropic's installer if missing,
once per prompt (`claude -p` with streamed JSON). Its own settings, skills,
agents, hooks and MCP servers apply, and sessions are its own transcripts, so
`claude --resume` picks them up too.

It only ever uses your Claude subscription:

- `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN` are removed from its
  environment, and a turn is stopped as soon as Claude Code says it found an
  API key anywhere else (settings, `apiKeyHelper`).
- Sign the node in once: run `claude` on it and `/login`, or run
  `claude setup-token` where a browser is and paste the token in the TUI
  (`/providers`). The token is kept in the worker's state directory (0600).
- No cost is shown: the subscription's usage limits apply instead.

Permissions are skipped (`--dangerously-skip-permissions`), as nobody is there
to answer them. Its MCP servers are managed with `claude mcp` on the node.

A worker started with `--harness opencode`, or asked to start it later (by
`node start-agent` or the TUI; a worker started without `--harness` hosts none
until asked, even if it hosted one before it restarted):

1. finds `opencode` on its `PATH` or in `~/.opencode/bin`. If it's missing, the
   worker installs it with the official script (`curl` and `bash` needed),
   leaving shell profiles untouched;
2. runs `opencode serve` on a random loopback port, behind a random password,
   and restarts it if it dies. It is stopped with the worker;
3. announces the harness, so `node ls` shows it.

`commandant prompt` then works like `run`:

```sh
commandant prompt my-box --cwd ~/src/app "add a test for the parser"
# …the agent's reply streams on stdout, its tool calls on stderr
# (its thinking is left out):
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
- **Commands and skills.** `--command review` runs one of the agent's commands
  or skills (listed by `node commands`), with the prompt as its arguments,
  which are then optional.
- **Ctrl-C** aborts the agent (status `cancelled`); a second one detaches.
  The exit code is 0 when the agent finished its turn, 1 on error.
- **Permissions.** Nobody is there to answer OpenCode's permission prompts, so
  the worker grants them once and notes it on stderr. That gives nothing an
  admin can't already do with `run`.

### Chatting in the terminal

`commandant tui` holds the same conversation in a terminal UI, keeping the
session from one prompt to the next. It opens on the list of nodes; **Enter**
opens one.

```
 Commandant  2 nodes · 2 online

 ▌ ● my-box   opencode · linux/x86_64   2 open · 1 working
   ● lab-box  opencode · linux/x86_64
```

Each node can have several chats, each its own session, and they all run at
once, on one node or several: start a long refactor, open another chat for a
question, and look at the nodes list to see what is still working.

```sh
commandant tui my-box --cwd ~/src/app    # straight to a chat on my-box
```

```
 ● my-box  opencode · linux/x86_64 · v0.1.0    ses_1f3a… · ~/src/app
  1 add a test for the parser   2 update the docs ⠋       ctrl-n new  ctrl-o sessions

 ▌ add a test for the parser

 ┊ The parser has no tests yet; a round-trip one covers the most.
 ⚙ edit src/parser_test.rs
   I added a **round-trip** test in `src/parser_test.rs`.
 ◆ build · Claude Sonnet 5 · high · 18.2s · 24.7k in · 412 out · $0.08

 ⠋ writing 4s  esc to cancel
 ▌
 ▌ now make it pass
 ▌
 build · Claude Sonnet 5 · high effort      24.9k/200k 12% · $0.08
```

- The reply streams in as the model writes it, with light markdown styling.
  The model's thinking streams too, dimmed behind `┊`, and tool calls show
  as `⚙`.
- `◆` closes each turn: the agent, model, effort, time, tokens and cost.
- Opening a node with no agent offers the ones its worker can host. Choosing one
  starts it there (installing it first, which can take a few minutes) and
  opens a chat once it's ready.
- The tabs are the node's chats. A spinner means that chat is working, a
  green `●` that it finished out of sight, a red `✗` that it failed.
- The prompt sits on a solid slab edged in the agent's color. Under it are the
  agent, model and effort the next prompt uses, how full the context window
  is, and what the session has cost (as OpenCode reckons it).

| Key or command | Does |
|---|---|
| **Enter** | Send the prompt |
| **Tab** / **Shift-Tab** | Next / previous agent (`build`, `plan`, …) |
| `/model [filter]` | Choose a model in a floating window |
| `/effort [filter]` | Choose the thinking effort the model offers |
| **Ctrl-T** | Next thinking effort |
| `/agent [filter]` | Choose an agent in a floating window |
| `/<command> [args]` | Run one of the agent's commands or skills |
| `/skills`, `/commands` | Choose a command or skill in a floating window, then type its arguments |
| `/mcp` | The agent's MCP servers and their status; **Enter** connects or disconnects one |
| **Esc** | Cancel the agent's turn (even before it has started) |
| **PgUp** / **PgDn** | Scroll the thread |
| **Ctrl-N**, `/new` | Another chat on this node, in a new session, while the others keep working |
| **Ctrl-O**, `/sessions` | Switch to one of the node's chats, or resume a session it saved (with its agent, model and cost; earlier messages aren't shown) |
| **Alt-←** / **Alt-→** | Previous / next chat on this node |
| **Ctrl-W**, `/close` | Close the chat (once its agent is idle) |
| **Ctrl-G**, `/nodes` | Back to the nodes; chats keep working there |
| **Ctrl-C**, `/quit` | Quit, cancelling every running turn, and print each session's `tui -s` command |

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
