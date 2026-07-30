# Workshop authentication companion

Workshop adds provider authentication without patching Grok Build's first-party
session authentication. It is a separate Rust workspace so future SpaceXAI
source syncs do not regenerate or overwrite it.

## Current scope

- Pi-style provider and authentication-method selection
- API-key entry into the operating-system credential store
- OpenRouter's documented OAuth PKCE flow
- xAI account login delegated to Grok's existing `grok login`
- A Pi harness adapter that calls Pi's supported
  `auth print-bearer-token`/`auth print-api-key` interface
- Safe OpenRouter model installation in `~/.grok/config.toml`

Workshop does not read Pi's credential files or copy Pi's embedded OAuth client
identities. Providers such as ChatGPT Codex, Claude subscriptions, and GitHub
Copilot remain owned by Pi until Workshop has its own registered OAuth clients
and matching inference transports.

## Build

```sh
cargo build --manifest-path workshop/Cargo.toml
```

Install the resulting `workshop` binary somewhere permanent before configuring
Grok. Grok stores its absolute path in the auth-provider configuration.

## OpenRouter account login

```sh
workshop auth login openrouter --method oauth
workshop configure openrouter anthropic/claude-sonnet-4
grok --model openrouter-anthropic-claude-sonnet-4
```

The login opens OpenRouter in a browser and listens on a random loopback port.
For SSH or remote environments, paste the final redirect URL or authorization
code into the prompt. OpenRouter returns a user-controlled API key billed from
the user's OpenRouter credits.

Use an existing API key instead:

```sh
export OPENROUTER_API_KEY=...
workshop auth login openrouter --method api-key --from-env
```

## Other providers

List the available authentication ownership and transport status:

```sh
workshop providers
workshop auth status
```

xAI OAuth stays in upstream Grok:

```sh
workshop auth login xai --method oauth
```

For OAuth owned by Pi, authenticate from Pi's `/login` screen. Workshop can
then ask Pi for a fresh credential through Pi's public CLI contract:

```sh
workshop auth token anthropic \
  --source pi \
  --model claude-sonnet-4-6
```

The token command exists for Grok's `[auth_provider.*]` command contract. Its
stdout contains only credential JSON; never run it in logs or paste its output
into chat.

## Storage and security

Secret values are stored in macOS Keychain, Windows Credential Manager, or the
Linux Secret Service through the `keyring` crate. `~/.workshop/auth.json`
contains only non-secret provider metadata and is written with mode `0600` on
Unix; its directory is mode `0700`.

Grok receives provider credentials only through its trusted
`[auth_provider.*]` helper mechanism. This preserves Grok's fail-closed rule:
an xAI session token is never substituted onto a third-party model endpoint.

