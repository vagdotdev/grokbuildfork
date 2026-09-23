#!/usr/bin/env python3
"""A loopback OpenAI-compatible endpoint that refuses every request with HTTP 400.

Stands in for the silent fallback's provider in the engine-failure gates: the fallback is tried
(the request is logged here) and fails at once, without retries or network, so the one plain
failure line must appear. Prints the base URL it listens on as its first stdout line.
"""
import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

port = int(sys.argv[1]) if len(sys.argv) > 1 else 0


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def _refuse(self):
        length = int(self.headers.get("Content-Length") or 0)
        if length:
            self.rfile.read(length)
        body = json.dumps({"error": {"message": "fake provider: refused (fault injected)", "type": "invalid_request_error"}}).encode()
        self.send_response(400)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    do_GET = _refuse
    do_POST = _refuse


server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
server.daemon_threads = True
print(f"http://127.0.0.1:{server.server_address[1]}/v1", flush=True)
server.serve_forever()
