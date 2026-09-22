//! Wire protocol between the TUI and `voice-engine`.
//!
//! **stdin → engine: length-prefixed frames.** `type: u8`, `len: u32` little-endian, `payload`.
//!
//! | type | name  | payload |
//! |---|---|---|
//! | 1 | audio | raw PCM, s16le, 16 kHz, mono (what `__mic-capture` emits; no resampling) |
//! | 2 | start | JSON `{"language": "de"}`; `language` omitted or null = auto-detect |
//! | 3 | stop  | empty; run the final decode over the whole utterance, then emit `final` |
//! | 4 | quit  | empty; exit 0 |
//!
//! Closing stdin behaves like `stop` for an open utterance followed by `quit`, so a parent that
//! dies never leaves an orphaned helper holding 1 GB of weights.
//!
//! **engine → stdout: one JSON object per line.**
//!
//! - `{"type":"ready","model":"…","load_ms":N,"probe_ms":N,"gpu":true|false}` once, after the
//!   weights are loaded and a one-second probe decode succeeded.
//! - `{"type":"partial","text":"…"}` roughly every 500 ms while an utterance is open.
//! - `{"type":"final","text":"…"}` after `stop`; the only text the prompt commits. Empty when
//!   nothing speakable was heard.
//! - `{"type":"error","message":"…"}` before a non-zero exit.
//!
//! Frames rather than a raw PCM pipe so one warm helper can serve several utterances: the
//! `start`/`stop` boundaries travel in-band with the audio.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

pub const FRAME_AUDIO: u8 = 1;
pub const FRAME_START: u8 = 2;
pub const FRAME_STOP: u8 = 3;
pub const FRAME_QUIT: u8 = 4;

/// Refuse absurd frames (an audio frame is ~10–64 ms of PCM; `start` is a few bytes of JSON).
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub enum Frame {
    Audio(Vec<u8>),
    Start(StartOptions),
    Stop,
    Quit,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct StartOptions {
    /// Concrete Whisper language code (`en`, `de`, `ja`, …). `None` = detect.
    pub language: Option<String>,
}

/// Read one frame; `Ok(None)` on a clean EOF at a frame boundary.
pub fn read_frame(input: &mut impl Read) -> io::Result<Option<Frame>> {
    let mut header = [0u8; 5];
    match input.read_exact(&mut header) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let kind = header[0];
    let len = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if len > MAX_FRAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame of {len} bytes exceeds the {MAX_FRAME_LEN} byte limit"),
        ));
    }
    let mut payload = vec![0u8; len];
    input.read_exact(&mut payload)?;
    match kind {
        FRAME_AUDIO => Ok(Some(Frame::Audio(payload))),
        FRAME_START => {
            let opts = if payload.is_empty() {
                StartOptions::default()
            } else {
                serde_json::from_slice(&payload).map_err(|e| {
                    io::Error::new(io::ErrorKind::InvalidData, format!("bad start frame: {e}"))
                })?
            };
            Ok(Some(Frame::Start(opts)))
        }
        FRAME_STOP => Ok(Some(Frame::Stop)),
        FRAME_QUIT => Ok(Some(Frame::Quit)),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown frame type {other}"),
        )),
    }
}

/// Encode a frame. The TUI-side client (`crates/workshop-voice`) carries the same layout; the
/// round-trip test below pins it.
#[cfg(test)]
pub fn encode_frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(kind);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message<'a> {
    /// Weights loaded and a one-second probe decode done. `probe_ms` is what one interim decode
    /// costs on this machine; the parent steps down a model tier when it cannot fit a partial
    /// into about a second.
    Ready {
        model: &'a str,
        load_ms: u64,
        probe_ms: u64,
        gpu: bool,
    },
    Partial {
        text: &'a str,
        /// Wall time of this decode; lets the parent watch cadence without guessing.
        decode_ms: u64,
    },
    Final {
        text: &'a str,
        decode_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        language: Option<&'a str>,
    },
    Error {
        message: &'a str,
    },
}

/// One JSON line, flushed immediately (the parent reads line by line).
pub fn emit(message: &Message<'_>) -> io::Result<()> {
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, message)?;
    out.write_all(b"\n")?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let mut bytes = Vec::new();
        bytes.extend(encode_frame(FRAME_START, br#"{"language":"de"}"#));
        bytes.extend(encode_frame(FRAME_AUDIO, &[1, 2, 3, 4]));
        bytes.extend(encode_frame(FRAME_STOP, &[]));
        bytes.extend(encode_frame(FRAME_QUIT, &[]));
        let mut cursor = io::Cursor::new(bytes);
        match read_frame(&mut cursor).unwrap() {
            Some(Frame::Start(opts)) => assert_eq!(opts.language.as_deref(), Some("de")),
            other => panic!("{other:?}"),
        }
        match read_frame(&mut cursor).unwrap() {
            Some(Frame::Audio(pcm)) => assert_eq!(pcm, vec![1, 2, 3, 4]),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            read_frame(&mut cursor).unwrap(),
            Some(Frame::Stop)
        ));
        assert!(matches!(
            read_frame(&mut cursor).unwrap(),
            Some(Frame::Quit)
        ));
        assert!(read_frame(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn start_without_payload_means_auto() {
        let mut cursor = io::Cursor::new(encode_frame(FRAME_START, &[]));
        match read_frame(&mut cursor).unwrap() {
            Some(Frame::Start(opts)) => assert!(opts.language.is_none()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_and_oversized_frames() {
        let mut cursor = io::Cursor::new(encode_frame(9, &[]));
        assert!(read_frame(&mut cursor).is_err());
        let mut huge = vec![FRAME_AUDIO];
        huge.extend_from_slice(&(u32::MAX).to_le_bytes());
        let mut cursor = io::Cursor::new(huge);
        assert!(read_frame(&mut cursor).is_err());
    }

    #[test]
    fn messages_serialize_as_tagged_lines() {
        let s = serde_json::to_string(&Message::Partial {
            text: "hi",
            decode_ms: 7,
        })
        .unwrap();
        assert_eq!(s, r#"{"type":"partial","text":"hi","decode_ms":7}"#);
        let s = serde_json::to_string(&Message::Final {
            text: "done",
            decode_ms: 9,
            language: Some("de"),
        })
        .unwrap();
        assert_eq!(
            s,
            r#"{"type":"final","text":"done","decode_ms":9,"language":"de"}"#
        );
        let s = serde_json::to_string(&Message::Ready {
            model: "m.bin",
            load_ms: 1,
            probe_ms: 2,
            gpu: false,
        })
        .unwrap();
        assert!(s.starts_with(r#"{"type":"ready""#));
    }
}
