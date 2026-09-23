#!/usr/bin/env python3
"""A scripted `opencode serve` for the engine-trust gates (loopback, no model, no network).

Speaks the 1.18.31 session API Workshop drives — health, /config/providers, POST /session,
GET /session/{id}, the /event SSE stream, prompt_async, the permission reply endpoint, abort — and
answers each prompt from a script keyed on the prompt text, emitting the exact event shapes captured
from the real server:

  * "create hello.txt" (build agent)  -> permission.asked (edit, with the unified diff) and, once
    the reply arrives, a completed `write` part; `once`/`always` writes the file into the cwd.
  * "rm -rf tmp" (build agent)        -> permission.asked (bash) then a completed `bash` part with
    metadata.exit; `once`/`always` really removes ./tmp so the effect is visible on disk.
  * "edit hello.txt"                  -> a completed `edit` part with metadata.diff/filediff.
  * "list files" / "ls"               -> a completed `bash` part (`ls -1`) with real output + exit 0.
  * "what are you"                    -> answers as Workshop's assistant when the server was given
    an instructions file naming Workshop (OPENCODE_CONFIG_CONTENT), else as "opencode".
  * "think"                           -> a reasoning part streamed before the answer part (and
    another one after the tool call of "list files").
  * "slow"                            -> waits 3 s before answering (to queue prompts behind it).
  * agent == plan                     -> never a tool part, never a permission ask: text only.

Every turn ends with a step-finish carrying tokens (total 8627 -> "8.6K") and goes idle. Every
prompt_async body and permission reply is appended to the `--log` file (JSON lines) so a gate can
pin the agent that was sent and the answer Workshop posted.
"""
import json
import os
import shutil
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

args = sys.argv[1:]


def arg(name, default=None):
    return args[args.index(name) + 1] if name in args else default


PORT = int(arg("--port", "4096"))
PROVIDERS = open(arg("--providers"), "rb").read()
LOG = arg("--log")
CWD = os.getcwd()

subs, lock = [], threading.Lock()
sessions = {}  # id -> {"messages": [...]}
permission_replies = {}  # permission id -> reply string
permission_events = {}  # permission id -> threading.Event
counter = [0]


def next_id(prefix):
    counter[0] += 1
    return "%s_%08d" % (prefix, counter[0])


def log(obj):
    if LOG:
        with open(LOG, "a") as f:
            f.write(json.dumps(obj) + "\n")


def broadcast(ev):
    with lock:
        targets = list(subs)
    for q in targets:
        q.put(ev)


def instructions_name_workshop():
    raw = os.environ.get("OPENCODE_CONFIG_CONTENT")
    if not raw:
        return False
    try:
        for path in json.loads(raw).get("instructions", []):
            with open(path) as f:
                if "Workshop" in f.read():
                    return True
    except Exception:
        return False
    return False


def part(sid, mid, ptype, extra):
    p = {"id": next_id("prt"), "sessionID": sid, "messageID": mid, "type": ptype}
    p.update(extra)
    return p


def emit_part(p):
    broadcast({"type": "message.part.updated", "properties": {"part": p}})


def emit_delta(sid, mid, pid, text):
    broadcast({"type": "message.part.delta", "properties": {
        "sessionID": sid, "messageID": mid, "partID": pid, "field": "text", "delta": text}})


def ask_permission(sid, mid, call_id, kind, patterns, metadata, always):
    pid = next_id("per")
    ev = threading.Event()
    permission_events[pid] = ev
    broadcast({"type": "permission.asked", "properties": {
        "id": pid, "sessionID": sid, "permission": kind, "patterns": patterns,
        "metadata": metadata, "always": always, "tool": {"messageID": mid, "callID": call_id}}})
    ev.wait(120)
    reply = permission_replies.get(pid, "reject")
    broadcast({"type": "permission.replied", "properties": {"sessionID": sid, "requestID": pid, "reply": reply}})
    return reply


def stream_text(sid, mid, text, ptype="text"):
    p = part(sid, mid, ptype, {"text": "", "time": {"start": now_ms()}})
    emit_part(p)
    for i in range(0, len(text), 12):
        emit_delta(sid, mid, p["id"], text[i:i + 12])
        time.sleep(0.01)
    p["text"] = text
    p["time"]["end"] = now_ms()
    emit_part(p)
    return p


