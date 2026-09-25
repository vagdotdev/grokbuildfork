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
//!
//! OpenCode additionally has an engine mode ([`opencode_engine`]): Workshop
//! runs the genuine `opencode serve` underneath its TUI and drives it over
//! the loopback session API. That is how OpenCode's free models (Big Pickle
//! and whatever OpenCode serves free today) become usable with no key — the
//! free tier is only reachable from the real OpenCode client, so Workshop
//! mirrors OpenCode's own free catalog instead of impersonating it:
//!
//! ```text
//! opencode_engine::ensure_opencode   detect, or official installer (pinned)
//!   -> OpenCodeEngine::start         `opencode serve` on a free loopback port
//!   -> free_models()                 GET /config/providers -> zero-cost opencode/* rows
//!   -> create_session / prompt       POST prompt_async + SSE /event -> AdapterEvents
//!   -> TurnHandle::cancel            POST /session/{id}/abort, session survives
//!   -> shutdown                      SIGTERM, then SIGKILL the group
//! ```

pub mod adapter;
pub mod detect;
pub mod env;
pub mod event;
pub mod opencode_engine;
pub mod probe;
mod spawn;
pub mod status;
pub mod supervisor;
pub mod vendors;
pub mod worktree;

pub use adapter::{
    Adapter, AdapterId, AskReply, LoginState, NormalizeError, Normalizer, PermissionPolicy,
    PinStatus, ProbeOutput, PromptDelivery, RunRequest, Terminal, VersionPin,
};
pub use detect::{DetectOptions, Detection, InstalledCli, detect};
pub use event::{AdapterEvent, QuestionChoice, QuestionPrompt, Usage, question_answers_prompt};
pub use status::{RailPill, RailStatus, probe_login, rail_status};
pub use supervisor::{Replier, RunHandle, RunOutcome, SpawnError, SupervisorOptions, spawn};
pub use worktree::{GitWorktreeIsolation, InPlace, IsolatedWorkspace, WorkspaceIsolation};
