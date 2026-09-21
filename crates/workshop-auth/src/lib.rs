//! Workshop connection policy.
//!
//! Login in Workshop is a connection picker, never an xAI browser flow. This
//! crate holds the pieces upstream code calls into:
//!
//! - [`methods`]: the `workshop.connect` ACP auth method that replaces the
//!   inherited `grok.com` default, the placeholder OAuth2 issuer, and the
//!   copy for the optional xAI card.
//! - [`xai_opt_in`]: the explicit, persisted opt-in that is the only way the
//!   inherited xAI OIDC flow gets a real issuer.
//! - [`detect`]: presence-only detection of `claude` / `codex` /
//!   `cursor-agent` / `opencode` and API-key env vars. No auth files, no
//!   keychain, no network.
//! - [`picker`] (feature `tui`): the Models / Subscriptions picker widget.
//!
//! Forbidden here by `gate:no-theft`: reading another app's credentials
//! (`~/.codex/auth.json`, OpenCode `auth.json`, the Claude Code keychain item,
//! `~/.cursor/sdk/auth.json`) or bundling a Claude OAuth capture.

#![deny(clippy::indexing_slicing)]

pub mod detect;
pub mod methods;
pub mod xai_opt_in;

#[cfg(feature = "tui")]
pub mod picker;
