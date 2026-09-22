#!/usr/bin/env python3
"""Static file server with HTTP Range support for the installer smoke tests.

python3 -m http.server ignores Range headers, so it cannot prove `.partial` resume; GitHub
release assets and Hugging Face both honor ranges. Usage: range-httpd.py PORT [DIR]
Logs one line per request to stderr: `GET /path 200|206|404 bytes`.
Set RANGE_HTTPD_THROTTLE_BPS to cap the transfer rate (lets a test kill a download mid-way).
"""
import os
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

THROTTLE_BPS = int(os.environ.get("RANGE_HTTPD_THROTTLE_BPS", "0") or 0)


class Handler(BaseHTTPRequestHandler):
    root = "."

    def do_GET(self):
        path = os.path.normpath(os.path.join(self.root, self.path.lstrip("/").split("?")[0]))
        if not path.startswith(os.path.abspath(self.root)) or not os.path.isfile(path):
            self.send_response(404)
            self.end_headers()
            self.log("404", 0)
            return
        size = os.path.getsize(path)
        start, end = 0, size - 1
        status = 200
        rng = self.headers.get("Range")
        if rng and rng.startswith("bytes="):
            spec = rng[len("bytes="):].split("-")
            try:
                start = int(spec[0]) if spec[0] else 0
                if spec[1]:
                    end = min(int(spec[1]), size - 1)
            except ValueError:
                start = 0
            if start >= size:
                self.send_response(416)
                self.send_header("Content-Range", f"bytes */{size}")
                self.end_headers()
                self.log("416", 0)
                return
            status = 206
        length = end - start + 1
        self.send_response(status)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Length", str(length))
        if status == 206:
            self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
        self.end_headers()
        with open(path, "rb") as f:
            f.seek(start)
            remaining = length
            while remaining > 0:
                chunk = f.read(min(1 << 20, remaining))
                if not chunk:
                    break
                try:
                    self.wfile.write(chunk)
                except BrokenPipeError:
                    break
                remaining -= len(chunk)
                if THROTTLE_BPS > 0:
                    time.sleep(len(chunk) / THROTTLE_BPS)
        self.log(str(status), length)

    def log(self, status, length):
        sys.stderr.write(f'GET {self.path} {status} {length}\n')
        sys.stderr.flush()

    def log_message(self, *args):  # quiet the default access log
        pass


if __name__ == "__main__":
    port = int(sys.argv[1])
    Handler.root = os.path.abspath(sys.argv[2] if len(sys.argv) > 2 else ".")
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
