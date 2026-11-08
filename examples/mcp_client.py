"""Speak MCP over stdio to weave-mcp and call a configured tool.

Point an existing client at the same server with examples/mcp-client-config.json.
This script is the small client: initialize, list tools, call weather, then
weave_run using the server config instead of passing tools again.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def main() -> None:
    binary = os.environ.get("WEAVE_MCP")
    if not binary:
        built = ROOT / "target" / "debug" / "weave-mcp"
        if not built.exists():
            subprocess.check_call(
                ["cargo", "build", "-p", "weave-mcp"],
                cwd=ROOT,
            )
        binary = str(built)
    proc = subprocess.Popen(
        [binary, "--config", str(ROOT / "examples" / "tools.json")],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
    )
    assert proc.stdin and proc.stdout

    def request(message: dict) -> dict:
        body = json.dumps(message).encode()
        proc.stdin.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
        proc.stdin.flush()
        headers = {}
        while True:
            line = proc.stdout.readline()
            if line in (b"\r\n", b"\n", b""):
                break
            key, value = line.decode().split(":", 1)
            headers[key.lower()] = value.strip()
        length = int(headers["content-length"])
        return json.loads(proc.stdout.read(length))

    init = request(
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "example", "version": "0"}},
        }
    )
    if init["result"]["serverInfo"]["name"] != "weave":
        raise SystemExit(f"initialize failed: {init}")
    listed = request({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
    names = [tool["name"] for tool in listed["result"]["tools"]]
    if "weather" not in names or "weave_run" not in names:
        raise SystemExit(f"tools/list missing configured tools: {names}")
    called = request(
        {
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "weather", "arguments": {"args": ["Paris"]}},
        }
    )
    if called["result"].get("isError"):
        raise SystemExit(f"weather failed: {called}")
    if "Paris" not in called["result"]["content"][0]["text"]:
        raise SystemExit(f"weather returned {called}")
    ran = request(
        {
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "weave_run",
                "arguments": {"source": 'print(weather("Paris"))\n'},
            },
        }
    )
    text = ran["result"]["content"][0]["text"]
    if ran["result"].get("isError") or "Paris" not in text:
        raise SystemExit(f"weave_run failed: {text}")
    proc.stdin.close()
    proc.wait(timeout=5)
    print("mcp_client_ok")


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:
        print(f"mcp_client failed: {exc}", file=sys.stderr)
        raise
