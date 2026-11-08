"""Acceptance checks over the exact local synthetic MCP fixture."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
FAKE = Path(__file__).parent / "original-probes/fake_mcp.py"
def run(messages, upstreams=None, tools=None):
    with tempfile.TemporaryDirectory() as temp:
        marker = str(Path(temp)/"marker")
        ups = [{"name": label, "command": sys.executable, "args": [str(FAKE),label,marker], "prefix": prefix} for label,prefix in (upstreams or [("A","a_")])]
        cfg = Path(temp)/"config.json"
        cfg.write_text(json.dumps({"upstreams": ups, "tools": tools or []}))
        reqs = [dict(jsonrpc="2.0", id=i, method="tools/call", params={"name":n,"arguments":args}) for i,(n,args) in enumerate(messages)]
        p = subprocess.Popen([str(ROOT/"target/debug/weave-mcp"),"--config",str(cfg)], stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        p.stdin.write(''.join(json.dumps(r)+'\n' for r in reqs)); p.stdin.flush()
        replies = [json.loads(p.stdout.readline()) for _ in reqs]
        p.stdin.close(); p.stdin=None
        _, err = p.communicate(timeout=8)
        p.stderr = err
        return p, sorted(replies,key=lambda r:r["id"])

def body(reply):
    return json.loads(reply["result"]["content"][0]["text"])

p, _ = run([], [("A",""),("B","")])
assert p.returncode != 0 and all(s in p.stderr for s in ("duplicate", "A", "B")), p.stderr
for name in ("print", "weave_run", "len"):
    p,_ = run([], tools=[{"name":name}])
    assert p.returncode != 0 and "reserved" in p.stderr
p,r = run([
    ("a_same",{}),
    ("weave_run",{"source":'print(a_same())\n'}),
    ("weave_run",{"source":'raw=a_secret()\nprint("summary only")\n'}),
    ("weave_run",{"source":'print(a_second_page())\n'}),
    ("weave_run",{"source":'print(a_read_u2d_file())\n'}),
    ("a_structured",{}),
    ("weave_run",{"source":'x=a_structured()\nprint(x["answer"])\n'}),
    ("weave_run",{"source":'raw=a_secret()\nprint("summary only")\n',"debug":True}),
])
assert p.returncode == 0, p.stderr
assert r[0]['result']['content'][0]['text'] == body(r[1])['stdout'].strip() == 'A'
assert "AUDIT_SYNTHETIC_PRIVATE_VALUE" not in json.dumps(r[2])
assert body(r[2]) == {"stdout":"summary only\n","status":"ok"}
assert body(r[3])["stdout"] == body(r[4])["stdout"] == 'A\n'
assert r[5]['result']['structuredContent'] == {"answer":42}
assert body(r[6])["stdout"] == '42\n'
assert "AUDIT_SYNTHETIC_PRIVATE_VALUE" in json.dumps(r[7])
print('MCP collision, routing, compact output, pagination, alias, structured-content and debug opt-in checks passed')
