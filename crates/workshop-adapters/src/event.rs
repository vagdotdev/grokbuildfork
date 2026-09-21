use serde::{Deserialize, Serialize};
use workshop_detect::Vendor;

use crate::error::FailureReason;

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}

/// One normalized event from any adapter. Vendor-specific payloads are reduced to these shapes;
/// anything unrecognized is surfaced as [`AdapterEvent::Unknown`] and counted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AdapterEvent {
    /// The child process started.
    Started { vendor: Vendor, pid: Option<u32> },
    /// The vendor session id became known (needed for resume).
    Session { id: String },
    /// A complete assistant message.
    Text { text: String },
    /// Reasoning / thinking text the CLI chose to expose.
    Thinking { text: String },
    ToolStarted {
        id: Option<String>,
        name: String,
        detail: Option<String>,
    },
    ToolCompleted {
        id: Option<String>,
        name: String,
        ok: Option<bool>,
        detail: Option<String>,
    },
    Usage(Usage),
    /// A line the CLI wrote to stderr (already bounded).
    Stderr { line: String },
    /// A JSON event with a `type` this adapter does not model.
    Unknown { kind: String },
    /// The vendor's terminal success event was seen.
    Completed {
        final_text: Option<String>,
        session_id: Option<String>,
    },
    /// The run failed; the supervisor emits this exactly once, last.
    Failed { reason: FailureReason },
    /// The run was cancelled; emitted exactly once, last.
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RunStatus {
    Completed,
    Failed { reason: FailureReason },
    Cancelled,
}

/// Summary returned when the child has exited and the event stream is closed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunOutcome {
    pub vendor: Vendor,
    pub status: RunStatus,
    /// Vendor session id, for [`crate::RunRequest::resume`].
    pub session_id: Option<String>,
    /// Last assistant message.
    pub final_text: Option<String>,
    pub exit_code: Option<i32>,
    pub usage: Usage,
    pub events_seen: usize,
    pub unknown_events: usize,
    /// Last few stderr lines, for diagnostics.
    pub stderr_tail: Vec<String>,
}

impl RunOutcome {
    pub fn is_completed(&self) -> bool {
        matches!(self.status, RunStatus::Completed)
    }
}
