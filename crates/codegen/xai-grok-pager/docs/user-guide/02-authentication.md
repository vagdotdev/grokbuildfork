# Authentication

Workshop needs no account and no key to start: a fresh install runs on OpenCode's free default
model. Everything else is opt-in and lives behind two commands, `/model` and `/auth`.
Nothing on this page happens until you choose it.

---

## What you can connect

| Connection | How | Where the secret lives |
|---|---|---|
| **OpenCode free models** (default) | Nothing to do. `/model` lists OpenCode's live free list; the official `opencode` CLI runs on your machine and talks to opencode.ai itself. | No secret. |
| **Claude Code / Codex / Cursor subscriptions** | `/auth`, pick the rail, `Enter`. If the CLI is not installed, the rail says `Install` and Enter runs that vendor's official installer (`curl -fsSL https://claude.ai/install.sh \| bash`, `npm install -g @openai/codex`, `curl https://cursor.com/install -fsS \| bash`) behind one status line, then goes straight into its sign-in. If it is installed, Enter runs that vendor's own login command in your terminal (`claude auth login`, `codex login`, `cursor-agent login`). Turns then run through the official CLI. | In the vendor CLI's own store. Workshop never reads or copies it. |
| **API keys** — OpenRouter, Google AI Studio, NVIDIA, OpenAI, Anthropic, OpenCode Zen | `/auth`, pick `Provider — API key` and paste the key (or `Provider — Sign in` for OpenRouter's browser sign-in). | Your OS keyring (macOS Keychain, Secret Service, Windows Credential Manager); if none is available, an owner-only file under `~/.workshop/secrets/`. Never in `config.toml`. |
| **Local servers** — Ollama, LM Studio, llama.cpp, vLLM | Start the server; `/model` lists its models once it answers on loopback. | No secret. |
| **xAI account** (optional) | Last row of `/auth`, labeled `xAI — Sign in · optional`. Press `Enter` twice; the browser opens `auth.x.ai`. | The inherited sign-in store under `~/.workshop`. This is the only Workshop path that ever contacts x.ai. |

The composer's bottom border reads `<model> (<effort>) · <mode>`: the active model by name
(`Big Pickle`, `Claude Sonnet`, or an API-key provider's model — never a provider or runtime
name), its effort level in parentheses when the model offers levels and you picked one on
`/model` (`Ling 3.0 Flash Fin Free (high)`; a model without levels shows none), then the current
mode when it is not the default (`· plan`, `· auto`, `· always-approve`).

---

## `/model`

One list of the models you can use right now, in groups: **OpenCode**'s free models first, then
each coding subscription whose CLI is installed — **Claude**, **Codex**, **Cursor** — with its
models when the CLI is signed in or one `Sign in` row when it is not (a CLI that is not installed
is not listed here; it stays on `/auth`), then every API-key provider that has a key configured,
and detected local servers. Type to filter (Esc clears the filter, then closes), `Enter` selects,
`Tab` switches to `/auth`, `Ctrl+R` refreshes the live lists, `Ctrl+A` shows the non-chat rows
(classifiers, routers) that are hidden by default. Every row says where its list came from and
how old it is (`fetched 2 min ago`, or `cached list from <date>` when the fetch failed).

The choice persists across launches in `~/.workshop/active-connection.json`.

---

## `/auth`

The **Subscriptions** view: the Claude / Codex / Cursor rails with a `Detecting` / `Ready` /
`Sign in` / `Install` pill, then the API-key providers, then the optional xAI row.

- A rail is **Ready** when the official CLI is installed and signed in. Workshop only checks that
  the binary is present and answers; it never opens another app's credential files. Its models are
  the ones the CLI itself lists (`Loading models…` until it has answered).
- **Sign in** on a rail runs the vendor's login command attached to your terminal and re-probes when
  it exits.
- **Install** on a rail runs the vendor's official installer — one keypress, nothing ever installs
  on its own — with one status line while it works, then the sign-in above follows by itself. The
  installer's output is kept in `~/.workshop/logs/install-<vendor>.log`.
- **API key** rows open a masked field: the count of characters typed is shown, never the key.
  `Enter` saves it to the OS keyring (the line says which backend was used), `Esc` cancels.
  Links to the key pages: [OpenRouter](https://openrouter.ai/settings/keys),
  [Google AI Studio](https://aistudio.google.com/apikey), [NVIDIA](https://build.nvidia.com),
  [OpenAI](https://platform.openai.com/api-keys),
  [Anthropic](https://console.anthropic.com/settings/keys), [OpenCode Zen](https://opencode.ai/auth).

Selecting an API-key model writes a `[model.<key>]` entry to `~/.workshop/config.toml` whose
`env_key` names the variable Workshop exports at runtime (`WORKSHOP_<PROVIDER>_API_KEY`, for
example `WORKSHOP_ANTHROPIC_API_KEY`); the key itself stays in the keyring. Setting that variable
yourself in the environment also works, which is how CI runs use API keys.

---

## From the command line

```bash
workshop login            # the /model and /auth lists as text, plus the next steps
workshop login --xai      # optional xAI account sign-in only (opens auth.x.ai); never the default
workshop logout           # clear the cached xAI sign-in, if you ever used it
workshop models           # list the models the active connection offers
workshop doctor           # terminal, clipboard, voice and free-model checks with concrete fixes
```

---

## Privacy

- On launch, only OpenCode's `opencode` CLI is brought up: a first run fetches the vendor's
  installer from opencode.ai and starts it on this machine; later launches just start it. Nothing
  is sent to a model before your first message; `/model` refreshes the model lists of the providers
  you connected when you open it.
- Free pools are shared services: prompts sent through them may be logged by the operator. The
  row's detail line says so.
- Telemetry is off and no analytics token is baked in. Auto-update is off unless you opt in with
  `WORKSHOP_ENABLE_AUTOUPDATE=1`; `workshop update` is explicit.

---

## Troubleshooting

| Symptom | What to do |
|---|---|
| The footer suddenly names another model | OpenCode's model could not be started or did not answer, so Workshop answered through another free model and says which one. Your choice is unchanged: the next launch tries OpenCode again; `/model` switches any time. `workshop doctor` shows what went wrong. |
| `Couldn't reach Big Pickle — Enter to retry · /model to switch` | Neither the model nor the free stand-in could be reached (offline, blocked installer). `Enter` on the empty composer retries; `workshop doctor` has the details. |
| A rail stays on `Sign in` after logging in | The vendor CLI must be on `PATH` (or in its usual install folder) and its own `login` must have finished. Reopen `/auth`; `Ctrl+R` re-probes. |
| `Could not save key` | No OS keyring was reachable and the fallback directory `~/.workshop/secrets/` is not writable. Fix the permissions or set `WORKSHOP_HOME` to a writable location. |
| A pasted key is refused by the provider | Keys are sent as `Authorization: Bearer` (or the provider's header) exactly as pasted; check for a trailing space or an expired key on the provider's key page. |
