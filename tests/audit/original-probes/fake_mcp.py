"""Local synthetic server for MCP compatibility tests; no network access."""
import json
import sys
import time
from pathlib import Path

label = sys.argv[1]
marker = Path(sys.argv[2])
tools = [{"name": name, "description": name, "inputSchema": {"type": "object", "properties": {}}, "annotations": {"readOnlyHint": name != "slow_write"}} for name in ("same", "secret", "structured", "read-file", "slow_write")]
for line in sys.stdin:
    message = json.loads(line)
    if "id" not in message:
        continue
    method = message["method"]
    params = message.get("params", {})
    if method == "initialize":
        result = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "audit", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": tools, "nextCursor": "second-page"} if not params.get("cursor") else {"tools": [{**tools[0], "name": "second_page"}]}
    elif method == "tools/call":
        name = params["name"]
        if name == "structured":
            result = {"content": [], "structuredContent": {"answer": 42}, "isError": False}
        else:
            if name == "slow_write":
                time.sleep(0.35)
                marker.write_text("synthetic write completed")
            text = "AUDIT_SYNTHETIC_PRIVATE_VALUE" if name == "secret" else label
            result = {"content": [{"type": "text", "text": text}], "isError": False}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}), flush=True)
