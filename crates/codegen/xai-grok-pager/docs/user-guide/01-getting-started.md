# Getting Started

Workshop is a terminal coding agent: it reads your codebase, runs shell commands, edits files and
tracks tasks, right in your terminal. It starts on a free model with nothing to configure; when you
want more, `/model` switches models and `/auth` connects a coding subscription (Claude Code, Codex,
Cursor) or an API key.

You can use it interactively as a full-screen TUI, run it headlessly for scripting and CI/CD, or
integrate it into editors via the Agent Client Protocol (ACP).

---

## Installation

macOS and Linux, one line:

```bash
curl -fsSL https://raw.githubusercontent.com/vagdotdev/grokbuildfork/release-channel/install.sh | sh
```

The installer downloads the release for your platform, verifies its SHA-256 against the channel
manifest, installs `~/.workshop/bin/workshop` and prints the one line to add to your `PATH` if it
is not there yet. It makes no other network requests and sends no telemetry. Later:

```bash
workshop update            # move to the newest release
workshop --version
```

Prefer a specific version? `WORKSHOP_VERSION=0.2.2 curl -fsSL … | sh`. Prefer to build from
source? `cargo build --release -p xai-grok-pager-bin --bin workshop` in a checkout of the
repository (`protoc` 29 is required).

**macOS:** the binary is not Apple-notarized. The installer clears the quarantine attribute; if
macOS still refuses to start it, run `xattr -d com.apple.quarantine ~/.workshop/bin/workshop`.

Workshop keeps everything in `~/.workshop` (override with `WORKSHOP_HOME`). It never reads another
tool's settings, hooks or sessions.

---

## First Launch

```bash
cd your-project
workshop
```

You land in the composer. The footer names the active model, **Big Pickle**: OpenCode's free
default, no sign-in, no key. Type a sentence and press `Enter`.

OpenCode's official `opencode` CLI is installed on your first launch and started in the background
every time Workshop opens, so it is usually ready by the time you press `Enter`; a message sent
before it is ready simply waits for it. The install (about a minute on a slow connection) reaches
only opencode.ai, the vendor's installer; every later message answers within seconds. While an
answer is on its way you see one line, `Thinking…`, with the elapsed seconds and `Ctrl+C to cancel`.
If the free model cannot be reached, Workshop quietly answers through another free model and the
footer names the model that answered; only if that fails too do you see `Couldn't reach Big Pickle
— Enter to retry · /model to switch`.

Two commands are all you need to know on day one:

- **`/model`** lists the models you can use right now: OpenCode's free models, then each coding
  subscription whose CLI is installed (Claude, Codex, Cursor) with its models — or a `Sign in` row
  when the CLI is not signed in yet — then any API-key provider you connected, and local servers
  (Ollama, LM Studio, llama.cpp / vLLM). Type to filter; `Ctrl+A` shows the non-chat models
  (classifiers, routers) hidden by default.
- **`/auth`** connects more: the Claude / Codex / Cursor subscription CLIs (installed and signed in
  with their own official login), API keys for OpenRouter, Google AI Studio, NVIDIA, OpenAI and
  Anthropic (kept in your OS keyring), and — last, optional and labeled — an xAI account.

See [Authentication](02-authentication.md) for the details of each.

---

## Basic Interaction

Workshop presents a full-screen TUI with two main areas:

- **Scrollback** -- the conversation history showing your prompts, Workshop's responses, tool
  calls, file edits, and more.
- **Prompt** -- the input area at the bottom where you type messages.

Type a message and press `Enter` to send it. Workshop reads files, runs commands, and edits code as
needed. Each tool run streams into the scrollback in real time. While the model has not answered
yet, an animated line shows the phase and the elapsed time; `Ctrl+C` cancels.

