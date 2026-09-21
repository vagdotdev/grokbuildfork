# ADR 0002 — Three connection classes; the picker is the only default auth surface

Status: accepted (M0/A). Owner: Workshop.

## Decision

Every way a model is reached is one of three classes, and the class is always shown:

| Class | Workshop owns | Credential | Examples |
|---|---|---|---|
| Direct API | prompt, tools, loop, HTTP | API key issued for API use | OpenAI, Anthropic, OpenRouter, optional xAI API |
| Local | same, on loopback | none / local token | Ollama, LM Studio, llama.cpp, vLLM |
| Agent adapter | process supervision + normalized events | the vendor CLI's own login | `cursor-agent`, `claude`, `codex`, `opencode` |

Login, welcome `l`, `/login`, `/auth`, `/models`, first run and `workshop login` open the
**connection picker** (`crates/workshop-auth`). The picker never starts an OAuth flow by itself.
Its layout follows the Blackpen export: tabs **Models** and **Subscriptions**; Subscriptions is the
rail **Claude, Codex, Cursor** with one pill each (Detecting / Ready / Sign in); the **xAI (optional)**
card is the last Models card, copy "Uses xAI accounts and `auth.x.ai`. Not required.", and is never
preselected. Only that card, after an explicit second Enter, sends `AuthenticateRequest` with
`workshop_xai_opt_in`, and only then does the shell attach the xAI OAuth2 provider for that login.

Headless / ACP without a configured connection fail closed ("No connection configured"), never with a
browser.

## Consequences

- `build_auth_methods` advertises `grok.com` only when an OAuth2 provider is configured
  (`has_oauth2_provider`); Workshop's default is none, so the method list is empty at cold start.
- The default model is a neutral placeholder (`workshop-unconfigured`) until a connection exists.
  Aux tools inherit it, so there is no hidden xAI fallback.
- Milestone C adds Direct/Local manifests and a credential broker; milestone D completes the picker
  (consent + presence scan, review screen, PTY snapshots); milestone E adds adapters.
