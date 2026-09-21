//! Workshop agent adapters: **spawn official CLIs, do not steal their sessions.**
//!
//! An adapter run is a supervised child process of `claude`, `codex`, `cursor-agent`/`agent`, or
//! `opencode`, started with that vendor's documented non-interactive flags, whose JSON output is
//! normalized into one [`AdapterEvent`] stream. The supervisor owns cancellation (the whole
//! process group is killed), idle and total timeouts, bounded output, and fail-closed handling of
//! schema drift. Session resume uses the vendor's own resume flag and session id.
//!
//! What this crate never does:
//!
//! * read or pass vendor credentials — children get the allowlisted environment from
//!   [`workshop_detect::env::minimal_env`], and the CLI authenticates with its own login;
//! * run in the user's checkout by default — runs happen in an isolated [`worktree::Worktree`] and
//!   changes come back as a patch preview;
//! * nest token-by-token inside Workshop's own agent loop — v1 is whole-task delegation.
//!
//! Flags were verified against the vendor docs on 2026-09-21; see [`command`].

pub mod command;
pub mod error;
pub mod event;
pub mod normalize;
pub mod request;
pub mod supervisor;
pub mod worktree;

pub use command::{SupportMatrix, build_argv, vendor_env};
pub use error::{AdapterError, FailureReason};
pub use event::{AdapterEvent, RunOutcome, RunStatus, Usage};
pub use request::{PermissionProfile, RunRequest, Workdir};
pub use supervisor::{RunHandle, Supervisor, SupervisorConfig};
pub use worktree::{PatchPreview, Worktree};
pub use workshop_detect::Vendor;
