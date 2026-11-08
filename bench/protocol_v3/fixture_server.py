"""Pinned local MCP fixture server; real disposable file/git/JSON-memory state.

Not a reference MCP package or a live service. Never accesses network/accounts.
"""

import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import time
from .core import canonical

IDENTITY = {
    "GIT_AUTHOR_NAME": "Atharv Raotole",
    "GIT_AUTHOR_EMAIL": "100996694+AtharvRaotole@users.noreply.github.com",
    "GIT_COMMITTER_NAME": "Atharv Raotole",
    "GIT_COMMITTER_EMAIL": "100996694+AtharvRaotole@users.noreply.github.com",
    "GIT_AUTHOR_DATE": "2026-01-01T00:00:00-05:00",
    "GIT_COMMITTER_DATE": "2026-01-01T00:00:00-05:00",
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_CONFIG_GLOBAL": os.devnull,
}


def contracts():
    tools = []
    for name, effect, props, result in [
        ("fs_read", "read", {"path": {"type": "string"}}, {"type": "string"}),
        (
            "fs_write",
            "irreversible",
            {"path": {"type": "string"}, "text": {"type": "string"}},
            {"type": "null"},
        ),
        ("git_head", "read", {}, {"type": "string"}),
        (
            "git_commit",
            "irreversible",
            {"path": {"type": "string"}, "text": {"type": "string"}},
            {"type": "string"},
        ),
        (
            "memory_read",
            "read",
            {"path": {"type": "string"}},
            {"type": "array", "items": {"type": "string"}},
        ),
        (
            "memory_put",
            "irreversible",
            {"path": {"type": "string"}, "text": {"type": "string"}},
            {"type": "null"},
        ),
    ]:
        tools.append(
            {
                "name": name,
                "description": name + " over disposable fixture state.",
                "effect": effect,
                "inputSchema": {
                    "type": "object",
                    "properties": props,
                    "required": list(props),
                    "additionalProperties": False,
                },
                "outputSchema": {
                    "type": "object",
                    "properties": {"value": result},
                    "required": ["value"],
                    "additionalProperties": False,
                },
            }
        )
    return tools


def git(root, *args):
    return subprocess.check_output(
        ["git", "-C", str(root), *args],
        env={**os.environ, **IDENTITY},
        stderr=subprocess.DEVNULL,
        text=True,
    ).strip()


def append_event(path, event):
    with path.open("a") as stream:
        fcntl.flock(stream, fcntl.LOCK_EX)
        stream.write(canonical(event) + "\n")
        stream.flush()
        os.fsync(stream.fileno())


@contextmanager
def capacity(state, width):
    streams = []
    owned = None
    try:
        for i in range(width):
            streams.append((state / f"slot-{i}").open("a"))
        while owned is None:
            for stream in streams:
                try:
                    fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    owned = stream
                    break
                except BlockingIOError:
                    pass
            if owned is None:
                time.sleep(0.002)
        yield
    finally:
        for stream in streams:
            stream.close()