def now_ms():
    return int(time.time() * 1000)


def tool_part(sid, mid, tool, call_id, inp, output, title, metadata, status="completed"):
    state = {"status": status, "input": inp, "time": {"start": now_ms(), "end": now_ms()}}
    if status == "completed":
        state.update({"output": output, "metadata": metadata, "title": title})
    else:
        state.update({"error": output, "metadata": metadata})
    return part(sid, mid, "tool", {"tool": tool, "callID": call_id, "state": state})


def unified_diff(path, old, new):
    old_l = old.splitlines(keepends=True)
    new_l = new.splitlines(keepends=True)
    out = ["Index: %s\n" % path, "=" * 67 + "\n", "--- %s\n" % path, "+++ %s\n" % path,
           "@@ -%d,%d +%d,%d @@\n" % (1 if old_l else 0, len(old_l), 1 if new_l else 0, len(new_l))]
    for l in old_l:
        out.append("-" + l if l.endswith("\n") else "-" + l + "\n\\ No newline at end of file\n")
    for l in new_l:
        out.append("+" + l if l.endswith("\n") else "+" + l + "\n\\ No newline at end of file\n")
    return "".join(out)


def run_turn(sid, agent, text):
    text_l = text.lower()
    user_mid = next_id("msg")
    broadcast({"type": "message.updated", "properties": {"info": {"id": user_mid, "sessionID": sid, "role": "user", "time": {"created": now_ms()}, "agent": agent}}})
    broadcast({"type": "session.status", "properties": {"sessionID": sid, "status": {"type": "busy"}}})
    mid = next_id("msg")
    broadcast({"type": "message.updated", "properties": {"info": {"id": mid, "sessionID": sid, "role": "assistant", "time": {"created": now_ms()}, "agent": agent, "modelID": "big-pickle", "providerID": "opencode"}}})
    emit_part(part(sid, mid, "step-start", {}))
    items = []
    if "slow" in text_l:
        time.sleep(3)
    if "think" in text_l:
        thought = "The user wants a short answer. Keep it brief."
        items.append(("reasoning", thought))
        stream_text(sid, mid, thought, ptype="reasoning")
    answer = None
    if agent == "plan":
        answer = "Plan: I would create the file, but plan mode is read-only. Ready when you exit plan mode."
    elif "what are you" in text_l:
        answer = ("I'm Workshop's assistant, a coding agent running in your terminal."
                  if instructions_name_workshop() else
                  "I'm opencode, an AI coding assistant that runs in your terminal.")
    elif "create hello.txt" in text_l:
        path = os.path.join(CWD, "hello.txt")
        call_id = next_id("call")
        diff = unified_diff(path, "", "hi")
        reply = ask_permission(sid, mid, call_id, "edit", ["hello.txt"], {"filepath": path, "diff": diff}, ["*"])
        inp = {"filePath": path, "content": "hi"}
        if reply in ("once", "always"):
            with open(path, "w") as f:
                f.write("hi")
            emit_part(tool_part(sid, mid, "write", call_id, inp, "Wrote file successfully.", "hello.txt",
                                {"diagnostics": {}, "filepath": path, "exists": False, "truncated": False}))
            answer = "Created hello.txt."
        else:
            emit_part(tool_part(sid, mid, "write", call_id, inp, "The user rejected permission to use this specific tool call.", "hello.txt", {}, status="error"))
            answer = "Understood — I did not create hello.txt."
    elif "rm -rf tmp" in text_l:
        call_id = next_id("call")
        reply = ask_permission(sid, mid, call_id, "bash", ["rm -rf tmp"], {"command": "rm -rf tmp"}, ["rm *"])
        inp = {"command": "rm -rf tmp"}
        if reply in ("once", "always"):
            shutil.rmtree(os.path.join(CWD, "tmp"), ignore_errors=True)
            emit_part(tool_part(sid, mid, "bash", call_id, inp, "(no output)", "rm -rf tmp",
                                {"output": "(no output)", "exit": 0, "truncated": False}))
            answer = "Removed tmp."
        else:
            emit_part(tool_part(sid, mid, "bash", call_id, inp, "The user rejected permission to use this specific tool call.", "rm -rf tmp", {}, status="error"))
            answer = "Understood — tmp was left alone."
    elif "edit hello.txt" in text_l:
        path = os.path.join(CWD, "hello.txt")
        call_id = next_id("call")
        diff = unified_diff(path, "hi", "hello")
        try:
            with open(path, "w") as f:
                f.write("hello")
        except OSError:
            pass
        emit_part(tool_part(sid, mid, "edit", call_id, {"filePath": path, "oldString": "hi", "newString": "hello"},
                            "Edit applied successfully.", "hello.txt",
                            {"diagnostics": {}, "diff": diff, "filediff": {"file": path, "patch": diff, "additions": 1, "deletions": 1}, "truncated": False}))
        answer = "Changed hi to hello in hello.txt."
    elif "list files" in text_l or text_l.strip() == "ls":
        call_id = next_id("call")
        out = subprocess.run(["ls", "-1"], cwd=CWD, capture_output=True, text=True).stdout or "(no output)"
        emit_part(tool_part(sid, mid, "bash", call_id, {"command": "ls -1"}, out, "ls -1",
                            {"output": out, "exit": 0, "truncated": False}))
        if "think" in text_l:
            stream_text(sid, mid, "The listing is in. Summarize it.", ptype="reasoning")
        answer = "Here is the listing."
    else:
        answer = "Echo: " + text.strip()
    stream_text(sid, mid, answer)
    emit_part(part(sid, mid, "step-finish", {"reason": "stop", "cost": 0,
                                             "tokens": {"total": 8627, "input": 158, "output": 21, "reasoning": 0, "cache": {"write": 0, "read": 8448}}}))
    sessions[sid]["messages"].append({"role": "user", "text": text})
    sessions[sid]["messages"].append({"role": "assistant", "text": answer})
    broadcast({"type": "session.status", "properties": {"sessionID": sid, "status": {"type": "idle"}}})
    broadcast({"type": "session.idle", "properties": {"sessionID": sid}})


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
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
        if path.startswith("/session/") and path.count("/") == 2:
            # The real server persists sessions across restarts; any id it minted is known.
            sid = path.split("/")[2]
            if sid in sessions or sid.startswith("ses_fake"):
                sessions.setdefault(sid, {"messages": []})
                return self._json(200, {"id": sid, "title": "fake", "directory": CWD})
            return self._json(404, {"error": "not found"})
        if path.startswith("/session/") and path.endswith("/message"):
            sid = path.split("/")[2]
            return self._json(200, [{"info": {"role": m["role"]}, "parts": [{"type": "text", "text": m["text"]}]}
                                    for m in sessions.get(sid, {"messages": []})["messages"]])
        if path == "/event":
            import queue
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
        body = json.loads(self.rfile.read(n) or b"{}") if n else {}
        if path == "/session":
            sid = next_id("ses_fake")
            sessions[sid] = {"messages": []}
            return self._json(200, {"id": sid, "title": body.get("title", ""), "directory": CWD})
        if path.startswith("/session/") and path.endswith("/prompt_async"):
            sid = path.split("/")[2]
            sessions.setdefault(sid, {"messages": []})
            text = "".join(p.get("text", "") for p in body.get("parts", []))
            log({"session": sid, "agent": body.get("agent"), "text": text, "model": body.get("model")})
            threading.Thread(target=run_turn, args=(sid, body.get("agent"), text), daemon=True).start()
            self.send_response(204)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if path.startswith("/session/") and "/permissions/" in path:
            pid = path.rsplit("/", 1)[1]
            permission_replies[pid] = body.get("response", "reject")
            log({"permission": pid, "response": permission_replies[pid]})
            ev = permission_events.get(pid)
            if ev:
                ev.set()
            return self._json(200, True)
        if path.startswith("/session/") and path.endswith("/abort"):
            sid = path.split("/")[2]
            broadcast({"type": "session.error", "properties": {"sessionID": sid, "error": {"name": "MessageAbortedError", "data": {"message": "Aborted"}}}})
            broadcast({"type": "session.idle", "properties": {"sessionID": sid}})
            return self._json(200, True)
        self._json(404, {"error": "no route POST " + path})


srv = ThreadingHTTPServer(("127.0.0.1", PORT), H)
srv.daemon_threads = True
print("opencode server listening on http://127.0.0.1:%d" % PORT, flush=True)
srv.serve_forever()
