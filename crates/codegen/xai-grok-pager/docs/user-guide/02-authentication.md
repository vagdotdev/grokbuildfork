# Authentication

Workshop needs no account and no key to start: a fresh install runs on the OpenCode engine's free
default model. Everything else is opt-in and lives behind two commands, `/model` and `/auth`.
Nothing on this page happens until you choose it.

---

## What you can connect

| Connection | How | Where the secret lives |
|---|---|---|
| **OpenCode free models** (default) | Nothing to do. `/model` lists the engine's live free list; the official `opencode` CLI runs on your machine and talks to opencode.ai itself. | No secret. |
| **Kilo free pool** and other keyless catalogs | Pick a `free` row in `/model`. | No secret. |
| **Claude Code / Codex / Cursor subscriptions** | `/auth`, pick the rail, `Enter` runs that vendor's own login command in your terminal (`claude auth login`, `codex login`, `cursor-agent login`). Turns then run through the official CLI. | In the vendor CLI's own store. Workshop never reads or copies it. |
| **API keys** — OpenRouter, Google AI Studio, NVIDIA, OpenAI, Anthropic, OpenCode Zen | `/auth`, pick `Provider — API key` and paste the key (or `Provider — Sign in` for OpenRouter's browser sign-in). | Your OS keyring (macOS Keychain, Secret Service, Windows Credential Manager); if none is available, an owner-only file under `~/.workshop/secrets/`. Never in `config.toml`. |
| **Local servers** — Ollama, LM Studio, llama.cpp, vLLM | Start the server; `/model` lists its models once it answers on loopback. | No secret. |
| **xAI account** (optional) | Last row of `/auth`, labeled `xAI — Sign in · optional`. Press `Enter` twice; the browser opens `auth.x.ai`. | The inherited sign-in store under `~/.workshop`. This is the only Workshop path that ever contacts x.ai. |

The composer footer always names the active connection (`OpenCode · Big Pickle`, `Claude · <model>`,
or the model name for an API-key provider).

---

## `/model`

One list of the models you can use right now, grouped **Recommended** then **All models**. Type
to filter (Esc clears the filter, then closes), `Enter` selects, `Tab` switches to `/auth`,
`Ctrl+R` refreshes the live lists, `Ctrl+A` shows the non-chat rows (classifiers, routers) that
are hidden by default. Every row says where its list came from and how old it is (`fetched 2 min
ago`, or `cached list from <date>` when the fetch failed).

The choice persists across launches in `~/.workshop/active-connection.json`.

---

## `/auth`

The **Subscriptions** view: the Claude / Codex / Cursor rails with a `Detecting` / `Ready` /
`Sign in` pill, then the API-key providers, then the optional xAI row.

- A rail is **Ready** when the official CLI is installed and signed in. Workshop only checks that
  the binary is present and answers; it never opens another app's credential files.
- **Sign in** on a rail runs the vendor's login command attached to your terminal and re-probes when
  it exits.
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
workshop doctor           # terminal, clipboard, voice and engine checks with concrete fixes
```

---

## Privacy

- On launch, only the OpenCode engine is brought up: a first run fetches the vendor's installer
  from opencode.ai and starts the engine on this machine; later launches just start it. Nothing
  is sent to a model before your first message; `/model` fetches the keyless catalogs (Kilo,
  OpenRouter, NVIDIA) when you open it.
- Free pools are shared services: prompts sent through them may be logged by the operator. The
  row's detail line says so.
- Telemetry is off and no analytics token is baked in. Auto-update is off unless you opt in with
  `WORKSHOP_ENABLE_AUTOUPDATE=1`; `workshop update` is explicit.

---

## Troubleshooting

| Symptom | What to do |
|---|---|
| `OpenCode unavailable (…) — using Auto Free (Kilo) instead` | The engine could not be installed or started (offline, blocked installer). The line names the cause and `~/.workshop/logs/opencode-engine.log`; Workshop already switched you to the keyless Kilo pool. `workshop doctor` repeats the check. |
| A rail stays on `Sign in` after logging in | The vendor CLI must be on `PATH` (or in its usual install folder) and its own `login` must have finished. Reopen `/auth`; `Ctrl+R` re-probes. |
| `Could not save key` | No OS keyring was reachable and the fallback directory `~/.workshop/secrets/` is not writable. Fix the permissions or set `WORKSHOP_HOME` to a writable location. |
| A pasted key is refused by the provider | Keys are sent as `Authorization: Bearer` (or the provider's header) exactly as pasted; check for a trailing space or an expired key on the provider's key page. |
