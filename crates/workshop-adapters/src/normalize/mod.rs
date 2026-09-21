//! Vendor JSON line → [`AdapterEvent`] normalizers.
//!
//! Each normalizer is a small state machine over the CLI's documented JSON stream. Unknown event
//! types become [`AdapterEvent::Unknown`]; a line that is not a JSON object is reported to the
//! supervisor as drift. Terminal success is only ever the vendor's own terminal event.

use serde_json::Value;
use workshop_detect::Vendor;

use crate::event::AdapterEvent;

mod claude;
mod codex;
mod cursor;
mod opencode;

pub use claude::ClaudeNormalizer;
pub use codex::CodexNormalizer;
pub use cursor::CursorNormalizer;
pub use opencode::OpenCodeNormalizer;

/// Outcome of feeding one stdout line.
#[derive(Debug, Clone, PartialEq)]
pub enum LineResult {
    Events(Vec<AdapterEvent>),
    /// The line was not a JSON object. The supervisor decides whether this is fatal.
    NotJson(String),
    /// Blank line; ignored.
    Empty,
}

pub trait Normalizer: Send {
    fn vendor(&self) -> Vendor;
    /// Normalize one parsed JSON object.
    fn object(&mut self, value: &Value) -> Vec<AdapterEvent>;
    /// Session id observed so far.
    fn session_id(&self) -> Option<&str>;
    /// Last assistant text observed so far.
    fn last_text(&self) -> Option<&str>;
    /// Whether the vendor's terminal success event has been seen.
    fn completed(&self) -> bool;

    fn line(&mut self, line: &str) -> LineResult {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return LineResult::Empty;
        }
        match serde_json::from_str::<Value>(trimmed) {
            Ok(v) if v.is_object() => LineResult::Events(self.object(&v)),
            _ => LineResult::NotJson(trimmed.chars().take(200).collect()),
        }
    }
}

pub fn for_vendor(vendor: Vendor) -> Box<dyn Normalizer> {
    match vendor {
        Vendor::Claude => Box::new(ClaudeNormalizer::default()),
        Vendor::Codex => Box::new(CodexNormalizer::default()),
        Vendor::Cursor => Box::new(CursorNormalizer::default()),
        Vendor::OpenCode => Box::new(OpenCodeNormalizer::default()),
    }
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn u64_field(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(Value::as_u64)
}

fn compact(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.chars().take(400).collect()),
        other => Some(other.to_string().chars().take(400).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_json_and_blank_lines_are_classified() {
        let mut n = for_vendor(Vendor::Codex);
        assert_eq!(n.line("   "), LineResult::Empty);
        assert_eq!(
            n.line("Reading additional input from stdin..."),
            LineResult::NotJson("Reading additional input from stdin...".into())
        );
        assert_eq!(n.line("[1,2]"), LineResult::NotJson("[1,2]".into()));
        assert!(matches!(n.line(r#"{"type":"turn.started"}"#), LineResult::Events(_)));
    }
}
