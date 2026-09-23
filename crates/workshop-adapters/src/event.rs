//! The single event vocabulary every vendor stream is normalized into.

use serde::{Deserialize, Serialize};

/// One normalized event from a delegated CLI run.
///
/// Invariant maintained by the supervisor: the last event of a run is always
/// `Done` or `Error`. `Error` may also appear mid-run for non-fatal vendor
/// errors (for example Codex "Reconnecting..." notices); consumers should use
/// [`crate::RunOutcome`] to learn how the run ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AdapterEvent {
    /// A chunk of assistant-visible text. Vendors without token streaming emit
    /// whole messages as one delta.
    TextDelta { text: String },
    /// A chunk of model reasoning / thinking text.
    Thinking { text: String },
    /// The agent invoked a tool. `id` correlates with a later `ToolResult`.
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// Vendor detail for a finished tool call, emitted right before its `ToolResult` when the
    /// backend reports more than plain output: the tool's own title (`hello.txt`, the command)
    /// and its metadata (`opencode serve`: `exit`, `output`, `filediff`, `diff`, …). Hosts that
    /// only need the text can ignore it.
    ToolDetail {
        id: String,
        title: Option<String>,
        metadata: serde_json::Value,
    },
    /// The outcome of a tool call.
    ToolResult {
        id: String,
        output: String,
        is_error: bool,
    },
    /// Token accounting for the portion of the run that just finished.
    /// Vendors may report several increments per run; sum them.
    Usage(Usage),
    /// The run finished successfully.
    Done {
        /// Vendor session / thread id usable with [`crate::RunRequest::resume`].
        session_id: Option<String>,
        /// Final assistant text when the vendor reports one separately.
        result: Option<String>,
    },
    /// A vendor-reported error. Fatal when it is the last event of the run.
    Error { message: String },
}

/// Token usage reported by a vendor CLI. All counts are incremental.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    /// Vendor cost estimate when reported. Subscription runs usually report
    /// zero or nothing; never treat this as a bill.
    pub cost_usd: Option<f64>,
}

impl Usage {
    /// Add another increment into this one.
    pub fn add(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_write_tokens += other.cache_write_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
        self.cost_usd = match (self.cost_usd, other.cost_usd) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
        };
    }
}
