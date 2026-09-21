//! Workshop agent-adapter detection.
//!
//! Finds the official vendor CLIs (`claude`, `codex`, `cursor-agent`/`agent`, `opencode`) on
//! `PATH` and in the known install directories, verifies each binary's identity through its own
//! `--version` / `--help` output, and asks the CLI's **official status command** whether the user is
//! signed in.
//!
//! Hard rules (see the Workshop production plan, "Agent adapters — spawn, do not steal"):
//!
//! * Login state comes only from the vendor's documented status command. This crate never opens
//!   another application's credential files, keychain items, or databases.
//! * A binary is a vendor CLI only after its identity is verified. An unrelated executable that
//!   happens to be called `agent` is not Cursor.
//! * Child processes run with a minimal, credential-free environment ([`env::minimal_env`]).
//!
//! The picker data model ([`Rail`], [`Pill`], [`RailState`], [`copy`]) mirrors the Blackpen picker
//! export: three rails (Claude, Codex, Cursor), one pill per rail (Detecting, Ready, Sign in), and
//! per-rail model radios keyed `provider:model:variant`.

pub mod copy;
pub mod env;
pub mod identify;
pub mod locate;
pub mod model;
pub mod probe;
pub mod process;
pub mod status;

pub use identify::{Identity, IdentifyError, identify};
pub use locate::{Candidate, CandidateSource, DetectConfig, known_dirs, locate};
pub use model::{ModelRef, Pill, Rail, RailState, Vendor, composer_label, rail_state, rails};
pub use probe::{Probe, Rejected, VendorProbe, probe_all, probe_vendor};
pub use status::{LoginState, login_argv, status_argv, version_argv};
