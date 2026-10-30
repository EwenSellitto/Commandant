# What each harness can be configured with

Research for the ticket "What each harness can be configured with" (issue #76),
under the map "Fleet file: declarative node and harness setup" (issue #75).
This file lives on a throwaway branch and is never merged.

**Question.** For Claude Code and OpenCode, at user scope (the worker's Unix
user): where do MCP servers, skills, custom agents, instructions, and the
default agent / model / effort live? Can each be set by files alone? Where
would a value Commandant writes collide with the user's own?

**Sources and versions** (read 2026-10-30):

- Claude Code: the official docs at code.claude.com/docs/en/ (pages: `mcp`,
  `skills`, `sub-agents`, `memory`, `model-config`, `settings`,
  `settings-reference`, `claude-directory`, `cli-reference`,
  `plugins-reference`). The pages carry no date. The newest version they
  mention is v2.1.309. Installed on this node: **2.1.314**.
- OpenCode: the docs at opencode.ai/docs/ (`config`, `mcp-servers`, `skills`,
  `agents`, `rules`, `models`; the config page says "Last updated: Oct 29,
  2026" and has an OpenCode 2 banner), plus the source at
  github.com/anomalyco/opencode (`sst/opencode` redirects there). I read the
  source at tag **v1.18.33**, the version installed on this node. The latest
  release is v1.19.5 (2026-10-28).
- **Warning about OpenCode versions:** the live schema at
  `https://opencode.ai/config.json` already has the OpenCode 2 shape: `mcp`
  has a `servers` wrapper, `model` takes `provider/model#variant`, and there
  are `agents` and `plugins` keys. v1.18.33 takes that shape and converts it
  to its own (`packages/opencode/src/config/v2-compat.ts`), but it marks
  `model#variant` as **unsupported**. This file describes the 1.x (flat)
  shape, which is the one the worker's harness runs today.

## How the worker drives each harness today

This decides which routes avoid files entirely:

- **Claude Code** starts as a new `claude -p … --output-format stream-json`
  process on every turn (`crates/commandant-worker/src/claude/stream.rs`).
  Each turn already passes `--model`, `--effort` and `--agent`. The worker
  controls argv and env for every turn, so per-run flags (`--settings`,
  `--mcp-config`, `--plugin-dir`, `--append-system-prompt-file`, `--agents`)
  can carry Commandant's config without writing into `~/.claude`. A new turn
  also re-reads every file.
- **OpenCode** runs one long-lived `opencode serve` per worker
  (`crates/commandant-worker/src/opencode/mod.rs`). Each prompt passes model,
  agent and variant in the API call. The worker controls that process's env,
  so `OPENCODE_CONFIG`, `OPENCODE_CONFIG_DIR` and `OPENCODE_CONFIG_CONTENT`
  can carry Commandant's config. Config is cached for each instance (one per
  project directory), so a change needs a reload, and a reload aborts running
  prompts. The worker already handles this for credentials with its `stale`
  flag.

## Feature × harness table

"File-only" means the item can be set by writing files, with no interactive
step. In every Claude Code path below, `~` stands for `$CLAUDE_CONFIG_DIR`
when that is set. In every OpenCode path, `~/.config` stands for
`$XDG_CONFIG_HOME`.

| Feature | Claude Code | OpenCode |
|---|---|---|
| **MCP servers: where (user scope)** | Top-level `mcpServers` in `~/.claude.json`. That file also holds app state, and Claude Code rewrites it often (it keeps 5 backups in `~/.claude/backups/`). Checked on this node: top-level `mcpServers` holds `context7` and `codebase-memory-mcp`. Per-run: `--mcp-config <file-or-json>`. With a plugin: `.mcp.json` at the plugin root. | `mcp` key in `~/.config/opencode/opencode.json` (or `.jsonc`). Also any config layer: `OPENCODE_CONFIG`, `OPENCODE_CONFIG_DIR/opencode.json`, `OPENCODE_CONFIG_CONTENT`. |
| MCP: format | `{"mcpServers": {"<name>": {...}}}`. stdio: no `type`, `command`, `args`, `env`. Remote: `type` = `http` (alias `streamable-http`), `sse` (deprecated) or `ws`, plus `url`, `headers`, optional `headersHelper`, `oauth`, `timeout`, `alwaysLoad`. `${VAR}` and `${VAR:-default}` expand in `command`, `args`, `env`, `url`, `headers`. | `{"mcp": {"<name>": {...}}}`. Local: `"type": "local"`, `command` (one array: binary plus args), `environment`, `enabled`, `timeout`, `cwd`. Remote: `"type": "remote"`, `url`, `headers`, `oauth` (object or `false`), `enabled`, `timeout`. `{env:VAR}` and `{file:path}` work anywhere in the config. |
| MCP: enabled or not | No `enabled` field. You switch a server off with the `/mcp` toggle, which writes `disabledMcpServers` for that project into `~/.claude.json`. To switch one off with a file: `deniedMcpServers` (by name, URL or command) in `settings.json`, or leave it out of `--mcp-config` and add `--strict-mcp-config`. | `"enabled": false` on the entry. The worker can also connect or disconnect a server at runtime through the API (`/mcp/{name}/connect`), and that change is not saved. |
| MCP: file-only? | **Yes** for stdio servers and for remote servers with a static `Authorization` header ("Claude Code uses it as the server's credential and doesn't fall back to OAuth"). **No** for OAuth servers: "A `claude -p` … run has no `/mcp` panel", so someone must sign in once with `/mcp` or `claude mcp login <name>`. `claude mcp add-json --scope user <name> '<json>'` writes user scope without any prompt. | **Yes** for local servers and for remote servers with static `headers`. **No** for OAuth: `opencode mcp auth <name>` opens a browser. Tokens are kept in `~/.local/share/opencode/mcp-auth.json`. |
| **Skills: where** | `~/.claude/skills/<name>/SKILL.md`. Legacy commands: `~/.claude/commands/*.md`. With a plugin: `<plugin>/skills/`, named `plugin:skill`. | Scanned in this order: `~/.claude/skills/`, `~/.agents/skills/`, the project's `.claude/skills/` and `.agents/skills/`, then `{skill,skills}/**/SKILL.md` in each config dir (`~/.config/opencode/`, `.opencode/` dirs, `~/.opencode/`, `$OPENCODE_CONFIG_DIR`), then `skills.paths` and `skills.urls` in the config (source: `src/skill/index.ts`). |
| Skills: format | YAML frontmatter + Markdown. Every field is optional, and `description` is recommended. Fields: `name`, `description`, `when_to_use`, `allowed-tools`, `disable-model-invocation`, `user-invocable`, `model`, `effort`, `context`, `agent`, `paths`, `hooks`, … Unknown fields are ignored. | Frontmatter needs `name` and `description` (1–1024 chars). Optional: `license`, `compatibility`, `metadata`. `name` must match `^[a-z0-9]+(-[a-z0-9]+)*$` **and the name of its folder**. Unknown fields are ignored. |
| Skills: file-only? | **Yes.** Claude Code watches the folder. A new turn is a new process anyway. | **Yes**, after a reload of the instance. |
| **Custom agents: where** | `~/.claude/agents/*.md`. Per-run: `--agents '<json>'` (or a path to a JSON file with `-p`, v2.1.281+). With a plugin: `<plugin>/agents/`, named `plugin:agent`. | `~/.config/opencode/{agent,agents}/**/*.md`, where the file name is the agent name. Or the `agent` key in any config layer. Also `.opencode/` dirs and `$OPENCODE_CONFIG_DIR/agents/`. |
| Agents: format | Frontmatter needs `name` and `description`. Optional: `tools`, `disallowedTools`, `model` (alias, full ID, or `inherit`), `effort`, `permissionMode`, `maxTurns`, `skills`, `mcpServers`, `hooks`, `memory`, `background`, `isolation`, `color`, `initialPrompt`, `omitClaudeMd`. The body is the system prompt. | Frontmatter or JSON. `description` is required. Optional: `mode` (`primary`, `subagent` or `all`; default `all`), `model` (`provider/model`), `variant`, `prompt`, `temperature`, `top_p`, `steps`, `permission`, `disable`, `hidden`, `color`. Other keys are passed to the provider as options (e.g. `reasoningEffort`). |
| Agents: file-only? | **Yes.** The folder is watched, but the first file in a new `agents/` folder needs a restart. A new turn is a new process anyway. `/agents` no longer opens a wizard (it stopped in v2.1.197). | **Yes**, after a reload. `opencode agent create` is interactive, but it is not needed. |
| **Instructions: where** | `~/.claude/CLAUDE.md` (one file). Also `~/.claude/rules/*.md`: **all of them load**, in every project, and they add to each other. `@path` imports work up to 4 hops deep, and imports in user files load without the approval dialog. Per-run: `--append-system-prompt-file <file>`. A `CLAUDE.md` inside a plugin is **not** loaded. | `~/.config/opencode/AGENTS.md` (one file). If it is missing, `~/.claude/CLAUDE.md` is used instead (`OPENCODE_DISABLE_CLAUDE_CODE_PROMPT=1` turns that off). The `instructions` array (paths, globs, URLs) in any config layer is **added** to the AGENTS.md files. Across layers the arrays are concatenated and duplicates removed (`mergeConfigConcatArrays`). |
| Instructions: AGENTS.md | Reads a project's `AGENTS.md` only when there is no project `CLAUDE.md` (v2.1.277+). The user-level `~/.claude/CLAUDE.md` loads either way. | Native. In a project, the first match wins: `AGENTS.md`, then `CLAUDE.md`. |
| Instructions: file-only? | **Yes.** | **Yes**, after a reload. |
| **Default model** | `"model"` in `~/.claude/settings.json`. Precedence, highest first: `/model`, `--model`, `ANTHROPIC_MODEL`, the `model` setting, `ANTHROPIC_DEFAULT_MODEL`. Inside settings: managed, `--settings`, local, project, user. | `"model": "provider/model"` in the config. Precedence: `--model` (or the per-prompt API choice), the config `model`, the last-used model, then OpenCode's internal priority. |
| **Default effort** | `"effortLevel"`: `low`, `medium`, `high` or `xhigh` (`max` is not accepted here). **Caveat:** in `~/.claude/settings.json` it "applies only to Opus 5 and earlier". Opus 5.5+ reads `modelSettings.<model>.effortLevel` instead. From `--settings`, project or managed settings it applies to every model. `CLAUDE_CODE_EFFORT_LEVEL` **outranks `--effort`**. | v1.x has **no top-level default variant**: `model#variant` is flagged unsupported in v1.18. Per agent: `agent.<name>.variant`, used only with that agent's own `model`. Per model: `provider.<id>.models.<id>.variants` defines variants but does not pick a default. |
| **Default agent** | `"agent": "<name>"` in any settings file, or `--agent`. A plugin's `settings.json` can also set `agent`. | `"default_agent": "<name>"`. It must be a primary agent. An invalid value falls back to `build` with a warning. |
| **Ways to add config without touching the user's files** | `--settings <file>`: one session, above user/project/local, below managed. `--mcp-config <file>`: added to the user's servers, or replaces them with `--strict-mcp-config`. `--agents <json\|file>`: above project and user agents. `--plugin-dir <dir>`: skills, agents, MCP and a default `agent`, all under a `commandant:` prefix. `--append-system-prompt-file <file>`. All of these are per-run flags, and the worker builds argv every turn. | Env on `opencode serve`: `OPENCODE_CONFIG=<file>` (loaded after the global file, **before** the project), `OPENCODE_CONFIG_DIR=<dir>` (its `opencode.json`, `agents/`, `skills/` and `commands/` load **after** the global file and `.opencode/` dirs), and `OPENCODE_CONFIG_CONTENT=<json>` (after the project). |

## Where Commandant's values would collide with the user's own

### Claude Code

1. **`~/.claude.json` (user-scope MCP servers).** Claude Code writes this
   file itself: account details, trust decisions, caches, per-project state.
   If Commandant edits it by hand, Claude Code can overwrite the edit, and the
   two can race. If the user defines a server with the same name, the whole
   entry from the higher source is used, with no field merging. Order:
   local > project > user > plugin > claude.ai connectors. **What is safer:**
   `claude mcp add-json --scope user` (the CLI does the write), or better a
   Commandant file passed with `--mcp-config` on every turn, which writes
   nothing into the user's config. *Gap:* the docs don't say which wins when a
   `--mcp-config` server and a user-scope server share a name. Test that
   before relying on it.
2. **`~/.claude/settings.json`** is the user's own file. Here it holds
   `model`, `modelSettings`, `env`, `hooks` and plugins. `/model` and
   `/config` rewrite it. A value passed with `--settings` replaces the user's
   value for that session with no warning. List keys such as
   `permissions.allow` combine across scopes instead. So a Commandant default
   either always wins (`--settings`) or always loses to anything the user
   sets in project or local files (if Commandant writes the user file). The
   recipe format has to choose one.
3. **Skills and agents, by name.** For skills, personal beats project, so a
   Commandant skill in `~/.claude/skills/` hides a project skill of the same
   name. For agents, project beats user, so a project agent hides a
   Commandant agent in `~/.claude/agents/`. In both cases two files with the
   same name in one folder are impossible. A Commandant folder inside the
   user's own folder (e.g. `~/.claude/skills/<name>/`) can't be told apart
   from the user's skills when it is removed. `--plugin-dir` avoids every one
   of these problems: names get a `commandant:` prefix and the files stay out
   of `~/.claude`. The cost is that users type `/commandant:deploy`, not
   `/deploy`.
4. **Instructions.** `~/.claude/CLAUDE.md` is one file the user owns.
   `~/.claude/rules/commandant.md` is a separate file that adds to it, so
   both coexist. `--append-system-prompt-file` writes nothing to disk.
5. **Effort.** If `CLAUDE_CODE_EFFORT_LEVEL` is set in Commandant's env or in
   a settings `env` block, it silently overrides the per-task `--effort` the
   worker passes. Do not use it. Note also that `effortLevel` in the user
   file is ignored on Opus 5.5+.

### OpenCode

1. **`~/.config/opencode/opencode.json`** is the user's one global file. On
   this node it is a symlink into the user's dotfiles repo, so writing to it
   would change a git repo the user tracks. OpenCode also rewrites it itself
   (`Config.updateGlobal`). **What is safer:** a Commandant-owned
   `OPENCODE_CONFIG_DIR` (or `OPENCODE_CONFIG`) set on the `opencode serve`
   process.
2. **Merging is deep.** Layers are combined with remeda `mergeDeep`, and
   `instructions` arrays are concatenated with duplicates removed. So a
   Commandant `mcp.github` or `agent.review` entry with the same name as the
   user's is **merged field by field** into the user's entry. It does not
   replace it, so the result can mix the two (e.g. the user's `command` with
   Commandant's `environment`). Claude Code takes the whole entry instead.
   Which side wins a field depends on the layer. `OPENCODE_CONFIG_DIR` and
   `OPENCODE_CONFIG_CONTENT` load after the user's global file, so they win.
   `OPENCODE_CONFIG` loads before project files, so a project wins over it.
3. **Skills, by name.** OpenCode scans every location into one map. On a
   duplicate name it logs `duplicate skill name` and **the location scanned
   last wins**: `~/.claude/skills`, then `~/.agents/skills`, then project
   folders, then config dirs, then `OPENCODE_CONFIG_DIR`, then `skills.paths`.
4. **Collisions across the two harnesses.** OpenCode also reads
   `~/.claude/skills/` and, when there is no `~/.config/opencode/AGENTS.md`,
   `~/.claude/CLAUDE.md`. So whatever Commandant writes for Claude Code in
   those places also reaches OpenCode, unless `OPENCODE_DISABLE_CLAUDE_CODE*`
   is set on the server. This could also be used on purpose: one skill
   written to OpenCode's stricter rules (`name` matches the folder, matches
   the regex, and has a `description`) loads in both harnesses. Agents can't
   be shared: the formats differ (`tools` vs `permission`, `sonnet` vs
   `provider/model`).
5. **Default variant.** v1.x has no top-level default effort. The worker
   already sends a variant with each prompt, so the fleet file can keep a
   default effort in Commandant's own recipe and send it per prompt, without
   writing it into OpenCode's config.

## Gaps and open points

- Claude Code: the docs don't say how a `--mcp-config` server and a
  user-scope server with the same name are resolved. Needs a test.
- Claude Code: `settings.json` has no key for defining MCP servers. User
  scope is only `~/.claude.json` (via the file or `claude mcp`), a plugin, or
  `--mcp-config`. Only the managed key `managedMcpServers` defines servers in
  settings.
- OpenCode: I did not check how the worker reloads after a config change
  (its `stale` reload exists for credentials). A config change will need the
  same reload, with the same rule of waiting until no prompts are running.
- OpenCode 2 changes the config shape: `mcp.servers`, `model#variant`,
  `agents`, `plugins`. A recipe-to-files translator for OpenCode should aim
  at one major version, or write the v2 shape, which v1.18 already accepts
  except for `#variant`.
- OAuth MCP servers need a human once, on both harnesses. A fleet file can
  declare them, but it can't finish their sign-in. Static `Authorization`
  headers avoid this.
