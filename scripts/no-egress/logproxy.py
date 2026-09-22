#!/usr/bin/env python3
"""Logging HTTP proxy that records every requested host and refuses to forward (502).

Anything honoring HTTP(S)_PROXY shows up in the log by exact hostname; nothing leaves the box.
"""
import argparse, socket, threading, time

def handle(conn, log):
    try:
        conn.settimeout(5)
        data = b""
        while b"\r\n\r\n" not in data and len(data) < 65536:
            chunk = conn.recv(4096)
            if not chunk:
                break
            data += chunk
        line = data.split(b"\r\n", 1)[0].decode("latin1", "replace")
        parts = line.split(" ")
        target = parts[1] if len(parts) > 1 else "?"
        if parts and parts[0] != "CONNECT":
            # absolute-form http request: extract host header
            for h in data.split(b"\r\n"):
                if h.lower().startswith(b"host:"):
                    target = h.split(b":", 1)[1].strip().decode("latin1", "replace") + " (" + target + ")"
        with open(log, "a") as f:
            f.write("%s %s %s\n" % (time.strftime("%H:%M:%S"), parts[0] if parts else "?", target))
        conn.sendall(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
    except Exception as e:  # noqa
        with open(log, "a") as f:
            f.write("%s ERROR %r\n" % (time.strftime("%H:%M:%S"), e))
    finally:
        conn.close()

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=3128)
    ap.add_argument("--log", required=True)
    a = ap.parse_args()
    open(a.log, "a").close()
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(("127.0.0.1", a.port))
    s.listen(64)
    while True:
        c, _ = s.accept()
        threading.Thread(target=handle, args=(c, a.log), daemon=True).start()

if __name__ == "__main__":
    main()
