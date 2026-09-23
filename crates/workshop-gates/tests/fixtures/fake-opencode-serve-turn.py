#!/usr/bin/env python3
"""A loopback stand-in for `opencode serve` that answers.

Serves health, a captured `/config/providers` (--providers), session create, the `/event`
stream, and a prompt that replays a captured turn (--turn, JSON lines of events, --pace seconds
between them so a gate can watch the turn mid-way). With --record every `prompt_async` body is
appended to that file as one JSON line, so a gate can check what reached the engine boundary
(model, variant, agent). Started by a fake `opencode` binary's `serve` subcommand:
`serve --hostname 127.0.0.1 --port N`.
"""
import argparse
import json
import queue
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ap = argparse.ArgumentParser()
ap.add_argument("--port", type=int, required=True)
ap.add_argument("--providers", required=True)
ap.add_argument("--turn", required=True)
ap.add_argument("--record", default=None)
ap.add_argument("--pace", type=float, default=0.01)
a = ap.parse_args()
PROVIDERS = open(a.providers, "rb").read()
TURN = [json.loads(l) for l in open(a.turn) if l.strip()]
SESSION = "ses_fake0001"
subs, lock = [], threading.Lock()


def broadcast(ev):
    with lock:
        targets = list(subs)
    for q in targets:
        q.put(ev)


def replay():
    time.sleep(0.05)
    for ev in TURN:
        broadcast(ev)
        time.sleep(a.pace)


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def _raw(self, code, body, ctype="application/json"):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _json(self, code, obj):
        self._raw(code, json.dumps(obj).encode())

    def do_GET(self):
        path = self.path.split("?")[0]
        if path == "/global/health":
            return self._json(200, {"healthy": True, "version": "1.18.31"})
        if path == "/config/providers":
            return self._raw(200, PROVIDERS)
        if path == "/session/" + SESSION:
            return self._json(200, {"id": SESSION, "title": "fake", "directory": "/work"})
        if path == "/session/" + SESSION + "/message":
            return self._json(200, [])
        if path == "/event":
            q = queue.Queue()
            with lock:
                subs.append(q)
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Cache-Control", "no-cache")
            self.send_header("Connection", "close")
            self.end_headers()
            try:
                self.wfile.write(b"data: " + json.dumps({"type": "server.connected", "properties": {}}).encode() + b"\n\n")
                self.wfile.flush()
                while True:
                    try:
                        ev = q.get(timeout=1.0)
                    except queue.Empty:
                        continue
                    self.wfile.write(b"data: " + json.dumps(ev).encode() + b"\n\n")
                    self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError, OSError):
                pass
            finally:
                with lock:
                    if q in subs:
                        subs.remove(q)
            return
        self._json(404, {"error": "no route GET " + path})

    def do_POST(self):
        path = self.path.split("?")[0]
        n = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(n) if n else b""
        if path == "/session":
            return self._json(200, {"id": SESSION, "title": "Workshop", "directory": "/work"})
        if path == "/session/" + SESSION + "/prompt_async":
            if a.record:
                with open(a.record, "ab") as f:
                    f.write(body.strip() + b"\n")
            threading.Thread(target=replay, daemon=True).start()
            self.send_response(204)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if path == "/session/" + SESSION + "/abort":
            return self._json(200, True)
        if path.startswith("/session/" + SESSION + "/permissions/"):
            return self._json(200, True)
        self._json(404, {"error": "no route POST " + path})


srv = ThreadingHTTPServer(("127.0.0.1", a.port), H)
srv.daemon_threads = True
print("opencode server listening on http://127.0.0.1:%d" % a.port, flush=True)
srv.serve_forever()
