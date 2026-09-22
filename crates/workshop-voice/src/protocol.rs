//! Client side of the `voice-engine` protocol (the helper's `voice/engine/src/protocol.rs` is the
//! reference; the byte layout is pinned by tests on both sides).
//!
//! stdin → helper: `type: u8`, `len: u32 LE`, payload. 1 audio (PCM s16le 16 kHz mono),
//! 2 start (JSON `{"language": …}`), 3 stop, 4 quit. Helper → stdout: one JSON object per line
//! tagged `ready` / `partial` / `final` / `error`.

use serde::Deserialize;

pub const FRAME_AUDIO: u8 = 1;
pub const FRAME_START: u8 = 2;
pub const FRAME_STOP: u8 = 3;
pub const FRAME_QUIT: u8 = 4;

pub fn encode_frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(kind);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

pub fn start_frame(language: Option<&str>) -> Vec<u8> {
    let body = match language.filter(|l| !l.is_empty() && *l != "auto") {
        Some(l) => serde_json::json!({ "language": l }).to_string(),
        None => "{}".to_owned(),
    };
    encode_frame(FRAME_START, body.as_bytes())
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineMessage {
    Ready {
        #[serde(default)]
        model: String,
        #[serde(default)]
        load_ms: u64,
        #[serde(default)]
        probe_ms: u64,
        #[serde(default)]
        gpu: bool,
    },
    Partial {
        #[serde(default)]
        text: String,
        #[serde(default)]
        decode_ms: u64,
    },
    Final {
        #[serde(default)]
        text: String,
        #[serde(default)]
        decode_ms: u64,
        #[serde(default)]
        language: Option<String>,
    },
    Error {
        #[serde(default)]
        message: String,
    },
    #[serde(other)]
    Unknown,
}

pub fn parse_line(line: &str) -> Option<EngineMessage> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    serde_json::from_str(line).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_layout_is_type_len_le_payload() {
        assert_eq!(
            encode_frame(FRAME_AUDIO, &[9, 8]),
            vec![1, 2, 0, 0, 0, 9, 8]
        );
        assert_eq!(encode_frame(FRAME_STOP, &[]), vec![3, 0, 0, 0, 0]);
        assert_eq!(
            start_frame(Some("de")),
            encode_frame(FRAME_START, br#"{"language":"de"}"#)
        );
        assert_eq!(start_frame(Some("auto")), encode_frame(FRAME_START, b"{}"));
        assert_eq!(start_frame(None), encode_frame(FRAME_START, b"{}"));
    }

    #[test]
    fn parses_helper_lines() {
        assert_eq!(
            parse_line(r#"{"type":"ready","model":"m","load_ms":1,"probe_ms":2,"gpu":false}"#),
            Some(EngineMessage::Ready {
                model: "m".into(),
                load_ms: 1,
                probe_ms: 2,
                gpu: false
            })
        );
        assert_eq!(
            parse_line(r#"{"type":"partial","text":"hi","decode_ms":40}"#),
            Some(EngineMessage::Partial {
                text: "hi".into(),
                decode_ms: 40
            })
        );
        assert_eq!(
            parse_line(r#"{"type":"final","text":"done","decode_ms":9,"language":"en"}"#),
            Some(EngineMessage::Final {
                text: "done".into(),
                decode_ms: 9,
                language: Some("en".into())
            })
        );
        assert_eq!(
            parse_line(r#"{"type":"something_new"}"#),
            Some(EngineMessage::Unknown)
        );
        assert_eq!(parse_line("  "), None);
        assert_eq!(parse_line("not json"), None);
    }
}
