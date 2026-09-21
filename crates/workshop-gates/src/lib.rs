//! Workshop CI gates.
//!
//! This crate has no runtime code. Its integration tests are the gates the production plan
//! requires on every PR:
//!
//! * `tests/no_theft.rs` — **gate:no-theft**. Fails if any workspace source opens another
//!   application's credential store (Codex or OpenCode `auth.json`, the Claude Code keychain item
//!   or credentials file, Cursor's SDK auth file or state database), references the deleted
//!   token-vault module from the old dock, or bundles a Claude OAuth capture path (the Meridian
//!   plugin and its loopback proxy, Claude/Anthropic OAuth endpoints, the Claude Code OAuth
//!   client id). The exact patterns live in the test; this file deliberately does not repeat them.
//!
//! The no-xAI gates (default issuer, `grok.com` method id, production endpoints, updater) belong
//! to the M0/A/B overlay work and land in this crate as `tests/no_xai.rs`.
//!
//! Allowlisting is by path only, through `no-theft-allowlist.txt` next to this crate's manifest.
//! Keep it empty unless a file must name the thing being forbidden.
