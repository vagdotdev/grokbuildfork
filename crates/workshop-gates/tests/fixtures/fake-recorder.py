#!/usr/bin/env python3
"""A stand-in microphone recorder for Linux capture. Installed under the names the capture backend
looks for on `PATH` (`pw-record`, `parec`, `arecord`), it streams silence — raw PCM s16le 16 kHz
mono, 100 ms every 100 ms — to stdout until it is killed or its reader goes away. `--help` names
`--raw` so the `pw-record` capability probe is satisfied.
"""
import sys
import time

if "--help" in sys.argv[1:]:
    print("Usage: fake-recorder [options]\n  --raw  write raw samples\n  --rate RATE\n  --channels N\n  --format FMT")
    sys.exit(0)

CHUNK = b"\x00" * (16000 * 2 // 10)
try:
    while True:
        sys.stdout.buffer.write(CHUNK)
        sys.stdout.buffer.flush()
        time.sleep(0.1)
except (BrokenPipeError, KeyboardInterrupt):
    pass
