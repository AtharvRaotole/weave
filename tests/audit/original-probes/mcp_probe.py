import json
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
repo = Path(sys.argv[1]).resolve()
binary = repo / "target/debug/weave-mcp"

def call(name, arguments):
    return {"method": "tools/call", "params": {"name": name, "arguments": arguments}}

def run(messages, duplicate=False):
    config = {"upstreams": [{"name": label, "command": sys.executable, "args": [str(HERE / "fake_mcp.py"), label, str(HERE / "cancel-marker")], "prefix": "" if duplicate else "a_"} for label in (("A", "B") if duplicate else ("A",))]}
    path = HERE / "mcp-config.json"
    path.write_text(json.dumps(config))
    payload = "".join(json.dumps({"jsonrpc": "2.0", **({} if m["method"].startswith("notifications/") else {"id": i + 1}), **m}) + "\n" for i, m in enumerate(messages))
    before = time.monotonic()
    proc = subprocess.run([str(binary), "--config", str(path)], input=payload, text=True, capture_output=True, timeout=8)
    return {"responses": [json.loads(s) for s in proc.stdout.splitlines()], "elapsed_s": time.monotonic() - before, "exit": proc.returncode, "stderr": proc.stderr}

out = {}
out["discovery_and_results"] = run([
    {"method": "tools/list"},
    call("weave_run", {"source": 'raw = a_secret()\nprint("summary only")\n'}),
    call("a_structured", {}),
    call("weave_check", {"source": 'print(a_read-file())\n'}),
    call("weave_check", {"source": 'print(a_second_page())\n'}),
])
out["duplicate_routing"] = run([call("same", {}), call("weave_run", {"source": 'print(same())\n'})], duplicate=True)
marker = HERE / "cancel-marker"
marker.unlink(missing_ok=True)
out["cancel"] = run([
    call("weave_run", {"source": 'a_slow_write()\nprint("done")\n'}),
    {"method": "notifications/cancelled", "params": {"requestId": 1, "reason": "audit cancellation"}},
    {"method": "ping"},
])
out["cancel"]["write_completed"] = marker.exists()
print(json.dumps(out, indent=2))
