"""Credential-free raw ACP peer for the native/headless integration tests.

No SDK or network is required. All responses remain ordinary ACP JSON-RPC;
the reused request IDs deliberately exercise cancellation generations.
"""
import json
import os
from pathlib import Path
import sys

pending = {}
prompts = {}
sessions = 0


def send(message):
    sys.stdout.write(json.dumps(message, ensure_ascii=True) + "\n")
    sys.stdout.flush()


def result(identity, value):
    send({"jsonrpc": "2.0", "id": identity, "result": value})


def update(session, text):
    send({"jsonrpc": "2.0", "method": "session/update", "params": {
        "sessionId": session, "update": {"sessionUpdate": "agent_message_chunk",
        "content": {"type": "text", "text": text}}}})


def finish(session, reason="end_turn"):
    identity = prompts.pop(session, None)
    if identity is not None:
        result(identity, {"stopReason": reason})


def request(identity, method, params, callback):
    pending[identity] = callback
    send({"jsonrpc": "2.0", "id": identity, "method": method, "params": params})


def permission(session, callback):
    request(41, "session/request_permission", {
        "sessionId": session,
        "toolCall": {"toolCallId": "change", "title": "Write file", "kind": "edit",
                     "locations": [{"path": str(Path.cwd() / "agent.txt")}]},
        "options": [{"optionId": "allow", "name": "Allow once", "kind": "allow_once"},
                    {"optionId": "deny", "name": "Reject", "kind": "reject_once"}]}, callback)


def prompt(identity, params):
    session = params["sessionId"]
    prompts[session] = identity
    text = "".join(block.get("text", "") for block in params["prompt"])
    if text.startswith("inspect-context"):
        update(session, json.dumps(params["prompt"], ensure_ascii=False, sort_keys=True))
        finish(session)
    elif text == "hold":
        update(session, "waiting")
    elif text == "permission":
        def resolved(message):
            if session in prompts:
                update(session, json.dumps(message.get("result", {}), sort_keys=True))
                finish(session)
        permission(session, resolved)
    elif text in ("read", "wait-read"):
        def read(message):
            if session in prompts:
                update(session, json.dumps(message.get("result", message.get("error")), ensure_ascii=True, sort_keys=True))
                finish(session)
        request(42, "fs/read_text_file", {"sessionId": session,
                "path": str(Path.cwd() / "agent.txt"), "line": 2, "limit": 1}, read)
    elif text == "write":
        def wrote(message):
            if session in prompts:
                update(session, "write-ok" if "result" in message else "write-denied")
                finish(session)
        request(42, "fs/write_text_file", {"sessionId": session,
                "path": str(Path.cwd() / "agent.txt"), "content": "node edit λ\nsecond\n"}, wrote)
    elif text == "write-burst":
        remaining = [64]
        def rejected(message):
            if "error" not in message:
                update(session, "unexpected-write")
            remaining[0] -= 1
            if not remaining[0]:
                update(session, "all-64-replies-received")
                finish(session)
        for index in range(64):
            request(1000 + index, "fs/write_text_file", {"sessionId": session,
                    "path": str(Path.cwd() / "agent.txt"), "content": "must not overwrite"}, rejected)
    elif text == "terminal":
        def created(message):
            terminal = message["result"]["terminalId"]
            def waited(message):
                def released(message):
                    update(session, "terminal-finished")
                    finish(session)
                request(53, "terminal/release", {"sessionId": session, "terminalId": terminal}, released)
            request(51, "terminal/wait_for_exit", {"sessionId": session, "terminalId": terminal}, waited)
        request(50, "terminal/create", {"sessionId": session, "command": sys.executable,
                "args": ["-X", "utf8", "-u", "-c", "print('\\n'.join('history-%04d λ' % i for i in range(400)))"],
                "outputByteLimit": 256}, created)
    elif text == "burst":
        for index in range(400):
            update(session, f"{index:04}:" + "x" * 128)
        finish(session)
    elif text == "large-event":
        update(session, "large:" + "λ" * 600_000)
        finish(session)
    else:
        update(session, f"echo:{len(text)}:{text[:32]}")
        finish(session)


for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    identity = message.get("id")
    params = message.get("params", {})
    if not method:
        callback = pending.pop(identity, None)
        if callback:
            callback(message)
    elif method == "initialize":
        prompt_capabilities = {"embeddedContext": True} if os.environ.get("VTCODE_TEST_EMBEDDED_CONTEXT") else {}
        result(identity, {"protocolVersion": 1, "agentCapabilities": {"loadSession": True,
            "promptCapabilities": prompt_capabilities,
            "sessionCapabilities": {"list": {}, "resume": {}, "close": {}, "delete": {}}},
            "agentInfo": {"name": "headless-test-peer", "version": "1"}})
    elif method == "session/new":
        sessions += 1
        result(identity, {"sessionId": f"session-{sessions}"})
    elif method in ("session/load", "session/resume", "session/close", "session/delete", "authenticate"):
        result(identity, {})
    elif method == "session/list":
        result(identity, {"sessions": [{"sessionId": "stored", "cwd": str(Path.cwd()), "title": "Stored"}]})
    elif method == "session/prompt":
        prompt(identity, params)
    elif method == "session/cancel":
        session = params["sessionId"]
        # A late cancelled host response is harmless to a later reused ID.
        pending.clear()
        finish(session, "cancelled")
    elif identity is not None:
        send({"jsonrpc": "2.0", "id": identity, "error": {"code": -32601, "message": method}})
