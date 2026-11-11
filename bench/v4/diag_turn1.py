import json, sys, urllib.request, os
sys.path.insert(0, '.')
from bench.v4.tasks import SUITES
from bench.v4 import real, agent
from bench.v4.providers import ChatClient
from bench.v4.tasks import SUITES
task = next(t for t in SUITES['real_dev_git'] if t.id == sys.argv[1])
world = real.RealWorld(task, concurrency=8)
wt = agent.weave_tool(world, tools=real.TOOLS, fps=real.FOOTPRINTS)
c = ChatClient('openai', 'gpt-4.1-2025-04-14', live=True, temperature=0, max_tokens=int(sys.argv[3]) if len(sys.argv)>3 else 4096)
msgs = [{"role": "system", "content": agent.SYSTEM}, {"role": "user", "content": task.instruction}]
body = c.responses_payload(msgs, [wt.chat_tool()])
if len(sys.argv) > 4: body["parallel_tool_calls"] = sys.argv[4] == "1"
for i in range(int(sys.argv[2])):
    req = urllib.request.Request(c.base + "/responses", data=json.dumps(body).encode(), method="POST",
        headers={"Authorization": "Bearer " + c._key, "Content-Type": "application/json"})
    out = json.load(urllib.request.urlopen(req, timeout=120))
    items = [(o.get('type'), len(o.get('arguments') or json.dumps(o.get('content') or ''))) for o in out['output']]
    print(i, out['status'], out['usage']['output_tokens'], items, flush=True)
    if out['status'] != 'completed':
        json.dump(out, open(f'/tmp/diag/incomplete-{sys.argv[1]}-{i}.json', 'w'))
world.close()