Press `Tab` to move focus between the prompt and the scrollback. While a turn is running, `Ctrl+C`
cancels it once the composer is empty — with a draft, the first press only clears it. `Esc` never
cancels a turn; mid-turn it shows a reminder to use `Ctrl+C`. Idle, press `Esc` twice within 800ms
to clear a non-empty prompt, or (with an empty prompt and conversation messages) to open rewind —
see [Keyboard Shortcuts](03-keyboard-shortcuts.md#escape). With the scrollback focused, use the
arrow keys to select entries and to collapse or expand them. To navigate with `j`/`k` and fold with
`h`/`l` instead, enable Vim mode.

### File References

Use `@` in your prompt to attach files:

```
@src/main.rs              # Attach a file
@src/main.rs:10-50        # Attach lines 10-50
@src/                     # Browse a directory
```

The `@` operator opens a fuzzy file picker. By default it respects `.gitignore` and hides dotfiles.
Prefix with `!` to search hidden files:

```
@!.github                 # Search hidden files
@!.env                    # Attach a .env file
```

### Permissions

Workshop starts in **Always-approve**: commands and edits run without prompts, and the composer
border says so (`Big Pickle · always-approve`). `Shift+Tab` cycles the session mode: **Normal**
asks before risky commands and edits, **Plan** explores read-only and presents a plan first,
**Always-approve** skips the prompts. The mode you pick is remembered for later launches. You can
also:

- Press `Ctrl+O` to toggle always-approve mode
- Use the `--always-approve` flag at launch: `workshop --always-approve`
- Type `/always-approve` in the prompt to toggle the mode

---

## Key Concepts

### Sessions

Every conversation is a **session**. Sessions are automatically saved to `~/.workshop/sessions/`
and can be resumed later. Each session tracks the full conversation history, tool calls, file
edits, and task state.

- Start a new session: `Ctrl+N` or `/new`
- Resume a previous session: `/resume` in the TUI, or `--resume <ID>` from the CLI
- Continue the most recent session: `workshop -c`

### Scrollback

The scrollback is the main display area. It shows:

- **User prompts** -- your messages, rendered as sticky headers
- **Agent messages** -- Workshop's responses with full markdown rendering and syntax highlighting
- **Thinking blocks** -- the model's reasoning process (collapsible)
- **Tool calls** -- file edits (with inline diffs), command executions, search results, and more
- **Task lists** -- TODO items tracking progress

Collapse or expand the selected entry with the `Left`/`Right` arrow keys (or `h`/`l` and `e` in
Vim mode). In Vim mode, press `y` to copy its content and `Y` to copy its metadata (for example,
the command that ran). Press `Enter` to open it in the fullscreen viewer (in any mode).

### Tools

Workshop has built-in tools for:

| Tool | Description |
|------|-------------|
| `read_file` / `search_replace` | Read and edit files with line-precise changes |
| `grep` | Regex search across your codebase (powered by ripgrep) |
| `list_dir` | List directory contents |
| `run_terminal_command` | Execute shell commands |
| `web_search` / `web_fetch` | Search the web and fetch URLs |
| `todo_write` | Create and manage task lists |
| `spawn_subagent` | Spawn parallel subagent sessions |
| `memory_search` | Search cross-session memory |

Tools can be extended with [MCP servers](05-configuration.md#mcp-servers) for integrations like
GitHub, databases, and more.

### Slash Commands

Type `/` in the prompt to access commands. These provide quick actions without writing a full
prompt:

```
/model                            # Switch model (type to filter the list)
/auth                             # Connect a subscription CLI or an API key
/compact                          # Compress conversation history
/always-approve                   # Toggle always-approve mode
/new                              # Start a new session
```

See [Slash Commands](04-slash-commands.md) for the complete reference.

---

## Common Launch Options

```bash
# Launch the interactive TUI and submit an initial prompt as the first turn
workshop "fix the failing auth test and run it"

# Initial prompt in a new git worktree. Use --worktree=<name> (with `=`) so the
# prompt isn't swallowed as the worktree name — `workshop -w "refactor module X"`
# would treat "refactor module X" as the worktree label, not the prompt.
workshop --worktree=feat "refactor module X"

# Base the worktree on a specific branch (e.g. main) instead of the current HEAD:
workshop -w --ref main "implement feature from main"

# Start in a specific project directory
workshop --cwd ~/projects/my-app

# Add project-specific rules
workshop --rules "Always use TypeScript. Prefer functional components."

# Auto-approve all tool executions
workshop --always-approve

# Resume a previous session
workshop --resume <session-id>

# Continue the most recent session
workshop -c

# Experimental scrollback-native render mode. Sticky: plain `workshop` reopens in
# the mode last chosen via --minimal/--fullscreen (or /minimal//fullscreen).
workshop --minimal

# Back to the standard fullscreen TUI (and make it sticky again)
workshop --fullscreen

# Headless mode (for scripts)
workshop -p "Explain this codebase"
```

---

## Headless Mode

Run Workshop non-interactively for scripting, CI/CD, and automation:

```bash
workshop -p "Your prompt here"
```

Output formats:

| Format | Flag | Description |
|--------|------|-------------|
| `plain` | (default) | Human-readable text |
| `json` | `--output-format json` | Single JSON object with `text`, `stopReason`, `sessionId`, and `requestId` |
| `streaming-json` | `--output-format streaming-json` | NDJSON event stream for real-time processing |

Example CI/CD usage:

```bash
workshop -p "Review changes for bugs" --output-format json --always-approve | jq -r '.text'
```

---

## Project Rules (AGENTS.md)

Add per-project instructions by creating an `AGENTS.md` file in your repository. Workshop reads
these files and injects their contents as a project-instructions message at the start of the
conversation:

```
~/.workshop/AGENTS.md       # Global rules (apply to all projects)
<repo-root>/AGENTS.md       # Repository-level rules
<cwd>/AGENTS.md             # Directory-level rules (highest priority)
```

Deeper files take precedence. Workshop also reads `CLAUDE.md` files for compatibility.

---

## Where to Go Next

| Document | What You Will Learn |
|----------|-------------------|
| [Authentication](02-authentication.md) | `/model`, `/auth`, subscription CLIs, API keys in the keyring, local servers |
| [Keyboard Shortcuts](03-keyboard-shortcuts.md) | Complete reference for all key bindings |
| [Slash Commands](04-slash-commands.md) | All available `/` commands |
| [Configuration](05-configuration.md) | config.toml, pager.toml, environment variables |
