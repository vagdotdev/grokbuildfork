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
    /// The agent asked the user something (its ask-user-question tool). The host puts the
    /// questions to the user and answers with [`crate::AskReply::Answer`]; when the run has no
    /// control channel for that (a headless CLI that already told itself the question was
    /// skipped), the answers reach the CLI as the next prompt of the same session
    /// ([`question_answers_prompt`]) once this run ends. No `ToolCall`/`ToolResult` pair is
    /// emitted for the call.
    Question {
        /// The vendor's ask id (its control request, else its tool call).
        id: String,
        questions: Vec<QuestionPrompt>,
    },
    /// The CLI asks before a tool call on its control channel (Claude Code's `can_use_tool`)
    /// and is blocked until the host answers with [`crate::AskReply::Allow`] / [`crate::AskReply::Deny`].
    /// `tool` and `input` are in the shared vocabulary (`bash` with `command`, `edit` with
    /// `filePath`, …), as the matching `ToolCall` will be.
    PermissionAsk {
        id: String,
        tool: String,
        input: serde_json::Value,
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

/// One choice of a [`QuestionPrompt`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuestionChoice {
    pub label: String,
    #[serde(default)]
    pub description: String,
}

/// One question the agent asks the user, in the shape OpenCode's `question` tool, Claude Code's
/// `AskUserQuestion` and Cursor's `askQuestionToolCall` all share.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuestionPrompt {
    pub question: String,
    #[serde(default)]
    pub header: String,
    #[serde(default)]
    pub options: Vec<QuestionChoice>,
    /// More than one choice may be picked.
    #[serde(default)]
    pub multiple: bool,
}

/// The user's answers to an [`AdapterEvent::Question`] as the follow-up prompt that continues a
/// headless CLI session: one line per question with the chosen labels (or typed text), so the
/// model that was told "questions skipped" gets them in the user's next message.
pub fn question_answers_prompt(questions: &[QuestionPrompt], answers: &[Vec<String>]) -> String {
    let mut prompt = String::from("My answers to your questions:\n");
    for (i, q) in questions.iter().enumerate() {
        let answer = answers
            .get(i)
            .map(|a| a.join(", "))
            .filter(|a| !a.trim().is_empty())
            .unwrap_or_else(|| "(no answer)".to_string());
        prompt.push_str(&format!("- {}: {answer}\n", q.question.trim()));
    }
    prompt.push_str("Continue with the task using these answers.");
    prompt
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_prompt_lists_every_question_with_its_answers() {
        let questions = vec![
            QuestionPrompt {
                question: "How should Ghostty be installed? ".into(),
                header: "Install".into(),
                options: vec![],
                multiple: false,
            },
            QuestionPrompt {
                question: "Which extras?".into(),
                header: String::new(),
                options: vec![],
                multiple: true,
            },
            QuestionPrompt {
                question: "Anything else?".into(),
                header: String::new(),
                options: vec![],
                multiple: false,
            },
        ];
        let answers = vec![
            vec!["PPA (Recommended)".into()],
            vec!["Themes".into(), "Shell integration".into()],
        ];
        assert_eq!(
            question_answers_prompt(&questions, &answers),
            "My answers to your questions:\n\
             - How should Ghostty be installed?: PPA (Recommended)\n\
             - Which extras?: Themes, Shell integration\n\
             - Anything else?: (no answer)\n\
             Continue with the task using these answers."
        );
    }
}
