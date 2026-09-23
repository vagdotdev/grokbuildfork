#!/usr/bin/env python3
"""A loopback stand-in for the release mirror the background voice setup fetches from.

Serves the files of one directory (`SHA256SUMS`, the `voice-engine-*.tar.gz` helper archive, the
model files a test lock pins). Every request path is appended to --log, so a gate can check that
the setup fetched exactly those files and nothing else. --slow-suffix throttles files with that
suffix (16 KiB per 50 ms) so a gate can watch `/voice` mid-download. Prints its base URL first.
"""
import argparse
import os
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ap = argparse.ArgumentParser()
ap.add_argument("--dir", required=True)
ap.add_argument("--log", required=True)
ap.add_argument("--slow-suffix", default=None)
a = ap.parse_args()


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def do_GET(self):
        path = self.path.split("?")[0]
        with open(a.log, "a") as f:
            f.write(path + "\n")
        name = os.path.basename(path)
        full = os.path.join(a.dir, name)
        if not name or not os.path.isfile(full):
            body = b"not found"
            self.send_response(404)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        size = os.path.getsize(full)
        self.send_response(200)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Length", str(size))
        self.end_headers()
        slow = a.slow_suffix and name.endswith(a.slow_suffix)
        try:
            with open(full, "rb") as f:
                while True:
                    chunk = f.read(16 * 1024)
                    if not chunk:
                        break
                    self.wfile.write(chunk)
                    self.wfile.flush()
                    if slow:
                        time.sleep(0.05)
        except (BrokenPipeError, ConnectionResetError):
            # The client quit mid-download (a gate ending early); nothing to report.
            pass


srv = ThreadingHTTPServer(("127.0.0.1", 0), H)
srv.daemon_threads = True
print("http://127.0.0.1:%d" % srv.server_address[1], flush=True)
srv.serve_forever()
