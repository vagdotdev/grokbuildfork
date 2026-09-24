# Security Policy

Workshop is a fork of [Grok Build](https://github.com/xai-org/grok-build); reports about Workshop
go to this repository, not to xAI.

Please report vulnerabilities privately through GitHub's security advisories for this repository:

https://github.com/vagdotdev/grokbuildfork/security/advisories/new

If that form is not available yet (the repository owner enables private vulnerability reporting
under Settings → Code security), open a GitHub issue titled "Security" with **no details** and ask
for a private channel. Do not put the vulnerability itself in a public issue. Include the Workshop
version (`workshop --version`), your OS, and the steps to reproduce.

What Workshop promises by default, and what a report is most welcome about: no request to any xAI
endpoint unless the user explicitly signs in with the optional xAI card; no telemetry; a delegated
CLI (Claude Code, Codex, Cursor, OpenCode) never receives Workshop's API keys or another vendor's
credentials; the installer and the updater verify every download against a SHA-256 the release
publishes. The gates that enforce this live in `crates/workshop-gates` and `scripts/no-xai-scan.sh`.
