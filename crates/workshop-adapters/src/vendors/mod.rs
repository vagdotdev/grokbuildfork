//! Vendor adapters. Flags and stream shapes are pinned per version; see
//! each module's docs for the verified source.

mod claude;
mod codex;
mod cursor;
mod opencode;

pub use claude::ClaudeAdapter;
pub use codex::CodexAdapter;
pub use cursor::CursorAdapter;
pub use opencode::OpenCodeAdapter;

use crate::adapter::{Adapter, AdapterId};

/// All built-in adapters in picker order.
pub fn all() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(ClaudeAdapter),
        Box::new(CodexAdapter),
        Box::new(CursorAdapter),
        Box::new(OpenCodeAdapter),
    ]
}

/// Look up one adapter by id.
pub fn by_id(id: AdapterId) -> Box<dyn Adapter> {
    match id {
        AdapterId::Claude => Box::new(ClaudeAdapter),
        AdapterId::Codex => Box::new(CodexAdapter),
        AdapterId::Cursor => Box::new(CursorAdapter),
        AdapterId::OpenCode => Box::new(OpenCodeAdapter),
    }
}

/// The tool vocabulary every vendor stream is normalized into: OpenCode's names, which the pager
/// renders as its own `Run` / `Edit` / `Read` / `Fetch` / `Web Search` rows, with OpenCode's
/// argument keys (`command`, `filePath`, `content`, `oldString`/`newString`, `pattern`, `query`,
/// `url`) so the same row code summarizes every vendor's call.
pub(crate) mod tool {
    pub const BASH: &str = "bash";
    pub const EDIT: &str = "edit";
    pub const WRITE: &str = "write";
    pub const READ: &str = "read";
    pub const LIST: &str = "list";
    pub const GLOB: &str = "glob";
    pub const GREP: &str = "grep";
    pub const WEB_FETCH: &str = "webfetch";
    pub const WEB_SEARCH: &str = "websearch";
    pub const TODO_WRITE: &str = "todowrite";
    pub const TODO_READ: &str = "todoread";
    pub const TASK: &str = "task";

    /// A vendor's own tool name in the vocabulary's spelling when no mapping applies:
    /// `ExitPlanMode` / `switchModeToolCall` -> `exit_plan_mode` / `switch_mode`. Never a
    /// protobuf key or a CamelCase class name on screen.
    pub fn snake_case(name: &str) -> String {
        let name = name.strip_suffix("ToolCall").unwrap_or(name);
        let mut out = String::with_capacity(name.len() + 4);
        let mut prev_lower = false;
        for c in name.chars() {
            if c.is_ascii_uppercase() {
                if prev_lower {
                    out.push('_');
                }
                out.push(c.to_ascii_lowercase());
                prev_lower = false;
            } else {
                out.push(c);
                prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_names_are_snake_case_without_the_oneof_suffix() {
        assert_eq!(tool::snake_case("switchModeToolCall"), "switch_mode");
        assert_eq!(tool::snake_case("ExitPlanMode"), "exit_plan_mode");
        assert_eq!(tool::snake_case("readLintsToolCall"), "read_lints");
        assert_eq!(tool::snake_case("LS"), "ls");
        assert_eq!(tool::snake_case("todo_list"), "todo_list");
        assert_eq!(
            json::creation_diff("one\ntwo"),
            "@@ -0,0 +1,2 @@\n+one\n+two\n\\ No newline at end of file\n"
        );
    }
}

/// Small JSON accessors shared by the normalizers.
pub(crate) mod json {
    use serde_json::Value;

    pub fn str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
        v.get(key).and_then(Value::as_str)
    }

    /// The first key/value of a protobuf `oneof` rendered as a single-key object.
    pub fn oneof(v: &Value) -> Option<(&str, &Value)> {
        v.as_object()
            .and_then(|o| o.iter().next())
            .map(|(k, b)| (k.as_str(), b))
    }

    /// A unified diff that adds the whole of `content` as a new file.
    pub fn creation_diff(content: &str) -> String {
        let lines: Vec<&str> = content.split_inclusive('\n').collect();
        let mut diff = format!("@@ -0,0 +1,{} @@\n", lines.len());
        for line in &lines {
            diff.push('+');
            diff.push_str(line);
            if !line.ends_with('\n') {
                diff.push_str("\n\\ No newline at end of file\n");
            }
        }
        diff
    }

    pub fn u64_of(v: &Value, key: &str) -> u64 {
        v.get(key).and_then(Value::as_u64).unwrap_or(0)
    }

    pub fn bool_of(v: &Value, key: &str) -> Option<bool> {
        v.get(key).and_then(Value::as_bool)
    }

    /// Render a value that may be a string or structured content as text.
    pub fn text_of(v: &Value) -> String {
        match v {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            Value::Array(items) => items
                .iter()
                .map(|item| match item.get("text").and_then(Value::as_str) {
                    Some(t) => t.to_string(),
                    None => item.to_string(),
                })
                .collect::<Vec<_>>()
                .join("\n"),
            other => other.to_string(),
        }
    }
}
