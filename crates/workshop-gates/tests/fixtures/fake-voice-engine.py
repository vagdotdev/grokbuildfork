#!/usr/bin/env python3
"""A stand-in for the `voice-engine` helper (whisper.cpp) that speaks its protocol and returns a
canned transcript, so a PTY gate can drive dictation start → recording → stop without a model,
a microphone or a CPU-heavy decode.

stdin frames from the TUI: `type: u8`, `len: u32 LE`, payload — 1 audio (PCM s16le 16 kHz mono),
2 start (JSON), 3 stop, 4 quit. stdout: one JSON object per line tagged `ready` / `partial` /
`final` / `error` (`crates/workshop-voice/src/protocol.rs`).

After half a second of audio it reports the first words as a partial (the live preview); on
`stop` it returns the whole sentence as the final. `FAKE_VOICE_TRANSCRIPT` overrides the text.
"""
import json
import os
import struct
import sys

TRANSCRIPT = os.environ.get("FAKE_VOICE_TRANSCRIPT", "hello from the fake microphone")
BYTES_PER_SECOND = 16000 * 2


def emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def main():
    if "--version" in sys.argv[1:]:
        print("voice-engine 0.0.0-test")
        return 0
    if "--probe" in sys.argv[1:]:
        print("1")
        return 0
    emit({"type": "ready", "model": "fake", "load_ms": 1, "probe_ms": 1, "gpu": False})
    stdin = sys.stdin.buffer
    audio = 0
    partial_sent = False
    while True:
        header = stdin.read(5)
        if len(header) < 5:
            return 0
        kind = header[0]
        (length,) = struct.unpack("<I", header[1:5])
        payload = stdin.read(length) if length else b""
        if kind == 2:
            audio = 0
            partial_sent = False
        elif kind == 1:
            audio += len(payload)
            if not partial_sent and audio >= BYTES_PER_SECOND // 2:
                partial_sent = True
                emit({"type": "partial", "text": " ".join(TRANSCRIPT.split()[:3]), "decode_ms": 5})
        elif kind == 3:
            emit({"type": "final", "text": TRANSCRIPT, "decode_ms": 5, "language": "en"})
            audio = 0
            partial_sent = False
        elif kind == 4:
            return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except BrokenPipeError:
        sys.exit(0)
