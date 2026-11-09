"""A tiny newline-delimited MCP server used by the upstream tests."""
import json
import os
import sys
import threading
import time

notes = {"a": "alpha", "b": "beta"}
lock = threading.Lock()
TOOLS = [
    {"name": "read_note", "description": "Read a note by key.",
     "inputSchema": {"type": "object", "properties": {"key": {"type": "string"}}, "required": ["key"]},
     "annotations": {"readOnlyHint": True}},
    {"name": "write_note", "description": "Write a note.",
     "inputSchema": {"type": "object", "properties": {"key": {"type": "string"}, "text": {"type": "string"}},
                     "required": ["key", "text"]}},
    {"name": "leak", "description": "Fail with a secret in the error content.",
     "inputSchema": {"type": "object", "properties": {}}, "annotations": {"readOnlyHint": True}},
    {"name": "rpc_fail", "description": "Fail with a secret in a JSON-RPC error.",
     "inputSchema": {"type": "object", "properties": {}}, "annotations": {"readOnlyHint": True}},
    {"name": "env_probe", "description": "Return an environment variable from the server process.",
     "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]},
     "annotations": {"readOnlyHint": True}},
]

sys.stderr.write("fake server starting with UPSTREAM-STDERR-SECRET\n")
sys.stderr.flush()


def reply(msg_id, result):
    with lock:
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg_id, "result": result}) + "\n")
        sys.stdout.flush()


def handle(msg):
    method, msg_id, params = msg.get("method"), msg.get("id"), msg.get("params") or {}
    if method == "initialize":
        reply(msg_id, {"protocolVersion": params.get("protocolVersion"), "capabilities": {"tools": {}},
                       "serverInfo": {"name": "fake", "version": "0"}})
    elif method == "tools/list":
        reply(msg_id, {"tools": TOOLS})
    elif method == "tools/call":
        args = params["arguments"]
        if params["name"] == "leak":
            sys.stderr.write("leak called: UPSTREAM-STDERR-SECRET\n")
            sys.stderr.flush()
            reply(msg_id, {"content": [{"type": "text", "text": "UPSTREAM-ERROR-SECRET"}], "isError": True})
        elif params["name"] == "rpc_fail":
            with lock:
                sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg_id,
                                             "error": {"code": -32000, "message": "RPC-SECRET"}}) + "\n")
                sys.stdout.flush()
        elif params["name"] == "env_probe":
            reply(msg_id, {"content": [{"type": "text", "text": os.environ.get(args["name"], "absent")}]})
        elif params["name"] == "read_note":
            time.sleep(0.2)
            if args["key"] not in notes:
                reply(msg_id, {"content": [{"type": "text", "text": f"no note {args['key']}"}], "isError": True})
            else:
                reply(msg_id, {"content": [{"type": "text", "text": notes[args["key"]]}]})
        else:
            notes[args["key"]] = args["text"]
            reply(msg_id, {"content": [{"type": "text", "text": "ok"}]})


for line in sys.stdin:
    if line.strip():
        message = json.loads(line)
        if "id" in message:
            threading.Thread(target=handle, args=(message,)).start()
