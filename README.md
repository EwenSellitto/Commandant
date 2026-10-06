# Commandant

Run commands and AI coding agents on your machines from one place.

Two binaries:

- `commandant-server` (Linux) plays two roles:
  - **server** (`serve`): the orchestrator. It knows the nodes, keeps the task
    history and checks tokens.
  - **worker** (`worker`): runs on each machine that does the work, optionally
    with a coding agent ([OpenCode](https://opencode.ai) or
    [Claude Code](https://code.claude.com)).
- `commandant` is the **client**: the commands you type (`node`, `run`,
  `prompt`, `tui`, …).

Workers connect *out* to the server, so they work behind NAT, on laptops and in
Docker without opening a port.

More detail: [how it works](docs/architecture.md) · [code layout](docs/crates.md)

## Install

Needs Rust 1.89+. The server binary also needs `pkg-config` and OpenSSL's
headers (`libssl-dev` on Debian, `openssl-devel` on Fedora), or use
[Docker](#docker).

```sh
cargo install --path crates/commandant-server   # machines that serve or work
cargo install --path crates/commandant-client   # machines you control them from
```

## Quick start

```sh
# 1. On the machine that coordinates (also runs a worker there):
commandant-server serve --local-worker
#    It prints a link: commandant://AuMPUzAc6IS5ocQ1a_7AWy_ocXe-CvHX8jw

# 2. On every other machine:
commandant-server worker commandant://…

# 3. On your laptop, once:
commandant login commandant://…

# 4. Use it:
commandant node ls
commandant run my-box -- git status
commandant tui                       # chat with the nodes' coding agents
```

A worker remembers its credentials: afterwards `commandant-server worker` alone is enough.

### The link

- It holds the server's address and the **admin token**: whoever has it controls
  every worker. Keep it private. For a machine you trust less, use a
  worker-only link from `commandant token create`.
- It survives restarts. It is saved in `~/.local/share/commandant/server/link`.
- If others reach the server on another address (a public name, a VPN, a port
  forward), pass `--advertise host[,host2…]` once; it is remembered. Clients try
  every address and use the first that answers.
- `commandant-server serve --reset` wipes nodes, tokens and history and makes a new link.

## Coding agents

Start a worker with an agent, or add one later:

```sh
commandant-server worker --harness opencode  # or claude-code
commandant node start-agent my-box opencode  # on a running node; the TUI offers it too
```

The worker installs the agent if it is missing. Then:

```sh
commandant prompt my-box --cwd ~/src/app "why does the build fail?"
commandant prompt my-box -s <session> "now fix it"     # continue a session
```

The reply streams on stdout, tool calls on stderr. Ctrl-C cancels the turn.
`--model`, `--agent`, `--effort` and `--command` choose how the agent works.
Nobody is there to answer the agent's permission prompts, so they are granted.

### OpenCode

Runs one `opencode serve` per worker, shared by every session. Sign it in to
model providers from the TUI (`/providers`), with `opencode auth login` on the
node, or with API keys in its environment. Without any, it uses OpenCode's free
models.

### Claude Code

Runs the real `claude` CLI, with your own settings, skills and MCP servers, and
**only on your Claude subscription**: API key variables are removed, and a turn
stops if Claude Code reports an API key. Sign the node in once, either with
`claude` then `/login` on it, or by pasting a token from `claude setup-token`
in the TUI (`/providers`). No cost is shown; your plan's usage limits apply.

### Projects

A node can clone repositories to work in. Each clone is a **copy** with an id
of its own (`<state-dir>/projects/<name>/<id>`); sessions can share a copy or
each have their own.

- `/project` in the TUI lists the node's projects and their copies, with each
  copy's branch and what its sessions are about. Pick a copy to join it, or
  **+ new copy**.
- `/project <repository URL or name>` clones a new copy.

The node signs in to git with its SSH agent, its SSH keys, or git's credential
helper. It never asks for a password.

## The terminal UI

`commandant tui` opens on the list of nodes. Each node can have several chats
(sessions) working at once.

```
 ● my-box  opencode · linux/x86_64 · v0.1.0    ses_1f3a… · ~/src/app
  1 add a test for the parser   2 update the docs ⠋       ctrl-n new  ctrl-o sessions

 ▌ add a test for the parser

 ┊ The parser has no tests yet; a round-trip one covers the most.
 ⚙ edit src/parser_test.rs
   I added a round-trip test in `src/parser_test.rs`.
 ◆ build · Claude Sonnet 5 · high · 18.2s · 24.7k in · 412 out · $0.08

 ▌ now make it pass
 build · Claude Sonnet 5 · high effort      24.9k/200k 12% · $0.08
```

`┊` is the model's thinking, `⚙` a tool call, `◆` a turn's summary. Type `/`
to see the commands, or `/help` for all commands and keys. The main ones:

| Key or command | Does |
|---|---|
| **Enter** / **Esc** | Send / cancel the turn |
| **Tab**, `/agent` | Switch agent |
| `/model`, `/effort` (**Ctrl-T**) | Choose the model and its thinking effort |
| `/skills`, `/mcp` | The agent's commands and skills; its MCP servers |
| `/project`, `/providers` | Work on a project; sign in to a model provider |
| **Ctrl-N**, **Ctrl-O** | New chat; switch to a chat or resume a saved session |
| **↑** / **↓** | Earlier prompts |
| **Ctrl-G** / **Ctrl-C** | Back to the nodes / quit |

## Command reference

Every command has `--help`.

`commandant-server` (bare, it prints help):

| Command | |
|---|---|
| `serve [--local-worker] [--harness H] [--advertise HOSTS] [--reset]` | Run the orchestrator (port 7400) |
| `worker [LINK] [--harness H] [--name N] [--state-dir DIR]` | Run a worker |

`commandant` finds the server from `--addr`/`--token`, then
`COMMANDANT_ADDR`/`COMMANDANT_TOKEN`, then what `login` saved:

| Command | |
|---|---|
| `login LINK` | Save the server and admin token for the client commands |
| `node ls` · `node rm NODE` | List nodes · forget one |
| `node start-agent NODE HARNESS` | Start an agent on a node that has none |
| `node sessions NODE` · `node commands NODE` · `node mcp NODE` | The agent's saved sessions, commands, MCP servers |
| `run NODE -- CMD…` | Run a command (no shell) and stream its output; exits with its code |
| `prompt NODE PROMPT…` | Ask the node's agent |
| `tui [NODE]` | Chat in the terminal |
| `task ls` · `task cancel ID` | Recent tasks · cancel one |
| `token create [--ttl 1h] [--reusable]` | A worker-only join token and link |

`NODE` is a name, an id or an id prefix. Most options also have an environment
variable, shown in `--help`.

Several workers can run on one machine: each takes the next free state
directory (`worker`, `worker-2`, …) and name. Workers refuse to run as root.

## Docker

```sh
# Orchestrator; prints the link:
docker compose -f docker/orchestrator.compose.yml up -d --build && \
  docker compose -f docker/orchestrator.compose.yml exec server cat /data/link

# A worker, on each machine (add COMMANDANT_HARNESS=opencode for an agent):
COMMANDANT_LINK='commandant://…' docker compose -f docker/worker.compose.yml up -d --build
```

The image holds `commandant-server` only and runs as user `commandant` (uid
1000); control the cluster with your own `commandant`. `docker-compose.yml` at
the root runs a server and two workers on one host, for development; the
workers join through the link in the server's volume.

## Security

- Traffic is **plaintext gRPC**. Keep port 7400 on a private network (VPN,
  Tailscale) and never on the internet.
- Workers run what they are told as their own user, with no sandbox, and their
  agents' permission prompts are granted. Give them a dedicated user or container.
- Only hashes of tokens are stored; secret files are readable by their owner only.

## Development

```sh
cargo test                                  # unit and end-to-end tests
cargo clippy --all-targets -- -D warnings
RUST_LOG=debug commandant-server serve      # verbose logs, for either binary
```
