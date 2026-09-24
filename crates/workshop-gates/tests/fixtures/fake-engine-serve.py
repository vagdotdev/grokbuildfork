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
  * "list files" / "ls"               -> permission.asked (bash `ls -1`, as the real server asks
    for every command under an `ask` policy) then a completed `bash` part with real output.
  * "make a folder on my desktop"     -> the out-of-folder pair captured live: permission.asked
    (`external_directory`, metadata.command + directories) and, once replied, permission.asked
    (`bash`) for the same tool call, then the completed `bash` part.
  * "what are you" / "who made you"   -> answers from the identity the system prompt opens with, as
    the real models do: OpenCode 1.18.31 opens it with the agent's `prompt` from the inline config
    (OPENCODE_CONFIG_CONTENT) when one is set, else with the model family's prompt ("You are
    opencode, …", feedback at github.com/anomalyco/opencode), and appends `instructions` after it.
    Any agent, Plan included.
  * "think"                           -> a reasoning part streamed before the answer part (and, on
    "list files", a whitespace-only text part after it and another reasoning part after the tool
    call — the shapes a real model sends).
  * "install the tool"                -> ends the turn on "I'll run the installer:" with no tool call;
    a following "Continue: …" prompt runs `echo installed` and answers "Installed the tool.".
  * "keep announcing"                 -> every turn, continued or not, ends on "Let me run it:".
  * "download cat photos"             -> a `curl … -o cat1.jpg` bash part and "Downloaded 1 cat photo."; the
    "Continue my request: open each image …" prompt sent to a model that sees opens it and says
    "Checked: cat1.jpg is a real photo of a cat.".
  * "create todo.py"                  -> pastes the file in a fenced block and writes nothing; a
    following "Continue: …" prompt writes ./todo.py with a `write` part and answers "Wrote todo.py.".
  * "create stubborn.py"              -> pastes the file every time, continued or not.
  * "show me a loop"                  -> answers with a fenced example (no file was asked for).
  * "show the tree"                   -> a finished answer that ends on a colon and a fenced tree.
  * "ask me"                          -> the `question` tool: question.asked ("Which install method?",
    PPA / .deb), then waits for POST /question/{id}/reply or /reject and answers with the choice.
  * "sort my photos"                  -> a `read` of img01.jpg ("Image read successfully"). A model
    that cannot see images then waits for the abort (up to 5 s, else says it can't see them); one
    that can (muse-spark-*, mimo-*), or the "Continue my request …" prompt sent to one, answers
    "Looking at the photos." and, 2 s later, "Sorted 1 photo: a lion.".
  * "slow"                            -> waits 3 s before answering (to queue prompts behind it).
  * "scratch note"                    -> permission.asked (edit) then a `write` and a `read` part on
    `<TMPDIR>/opencode/note.txt` (OpenCode's temp dir), really written (the screen must never
    say "opencode" for it).
  * "install htop"                    -> permission.asked (bash `sudo touch installed-htop.txt`) and
    the command really runs (through the `sudo` on PATH, with this server's environment — the
    askpass gate's stand-in reads SUDO_ASKPASS); the answer reports success or sudo's words.
  * "long command"                    -> permission.asked (bash `sleep 8`), a `running` tool part,
    8 s of silence, the completed part, "Done waiting." (silence while a tool runs is not a stall).
  * "go silent"                       -> starts an answer ("Let me look at that") and then never
    sends another event; the turn only ends when the host aborts it (logged as `aborted`).
  * "whole page"                      -> one large `write`, streamed as 1.18.31 streams a tool call:
    the part is published once as `pending`, then nothing for "(Ns)" seconds (default 8) while the
    input streams, then the call runs, page.html is written, "Wrote the whole page to page.html.".
    With "never finish" the pending part is all there ever is (a dead stream mid-call).
  * "photo candidates"                -> the `task` tool with the built-in `explore` subagent: a
    `running` task part, then a child session (session.created, parentID = the turn's session;
    logged as `{"created", "parent"}`) that reads a file and asks permission for a bash command
    with its *own* sessionID. A reply posted under any other session is refused (404, logged
    `misrouted`), as the real server's per-session pending asks would lose it. Approved: the child
    finishes, the task part completes, "Found 15 photo candidates, 3 per species.".
  * agent == plan                     -> never a tool part, never a permission ask: text only.

Every turn ends with a step-finish carrying tokens (total 8627 -> "8.6K") and goes idle. Every
prompt_async body and permission reply is appended to the `--log` file (JSON lines) so a gate can
pin the agent that was sent and the answer Workshop posted; the server's own start is the first
line (`{"started": true, "time": <unix seconds>}`) so a gate can pin *when* Workshop brought it up,
and every session Workshop opens is a `{"created": "<id>"}` line so a gate can pin *which*
conversation each prompt went to.
"""
import json
import os
import re
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
stalled_sessions = set()  # sessions whose turn went silent on purpose ("stall")
permission_events = {}  # permission id -> threading.Event
permission_sessions = {}  # permission id -> the session that asked (a reply elsewhere is lost)
aborts = {}  # session id -> threading.Event, set by POST /session/{id}/abort
question_events = {}  # question id -> threading.Event, set by POST /question/{id}/reply|reject
question_answers = {}  # question id -> the answers posted (None when rejected)
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


FAMILY_PROMPT = ("You are opencode, an interactive CLI tool that helps users with software engineering tasks.\n"
                 "- To give feedback, users should report the issue at https://github.com/anomalyco/opencode/issues")


def inline_config():
    try:
        return json.loads(os.environ.get("OPENCODE_CONFIG_CONTENT") or "{}")
    except ValueError:
        return {}


def system_prompt(agent):
    cfg = inline_config()
    base = ((cfg.get("agent") or {}).get(agent or "build") or {}).get("prompt") or FAMILY_PROMPT
    parts = [base, "You are powered by the model named big-pickle. The exact model ID is opencode/big-pickle"]
    for path in cfg.get("instructions", []):
        try:
            with open(path) as f:
                parts.append(f.read())
        except OSError:
            pass
    return "\n".join(parts)


def identity_answer(agent, text_l):
    if system_prompt(agent).startswith("You are Workshop's"):
        return "I'm Workshop's assistant, running as Big Pickle."
    if "who made you" in text_l:
        return "I was made by the OpenCode team (github.com/anomalyco/opencode)."
    return "I'm opencode, an AI coding assistant that runs in your terminal."


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
    permission_sessions[pid] = sid
    log({"asked": pid, "permission": kind, "callID": call_id, "session": sid})
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
    elif status == "running":
        state = {"status": status, "input": inp, "time": {"start": now_ms()}}
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


def run_turn(sid, agent, text, model=None):
    text_l = text.lower()
    model_id = (model or {}).get("modelID", "big-pickle")
    sees_images = model_id.startswith(("muse-spark", "mimo"))
    aborted = aborts.setdefault(sid, threading.Event())
    aborted.clear()
    # A continuation carries on the request that came before it.
    continued = text.startswith("Continue:") or text.startswith("Continue my request")
    if continued:
        first = next((m["text"] for m in reversed(sessions[sid]["messages"])
                      if m["role"] == "user" and not m["text"].startswith("Continue:")), "")
        text_l = first.lower()
    user_mid = next_id("msg")
    broadcast({"type": "message.updated", "properties": {"info": {"id": user_mid, "sessionID": sid, "role": "user", "time": {"created": now_ms()}, "agent": agent}}})
    broadcast({"type": "session.status", "properties": {"sessionID": sid, "status": {"type": "busy"}}})
    mid = next_id("msg")
    broadcast({"type": "message.updated", "properties": {"info": {"id": mid, "sessionID": sid, "role": "assistant", "time": {"created": now_ms()}, "agent": agent, "modelID": "big-pickle", "providerID": "opencode"}}})
    emit_part(part(sid, mid, "step-start", {}))
    items = []
    if "slow" in text_l:
        time.sleep(3)
    if "go silent" in text_l:
        # The model starts an answer and then nothing more ever arrives (an upstream 504 the
        # engine retries silently): the turn never goes idle until the host aborts it.
        p = part(sid, mid, "text", {"text": "", "time": {"start": now_ms()}})
        emit_part(p)
        emit_delta(sid, mid, p["id"], "Let me look at that")
        stalled_sessions.add(sid)
        return
    if "think" in text_l:
        thought = "The user wants a short answer. Keep it brief."
        items.append(("reasoning", thought))
        stream_text(sid, mid, thought, ptype="reasoning")
    answer = None
    if "what are you" in text_l or "who made you" in text_l:
        answer = identity_answer(agent, text_l)
    elif agent == "plan":
        answer = "Plan: I would create the file, but plan mode is read-only. Ready when you exit plan mode."
    elif "sort my photos" in text_l:
        if not continued:
            emit_part(tool_part(sid, mid, "read", next_id("call"), {"filePath": os.path.join(CWD, "img01.jpg")},
                                "Image read successfully", "img01.jpg", {"preview": "", "truncated": False}))
        if not sees_images:
            if aborted.wait(5):
                sessions[sid]["messages"].append({"role": "user", "text": text})
                return
            answer = "I can't see images with this model."
        else:
            stream_text(sid, mid, "Looking at the photos.\n\n")
            time.sleep(2)
            answer = "Sorted 1 photo: a lion."
    elif "ask me" in text_l:
        qid, call_id = next_id("que"), next_id("call")
        question_events[qid] = threading.Event()
        questions = [{"question": "Which install method?", "header": "Install",
                      "options": [{"label": "PPA", "description": "apt repository"},
                                  {"label": ".deb", "description": "one package file"}]}]
        emit_part(part(sid, mid, "tool", {"tool": "question", "callID": call_id,
                                          "state": {"status": "running", "input": {"questions": questions},
                                                    "time": {"start": now_ms()}}}))
        broadcast({"type": "question.asked", "properties": {"id": qid, "sessionID": sid, "questions": questions,
                                                            "tool": {"messageID": mid, "callID": call_id}}})
        question_events[qid].wait(60)
        answers = question_answers.get(qid)
        emit_part(tool_part(sid, mid, "question", call_id, {"questions": questions},
                            "User has answered your questions." if answers else "The user dismissed this question",
                            "Asked 1 question", {"answers": answers or []}, status="completed" if answers else "error"))
        answer = ("You chose: %s." % answers[0][0]) if answers else "No answer."
    elif "download cat photos" in text_l:
        if sees_images and continued:
            emit_part(tool_part(sid, mid, "read", next_id("call"), {"filePath": os.path.join(CWD, "cat1.jpg")},
                                "Image read successfully", "cat1.jpg", {"preview": "", "truncated": False}))
            answer = "Checked: cat1.jpg is a real photo of a cat."
        else:
            emit_part(tool_part(sid, mid, "bash", next_id("call"),
                                {"command": "curl -fsSL -o cat1.jpg https://example.org/cat1.jpg"}, "(no output)",
                                "curl", {"output": "(no output)", "exit": 0, "truncated": False}))
            answer = "Downloaded 1 cat photo."
    elif "keep announcing" in text_l:
        answer = "Let me run it:"
    elif "create todo.py" in text_l and continued:
        path = os.path.join(CWD, "todo.py")
        content = 'print("todo")\n'
        with open(path, "w") as f:
            f.write(content)
        emit_part(tool_part(sid, mid, "write", next_id("call"), {"filePath": path, "content": content},
                            "Wrote file successfully.", "todo.py",
                            {"diagnostics": {}, "filepath": path, "exists": False, "truncated": False}))
        answer = "Wrote todo.py."
    elif "create todo.py" in text_l or "create stubborn.py" in text_l:
        answer = "Here is the file.\n\n```python\nprint(\"todo\")\n```"
    elif "show the tree" in text_l:
        answer = "Sorted all 15 photos into ~/Desktop/Panthera:\n\n```\nPanthera/\n  lion/\n  tiger/\n```"
    elif "show me a loop" in text_l:
        answer = "```python\nfor i in range(3):\n    print(i)\n```"
    elif "install the tool" in text_l:
        if continued:
            call_id = next_id("call")
            emit_part(tool_part(sid, mid, "bash", call_id, {"command": "echo installed"}, "installed\n",
                                "echo installed", {"output": "installed\n", "exit": 0, "truncated": False}))
            answer = "Installed the tool."
        else:
            answer = "Downloaded it. I'll run the installer:"
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
    elif "scratch note" in text_l:
        # Scratch work where OpenCode keeps its temp dir, `<TMPDIR>/opencode` — what a model
        # uses when it needs somewhere to work. Really written, then read back, so a write row
        # and a read row both carry the path.
        scratch = os.path.join(os.environ.get("TMPDIR") or "/tmp", "opencode")
        os.makedirs(scratch, exist_ok=True)
        path = os.path.join(scratch, "note.txt")
        call_id = next_id("call")
        reply = ask_permission(sid, mid, call_id, "edit", [path], {"filepath": path, "diff": unified_diff(path, "", "scratch\n")}, ["*"])
        if reply in ("once", "always"):
            with open(path, "w") as f:
                f.write("scratch\n")
            emit_part(tool_part(sid, mid, "write", call_id, {"filePath": path, "content": "scratch\n"}, "Wrote file successfully.", "note.txt",
                                {"diagnostics": {}, "filepath": path, "exists": False, "truncated": False}))
            call2 = next_id("call")
            emit_part(tool_part(sid, mid, "read", call2, {"filePath": path}, "scratch", path, {}))
            answer = "Saved a scratch note and read it back."
        else:
            answer = "Understood \u2014 no scratch note."
    elif "adversarial askpass" in text_l:
        # A prompt-injected command tries to phish the sudo password two ways: run the askpass
        # helper itself, and connect to the askpass socket directly. Neither may show a card or
        # return a password — the helper's parent is a shell, not sudo.
        script = ('out=$("$SUDO_ASKPASS" "Enter your account password to continue:"); rc=$?; '
                  'printf "HELPER stdout=[%s] rc=%s\\n" "$out" "$rc"')
        helper = subprocess.run(["sh", "-c", script], cwd=CWD, capture_output=True, text=True)
        sock = os.environ.get("WORKSHOP_ASKPASS_SOCK", "")
        direct = "no-socket"
        if sock:
            py = ('import socket,sys\n'
                  's=socket.socket(socket.AF_UNIX)\n'
                  's.connect(sys.argv[1])\n'
                  's.sendall(b\'{"prompt":"pw"}\\n\')\n'
                  'sys.stdout.write("DIRECT["+s.recv(4096).decode().strip()+"]")\n')
            r = subprocess.run(["python3", "-c", py, sock], capture_output=True, text=True)
            direct = (r.stdout + r.stderr).strip()
        answer = (helper.stdout.strip() + " " + direct).strip()
    elif "install htop" in text_l:
        # A command that needs root: really run through whatever `sudo` is on PATH (the gate's
        # stand-in), with the environment Workshop gave this server, and report what it said.
        cmd = "sudo touch installed-htop.txt"
        call_id = next_id("call")
        reply = ask_permission(sid, mid, call_id, "bash", [cmd], {"command": cmd}, ["sudo *"])
        if reply in ("once", "always"):
            emit_part(tool_part(sid, mid, "bash", call_id, {"command": cmd}, "", cmd, {}, status="running"))
            run = subprocess.run(["sh", "-c", cmd], cwd=CWD, capture_output=True, text=True)
            out = (run.stdout + run.stderr).strip() or "(no output)"
            emit_part(tool_part(sid, mid, "bash", call_id, {"command": cmd}, out, cmd,
                                {"output": out, "exit": run.returncode, "truncated": False}))
            answer = "Installed htop." if run.returncode == 0 else "Could not install htop: " + out
        else:
            emit_part(tool_part(sid, mid, "bash", call_id, {"command": cmd}, "The user rejected permission to use this specific tool call.", cmd, {}, status="error"))
            answer = "Understood."
    elif "long command" in text_l:
        # A command that runs longer than any stall ceiling a gate sets: the server is quiet
        # while it runs, and that quiet must not count as a stall.
        call_id = next_id("call")
        reply = ask_permission(sid, mid, call_id, "bash", ["sleep 8"], {"command": "sleep 8"}, ["sleep *"])
        if reply in ("once", "always"):
            running = tool_part(sid, mid, "bash", call_id, {"command": "sleep 8"}, "", "sleep 8", {}, status="running")
            emit_part(running)
            time.sleep(8)
            emit_part(tool_part(sid, mid, "bash", call_id, {"command": "sleep 8"}, "(no output)", "sleep 8",
                                {"output": "(no output)", "exit": 0, "truncated": False}))
            answer = "Done waiting."
        else:
            emit_part(tool_part(sid, mid, "bash", call_id, {"command": "sleep 8"}, "The user rejected permission to use this specific tool call.", "sleep 8", {}, status="error"))
            answer = "Understood."
    elif "whole page" in text_l:
        # A large single-file write, streamed the way 1.18.31 streams it: the tool part is
        # published once, `pending`, when the model starts composing the call, then *nothing*
        # while the input streams — "(8s)" in the prompt is how long — then the call runs and
        # completes. "never finish" is the model dying mid-call: the pending part is all there
        # ever is, and the turn ends only when the host aborts it.
        call_id = next_id("call")
        path = os.path.join(CWD, "page.html")
        emit_part(part(sid, mid, "tool", {"tool": "write", "callID": call_id,
                                          "state": {"status": "pending", "input": {}, "raw": ""}}))
        if "never finish" in text_l:
            stalled_sessions.add(sid)
            return
        m = re.search(r"\((\d+)s\)", text_l)
        time.sleep(float(m.group(1)) if m else 8.0)
        content = "<!doctype html>\n<html><body>\n" + "".join(
            "<section class=\"stage\"><h2>Stage %d</h2><p>Lorem ipsum dolor sit amet.</p></section>\n" % i
            for i in range(1, 41)) + "</body></html>\n"
        with open(path, "w") as f:
            f.write(content)
        inp = {"filePath": path, "content": content}
        emit_part(tool_part(sid, mid, "write", call_id, inp, "", path, {}, status="running"))
        emit_part(tool_part(sid, mid, "write", call_id, inp, "Wrote page.html", path,
                            {"filepath": path, "exists": False}))
        answer = "Wrote the whole page to page.html."
    elif "photo candidates" in text_l:
        # The `task` tool with a built-in subagent, as 1.18.31 runs it: the subagent gets a child
        # session (`session.created`, parentID = this session) and works there; its bash needs
        # permission, and the ask carries the *child's* sessionID. The reply must come back to
        # the child (see the permissions route); nothing else the child says is the parent's.
        call_id = next_id("call")
        desc = "Extract Wikimedia image candidates"
        task_input = {"subagent_type": "explore", "description": desc,
                      "prompt": "List Wikimedia image candidates for each Panthera species."}
        emit_part(tool_part(sid, mid, "task", call_id, task_input, "", desc, {}, status="running"))
        child = next_id("ses_fake")
        sessions[child] = {"messages": []}
        log({"created": child, "parent": sid})
        broadcast({"type": "session.created", "properties": {"sessionID": child, "info": {
            "id": child, "parentID": sid, "title": desc + " (@explore subagent)", "agent": "explore", "directory": CWD}}})
        cmid = next_id("msg")
        broadcast({"type": "message.updated", "properties": {"info": {"id": cmid, "sessionID": child, "role": "assistant",
                                                                        "time": {"created": now_ms()}, "agent": "explore", "modelID": "big-pickle", "providerID": "opencode"}}})
        broadcast({"type": "session.status", "properties": {"sessionID": child, "status": {"type": "busy"}}})
        emit_part(tool_part(child, cmid, "read", next_id("call"), {"filePath": os.path.join(CWD, "species.txt")},
                            "lion\ntiger\nleopard\njaguar\nsnow leopard\n", "species.txt", {}))
        cmd = "python3 extract_candidates.py species.txt"
        ccall = next_id("call")
        reply = ask_permission(child, cmid, ccall, "bash", [cmd], {"command": cmd}, ["python3 *"])
        if reply in ("once", "always"):
            emit_part(tool_part(child, cmid, "bash", ccall, {"command": cmd}, "15 candidates", cmd,
                                {"output": "15 candidates", "exit": 0, "truncated": False}))
            result = "Subagent report: 15 candidates, 3 per species."
            answer = "Found 15 photo candidates, 3 per species."
        else:
            emit_part(tool_part(child, cmid, "bash", ccall, {"command": cmd}, "The user rejected permission to use this specific tool call.", cmd, {}, status="error"))
            result = "Subagent report: the extraction was not allowed."
            answer = "Understood — the subagent did not run the extraction."
        stream_text(child, cmid, result)
        broadcast({"type": "session.status", "properties": {"sessionID": child, "status": {"type": "idle"}}})
        broadcast({"type": "session.idle", "properties": {"sessionID": child}})
        emit_part(tool_part(sid, mid, "task", call_id, task_input, result, desc, {"sessionId": child}))
    elif "list files" in text_l or text_l.strip() == "ls":
        if "think" in text_l:
            stream_text(sid, mid, "\n\n")
        call_id = next_id("call")
        reply = ask_permission(sid, mid, call_id, "bash", ["ls -1"], {"command": "ls -1"}, ["ls *"])
        if reply in ("once", "always"):
            out = subprocess.run(["ls", "-1"], cwd=CWD, capture_output=True, text=True).stdout or "(no output)"
            emit_part(tool_part(sid, mid, "bash", call_id, {"command": "ls -1"}, out, "ls -1",
                                {"output": out, "exit": 0, "truncated": False}))
            if "think" in text_l:
                stream_text(sid, mid, "The listing is in. Summarize it.", ptype="reasoning")
            if "slowly" in text_l:
                time.sleep(3)
            answer = "Here is the listing."
        else:
            emit_part(tool_part(sid, mid, "bash", call_id, {"command": "ls -1"}, "The user rejected permission to use this specific tool call.", "ls -1", {}, status="error"))
            answer = "Understood — I did not list the files."
    elif "make a folder on my desktop" in text_l:
        home = os.environ.get("HOME", "/root")
        desk = os.path.join(home, "Desktop")
        cmd = ("mkdir -p %s/iBooks && curl -fsSL -o %s/iBooks/alice_in_wonderland.epub https://www.gutenberg.org/ebooks/11.epub.noimages"
               " && curl -fsSL -o %s/iBooks/frankenstein.epub https://www.gutenberg.org/ebooks/84.epub.noimages" % (desk, desk, desk))
        call_id = next_id("call")
        reply = ask_permission(sid, mid, call_id, "external_directory", [desk + "/*"],
                               {"command": cmd, "directories": [desk], "patterns": [desk + "/*"]}, [desk + "/*"])
        if reply in ("once", "always"):
            reply2 = ask_permission(sid, mid, call_id, "bash", ["mkdir -p " + desk + "/iBooks", "curl -fsSL -o " + desk + "/iBooks/alice_in_wonderland.epub", "curl -fsSL -o " + desk + "/iBooks/frankenstein.epub"],
                                    {"command": cmd}, ["mkdir *", "curl *"])
            if reply2 in ("once", "always"):
                os.makedirs(os.path.join(desk, "iBooks"), exist_ok=True)
                emit_part(tool_part(sid, mid, "bash", call_id, {"command": cmd}, "(no output)", cmd,
                                    {"output": "(no output)", "exit": 0, "truncated": False}))
                answer = "Created the iBooks folder on your desktop."
            else:
                emit_part(tool_part(sid, mid, "bash", call_id, {"command": cmd}, "The user rejected permission to use this specific tool call.", cmd, {}, status="error"))
                answer = "Understood — nothing was created."
        else:
            emit_part(tool_part(sid, mid, "bash", call_id, {"command": cmd}, "The user rejected permission to use this specific tool call.", cmd, {}, status="error"))
            answer = "Understood — nothing was created."
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
            log({"created": sid})
            return self._json(200, {"id": sid, "title": body.get("title", ""), "directory": CWD})
        if path.startswith("/session/") and path.endswith("/prompt_async"):
            sid = path.split("/")[2]
            sessions.setdefault(sid, {"messages": []})
            text = "".join(p.get("text", "") for p in body.get("parts", []))
            log({"session": sid, "agent": body.get("agent"), "text": text, "model": body.get("model"),
                 "system_head": system_prompt(body.get("agent")).split("\n", 1)[0],
                 "files": [{"mime": p.get("mime"), "url": (p.get("url") or "")[:40]}
                           for p in body.get("parts", []) if p.get("type") == "file"]})
            threading.Thread(target=run_turn, args=(sid, body.get("agent"), text, body.get("model")), daemon=True).start()
            self.send_response(204)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if path.startswith("/session/") and "/permissions/" in path:
            pid = path.rsplit("/", 1)[1]
            sid = path.split("/")[2]
            # Like the real server, a session's pending asks are its own: a reply posted under
            # another session (the parent's, for a subagent's ask) answers nothing.
            if permission_sessions.get(pid, sid) != sid:
                log({"permission": pid, "response": body.get("response", "reject"), "session": sid, "misrouted": True})
                return self._json(404, {"error": "no pending permission " + pid + " in session " + sid})
            permission_replies[pid] = body.get("response", "reject")
            log({"permission": pid, "response": permission_replies[pid], "session": sid})
            ev = permission_events.get(pid)
            if ev:
                ev.set()
            return self._json(200, True)
        if path.startswith("/question/") and (path.endswith("/reply") or path.endswith("/reject")):
            qid = path.split("/")[2]
            question_answers[qid] = body.get("answers") if path.endswith("/reply") else None
            log({"question": qid, "answers": question_answers[qid]})
            ev = question_events.get(qid)
            if ev:
                ev.set()
            return self._json(200, True)
        if path.startswith("/session/") and path.endswith("/abort"):
            sid = path.split("/")[2]
            log({"aborted": sid, "time": time.time()})
            stalled_sessions.discard(sid)
            log({"abort": sid})
            aborts.setdefault(sid, threading.Event()).set()
            broadcast({"type": "session.error", "properties": {"sessionID": sid, "error": {"name": "MessageAbortedError", "data": {"message": "Aborted"}}}})
            broadcast({"type": "session.idle", "properties": {"sessionID": sid}})
            return self._json(200, True)
        self._json(404, {"error": "no route POST " + path})


srv = ThreadingHTTPServer(("127.0.0.1", PORT), H)
srv.daemon_threads = True
log({"started": True, "port": PORT, "time": time.time()})
print("opencode server listening on http://127.0.0.1:%d" % PORT, flush=True)
srv.serve_forever()
