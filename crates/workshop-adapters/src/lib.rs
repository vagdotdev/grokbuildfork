//! Workshop agent adapters: drive the official Claude Code, Codex, Cursor
//! Agent, and OpenCode CLIs as subscription-backed agents.
//!
//! Spawn, do not steal. Workshop never reads another app's credentials —
//! not `~/.claude`, not `~/.codex/auth.json`, not OpenCode's `auth.json`,
//! not `~/.cursor/sdk/auth.json`, not a keychain. Login state comes from the
//! vendor's own status command; login itself is the vendor's own login
//! command attached to the user's terminal.
//!
//! Who the vendors are, where their CLIs live, whether a binary really is the
//! vendor's and whether the user is signed in is `workshop-detect`'s job — the
//! one detection stack, shared with the `/model` picker, so a vendor row and
//! the turn that follows it always see the same CLI. This crate is spawn and
//! stream normalization only.
//!
//! Flow:
//!
//! ```text
//! detect(adapter, cfg)           workshop-detect: PATH, known dirs, --version/--help identity
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
pub mod env;
pub mod event;
pub mod opencode_engine;
mod spawn;
pub mod supervisor;
pub mod vendors;
pub mod worktree;

pub use adapter::{
    Adapter, AdapterId, AskReply, NormalizeError, Normalizer, PermissionPolicy, PinStatus,
    PromptDelivery, RunRequest, Terminal, VersionPin,
};
pub use workshop_detect::LoginState;
pub use workshop_detect::VendorProbe as ProbeOutput;
pub use event::{AdapterEvent, QuestionChoice, QuestionPrompt, Usage, question_answers_prompt};
pub use supervisor::{Replier, RunHandle, RunOutcome, SpawnError, SupervisorOptions, spawn};
pub use workshop_detect::{DetectConfig, Detection, Identity as InstalledCli};
pub use worktree::{GitWorktreeIsolation, InPlace, IsolatedWorkspace, WorkspaceIsolation};

/// Locate and verify `adapter`'s CLI through the detection stack the picker uses (`PATH`, the
/// known install dirs, `cfg.preferred_dirs`; the binary's own `--version` / `--help` as its
/// identity). Blocking work runs off the async runtime's threads.
pub async fn detect(adapter: &dyn Adapter, cfg: &DetectConfig) -> Detection {
    let vendor = adapter.id();
    let cfg = cfg.clone();
    tokio::task::spawn_blocking(move || workshop_detect::detect_vendor(vendor, &cfg))
        .await
        .unwrap_or_else(|e| Detection::Unverified {
            path: std::path::PathBuf::new(),
            reason: format!("detection task failed: {e}"),
        })
}
