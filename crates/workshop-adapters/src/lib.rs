//! Workshop agent adapters: drive the official Claude Code, Codex, Cursor
//! Agent, and OpenCode CLIs as subscription-backed agents.
//!
//! Spawn, do not steal. Workshop never reads another app's credentials —
//! not `~/.claude`, not `~/.codex/auth.json`, not OpenCode's `auth.json`,
//! not `~/.cursor/sdk/auth.json`, not a keychain. Login state comes from the
//! vendor's own status command; login itself is the vendor's own login
//! command attached to the user's terminal.
//!
//! Flow:
//!
//! ```text
//! detect::detect(adapter)        PATH, known dirs, --version/--help identity
//!   -> status::probe_login       vendor status command -> Ready / Sign in
//!   -> worktree::prepare         isolated git worktree for the run
//!   -> supervisor::spawn         pinned flags, process group, minimal env
//!   -> RunHandle::next_event     TextDelta / Thinking / ToolCall / ToolResult /
//!                                Usage / Done / Error
//!   -> RunHandle::cancel         SIGINT, then SIGKILL the group
//! ```

pub mod adapter;
pub mod detect;
pub mod env;
pub mod event;
pub mod probe;
pub mod status;
pub mod supervisor;
pub mod vendors;
pub mod worktree;

pub use adapter::{
    Adapter, AdapterId, LoginState, NormalizeError, Normalizer, PermissionPolicy, PinStatus,
    ProbeOutput, PromptDelivery, RunRequest, Terminal, VersionPin,
};
pub use detect::{DetectOptions, Detection, InstalledCli, detect};
pub use event::{AdapterEvent, Usage};
pub use status::{RailPill, RailStatus, probe_login, rail_status};
pub use supervisor::{RunHandle, RunOutcome, SpawnError, SupervisorOptions, spawn};
pub use worktree::{GitWorktreeIsolation, InPlace, IsolatedWorkspace, WorkspaceIsolation};
