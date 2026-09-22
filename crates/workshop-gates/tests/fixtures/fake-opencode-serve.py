#!/usr/bin/env python3
"""`opencode serve` that is healthy but whose model never answers.

Speaks just enough of the real server's loopback API for Workshop to get past startup: the
"listening on" line, /global/health, POST /session, the /event SSE greeting, and a 200 for
prompt_async and abort. No message event ever follows, which is the "engine up, nothing comes
back" failure the first-event timeout must turn into one visible line.
"""
import json
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

args = sys.argv[1:]
port = int(args[args.index("--port") + 1]) if "--port" in args else 4096


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def _json(self, obj, status=200):
        body = json.dumps(obj).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        path = self.path.split("?")[0]
        if path == "/global/health":
            return self._json({"healthy": True, "version": "1.18.31"})
        if path == "/event":
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Cache-Control", "no-cache")
            self.end_headers()
            self.wfile.write(b'data: {"type":"server.connected","properties":{}}\n\n')
            self.wfile.flush()
            while True:  # keep the stream open, never emit a message event
                time.sleep(1)
        if path == "/config/providers":
            return self._json({"providers": [], "default": {}})
        return self._json({"error": "not found"}, 404)

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        if length:
            self.rfile.read(length)
        path = self.path.split("?")[0]
        if path == "/session":
            return self._json({"id": "ses_fake_silent"})
        if path.endswith("/prompt_async") or path.endswith("/abort"):
            return self._json({})
        return self._json({"error": "not found"}, 404)


server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
server.daemon_threads = True
print(f"opencode server listening on http://127.0.0.1:{port}", flush=True)
threading.Thread(target=server.serve_forever, daemon=True).start()
while True:
    time.sleep(3600)
