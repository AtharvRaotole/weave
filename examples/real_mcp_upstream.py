"""Run a Weave script against the real MCP filesystem server, through weave-mcp.

weave-mcp starts `npx -y @modelcontextprotocol/server-filesystem <dir>`, lists
its tools, and lets `weave_run` scripts call them. Needs Node (npx). No model.
"""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

SCRIPT = """
names = []
for line in fs_list_directory(ROOT).splitlines():
    if line.startswith("[FILE] ") and line.endswith(".txt"):
        names.append(line[7:])
total = 0
for name in sorted(names):
    total += int(fs_read_text_file(ROOT + "/" + name).strip())
cfg = parse_json(fs_read_text_file(ROOT + "/config.json"))
try:
    missing = fs_read_text_file(ROOT + "/nope.txt")
except ToolError:
    missing = "caught"
fs_write_file(ROOT + "/out.txt", f"{cfg['name']}={total}")
print(len(names), total, missing, fs_read_text_file(ROOT + "/out.txt"))
"""


def main() -> None:
    binary = os.environ.get("WEAVE_MCP") or str(ROOT / "target" / "debug" / "weave-mcp")
    if not Path(binary).exists():
        subprocess.check_call(["cargo", "build", "-p", "weave-mcp"], cwd=ROOT)
    with tempfile.TemporaryDirectory() as tmp:
        data = Path(tmp) / "data"
        data.mkdir()
        for name, value in (("a.txt", "17"), ("b.txt", "25"), ("c.txt", "8")):
            (data / name).write_text(value)
        (data / "config.json").write_text('{"name": "sum"}')
        data = data.resolve()
        config = Path(tmp) / "weave-mcp.json"
        config.write_text(json.dumps({"upstreams": [
            {"name": "fs", "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", str(data)], "prefix": "fs_"}
        ]}))
        proc = subprocess.Popen([binary, "--config", str(config)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)

        def request(i, method, params=None):
            proc.stdin.write(json.dumps({"jsonrpc": "2.0", "id": i, "method": method, "params": params or {}}) + "\n")
            proc.stdin.flush()
            return json.loads(proc.stdout.readline())

        request(1, "initialize", {"protocolVersion": "2025-06-18"})
        names = [t["name"] for t in request(2, "tools/list")["result"]["tools"]]
        assert "fs_read_text_file" in names and "fs_write_file" in names, names
        source = f"ROOT = {json.dumps(str(data))}\n" + SCRIPT
        started = time.perf_counter()
        result = request(3, "tools/call", {"name": "weave_run", "arguments": {"source": source}})["result"]
        elapsed = time.perf_counter() - started
        proc.kill()
        text = result["content"][0]["text"]
        assert not result.get("isError"), text
        stdout = json.loads(text)["stdout"]
        assert stdout == "3 50 caught sum=50\n", stdout
        print(f"real_mcp_upstream_ok tools={len(names)} weave_run_s={elapsed:.3f}")


if __name__ == "__main__":
    main()
