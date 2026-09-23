//! Workshop agent-adapter detection.
//!
//! Finds the official vendor CLIs (`claude`, `codex`, `cursor-agent`/`agent`, `opencode`) on
//! `PATH` and in the known install directories, verifies each binary's identity through its own
//! `--version` / `--help` output, and asks the CLI's **official status command** whether the user is
//! signed in, then the CLI itself which models that account has ([`models`]).
//!
//! Hard rules (see the Workshop production plan, "Agent adapters — spawn, do not steal"):
//!
//! * Login state and model lists come only from the vendor's own CLI. This crate never opens
//!   another application's credential files, keychain items, or databases.
//! * One child per vendor at a time, and a `claude` child is never killed mid-run
//!   ([`process::VendorSlot`]).
//! * A binary is a vendor CLI only after its identity is verified. An unrelated executable that
//!   happens to be called `agent` is not Cursor.
//! * Child processes run with a minimal, credential-free environment ([`env::minimal_env`]).
//!
//! The picker data model ([`Rail`], [`Pill`], [`RailState`], [`copy`]) mirrors the Blackpen picker
//! export: three rails (Claude, Codex, Cursor), one pill per rail (Detecting, Ready, Sign in), and
//! per-rail model radios keyed `provider:model:variant`.
//!
//! The workspace-wide **gate:no-theft** scan (foreign credential files, keychain items, and
//! Claude OAuth capture paths) lives in the overlay gates crate:
//! `crates/workshop-gates/tests/no_theft.rs` (source, comments and `#[cfg(test)]` stripped) plus
//! `scripts/no-theft-fs-audit.sh` (runtime strace audit with decoy credential files).

pub mod copy;
pub mod env;
pub mod identify;
pub mod install;
pub mod locate;
pub mod model;
pub mod models;
pub mod probe;
pub mod process;
pub mod status;

pub use identify::{IdentifyError, Identity, identify};
pub use install::{install_command, install_log_path, official_install_command, run_installer};
pub use locate::{Candidate, CandidateSource, DetectConfig, known_dirs, locate};
pub use model::{ModelRef, Pill, Rail, RailState, Vendor, composer_label, rail_state, rails};
pub use models::{
    Account, ModelsCache, ModelsError, RailModels, Refresh, SubscriptionModel, SubscriptionModels,
    cached_subscription_models, picker_rails, rail_models, rails_models, subscription_models,
};
pub use probe::{Probe, Rejected, VendorProbe, probe_all, probe_vendor};
pub use status::{LoginState, login_argv, status_argv, version_argv};
