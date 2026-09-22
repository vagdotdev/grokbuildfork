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

/// Small JSON accessors shared by the normalizers.
pub(crate) mod json {
    use serde_json::Value;

    pub fn str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
        v.get(key).and_then(Value::as_str)
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