def serve(args):
    root = Path(args.root)
    state = Path(args.state)
    log = state / "events.jsonl"
    output = threading.Lock()
    writes = threading.Lock()
    tools = {
        t["name"].split("_", 1)[1]: t
        for t in contracts()
        if t["name"].startswith(args.kind + "_")
    }

    def send(value):
        with output:
            print(canonical(value), flush=True)

    def path(value):
        if (
            not isinstance(value, str)
            or Path(value).is_absolute()
            or ".." in Path(value).parts
            or ".git" in Path(value).parts
        ):
            raise ValueError("path rejected")
        result = root / value
        if not result.resolve().is_relative_to(root.resolve()):
            raise ValueError("path rejected")
        return result

    def work(req):
        name = req["params"]["name"]
        values = req["params"].get("arguments", {})
        tool = tools.get(name)
        full = args.kind + "_" + name
        event = {
            "request_id": req["id"],
            "name": full,
            "arguments": values,
            "effect": tool["effect"] if tool else "unknown",
        }
        with capacity(state, args.width):
            with (state / "call-count").open("a+") as counter:
                fcntl.flock(counter, fcntl.LOCK_EX)
                counter.seek(0)
                count = int(counter.read() or "0")
                if count >= 64:
                    append_event(log, {**event, "event": "budget_refused"})
                    send(
                        {
                            "jsonrpc": "2.0",
                            "id": req["id"],
                            "result": {
                                "content": [
                                    {"type": "text", "text": "call budget exceeded"}
                                ],
                                "isError": True,
                            },
                        }
                    )
                    return
                counter.seek(0)
                counter.truncate()
                counter.write(str(count + 1))
                counter.flush()
            append_event(log, {**event, "event": "begin"})
            if tool and tool["effect"] == "irreversible":
                append_event(log, {**event, "event": "write_attempt"})
            try:
                if (
                    not tool
                    or set(values) != set(tool["inputSchema"]["properties"])
                    or any(not isinstance(v, str) for v in values.values())
                ):
                    raise ValueError("schema mismatch")
                if args.delay:
                    time.sleep(args.delay)
                if full == "fs_read":
                    result = path(values["path"]).read_text()
                elif full == "fs_write":
                    target = path(values["path"])
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_text(values["text"])
                    result = None
                elif full == "git_head":
                    result = git(root, "log", "-1", "--format=%s")
                elif full == "git_commit":
                    path(values["path"])
                    if "\n" in values["text"]:
                        raise ValueError("single-line message required")
                    git(root, "add", "--", values["path"])
                    git(
                        root,
                        "-c",
                        "commit.gpgsign=false",
                        "commit",
                        "--no-gpg-sign",
                        "-q",
                        "-m",
                        values["text"],
                    )
                    result = git(root, "log", "-1", "--format=%s")
                else:
                    with writes:
                        target = state / "memory.json"
                        memory = json.loads(target.read_text())
                        if full == "memory_read":
                            result = memory.get(values["path"], [])
                        else:
                            memory.setdefault(values["path"], []).append(values["text"])
                            target.write_text(canonical(memory))
                            result = None
                envelope = {"value": result}
                append_event(
                    log, {**event, "event": "end", "result": envelope, "error": None}
                )
                send(
                    {
                        "jsonrpc": "2.0",
                        "id": req["id"],
                        "result": {
                            "content": [{"type": "text", "text": canonical(envelope)}],
                            "structuredContent": envelope,
                            "isError": False,
                        },
                    }
                )
            except Exception:
                error = {"kind": "ToolError", "message": "tool request failed"}
                append_event(
                    log, {**event, "event": "end", "result": None, "error": error}
                )
                send(
                    {
                        "jsonrpc": "2.0",
                        "id": req["id"],
                        "result": {
                            "content": [{"type": "text", "text": canonical(error)}],
                            "isError": True,
                        },
                    }
                )

    with ThreadPoolExecutor(max_workers=16) as pool:
        for line in sys.stdin:
            req = json.loads(line)
            method = req.get("method")
            id = req.get("id")
            if id is None:
                continue
            if method == "tools/call":
                pool.submit(work, req)
                continue
            if method == "initialize":
                result = {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "serverInfo": {
                        "name": "weave-local-" + args.kind,
                        "version": "3.2",
                    },
                }
            elif method == "tools/list":
                result = {
                    "tools": [
                        {
                            "name": name,
                            "description": t["description"],
                            "inputSchema": t["inputSchema"],
                            "outputSchema": t["outputSchema"],
                            "annotations": {"readOnlyHint": t["effect"] == "read"},
                        }
                        for name, t in tools.items()
                    ]
                }
            else:
                result = {}
            send({"jsonrpc": "2.0", "id": id, "result": result})


if __name__ == "__main__":
    p = argparse.ArgumentParser()
    p.add_argument("--kind", choices=["fs", "git", "memory"], required=True)
    p.add_argument("--root", required=True)
    p.add_argument("--state", required=True)
    p.add_argument("--width", type=int, required=True)
    p.add_argument("--delay", type=float, default=0)
    serve(p.parse_args())
