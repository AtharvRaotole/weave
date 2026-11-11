# Getting started with Weave

Weave gives an agent **one tool that runs a small script** instead of many
single-call tools. The script can call your functions or MCP tools, branch,
loop, catch tool errors and combine results. Weave runs independent reads
concurrently, orders writes by their declared effect, and records a trace you
can replay without touching live systems.

Use it when an agent routinely needs several dependent or fan-out tool calls
per answer and you want those calls to happen in your runtime, not in extra
model turns. Do not expect it to beat native tool calling on every task: on
one-call tasks it adds nothing, and see [benchmarks](benchmarks.md) for what
has and has not been measured.

## Where you can use it

| You have | Use | Status in 1.0 |
| --- | --- | --- |
| A Python app calling any function-calling model API (OpenAI Chat Completions or Responses, or anything that takes a JSON-schema tool) | `weave.WeaveTool` | Supported |
| The OpenAI Agents SDK | `WeaveTool.agents_tool()` (`pip install openai-agents`) | Supported; tested in CI |
| An MCP client (any client that launches stdio servers, e.g. desktop chat apps and IDEs) | the `weave-mcp` binary, optionally proxying your other stdio MCP servers | Supported, stdio only |
| A script you want to run, check or replay from a shell | the `weave` CLI | Supported |
| A Rust host | the `weave` crate (`crates/weave`) | Supported; not published to crates.io |

Not in 1.0: remote (HTTP/SSE) MCP transports, async Python tool functions, a
step debugger. See [production](production.md) for the full support statement.

## Install

The distribution is **`weave-lang`**; you `import weave`.

```bash
# Intended public command, once the package is on PyPI (it is not yet):
pip install weave-lang
```

Paths that work today for anyone with access to the repository and releases:

```bash
# 1) A prebuilt wheel from the v1.0.0 GitHub release (no Rust needed).
#    Pick the file for your Python (cp39-cp313) and platform, then:
python -m pip install ./weave_lang-1.0.0-cp312-cp312-manylinux2014_x86_64.manylinux_2_17_x86_64.whl
#    With the GitHub CLI:
gh release download v1.0.0 -R AtharvRaotole/weave -p 'weave_lang-1.0.0-cp312-*manylinux*x86_64*' -p SHA256SUMS.txt
sha256sum --ignore-missing -c SHA256SUMS.txt

# 2) From the tagged source (needs Rust >= 1.97 via rustup):
pip install "git+https://github.com/AtharvRaotole/weave@v1.0.0"
#    or the release sdist: pip install ./weave_lang-1.0.0.tar.gz
```

Do not install `weave` (Weights & Biases Weave) in the same environment; it
uses the same import name.

The MCP proxy is a separate Rust binary:

```bash
git clone https://github.com/AtharvRaotole/weave && cd weave
cargo build --release --locked -p weave-mcp   # -> target/release/weave-mcp
```

Check the install:

```bash
python -c "import weave; print(weave.__version__)"   # 1.0.0
python examples/quickstart.py                        # prints 49, then "quickstart ok"
```

## Recipe 1: a one-file script (no model, no key)

[`examples/quickstart.py`](../examples/quickstart.py):

```python
import weave

@weave.declare(effect="read", arity=1)
def list_orders(customer_id):
    """List order ids for a customer."""
    return {"c9": ["o1", "o2", "o3"]}.get(customer_id, [])

@weave.declare(effect="read", arity=1)
def order_total(order_id):
    """Integer total of one order."""
    return {"o1": 12, "o2": 30, "o3": 7}[order_id]

engine = weave.Engine()
engine.install([list_orders, order_total])
result = engine.run("""
total = 0
for order in list_orders("c9"):
    total += order_total(order)
print(total)
""")
print(result.stdout)  # 49
```

`effect` is how Weave schedules a call: `pure` and `read` calls can overlap;
`reversible` and `irreversible` (the default) are ordered and never retried
unless you mark them `retry_safe`. Effects are declarations, not permissions.

## Recipe 2: give a model one tool

```python
tool = weave.WeaveTool([list_orders, order_total], worker_pool=2)

tools = [tool.chat_tool()]          # Chat Completions
# tools = [tool.responses_tool()]   # Responses API
# Agent(..., tools=[tool.agents_tool()])  # OpenAI Agents SDK

# In your loop, for each tool call the model returns:
messages.append(tool.handle(call))  # runs the script, returns printed output or a repairable error
# Or, for all the calls of one model turn at once (read-only scripts run concurrently):
messages.extend(tool.handle_many(calls))
```

A script's last bare expression is printed, as in a REPL, so `get_customer(cid)` on its own line shows its
value.

The tool description teaches the model the language rules and lists your
functions with their effects. [`examples/run_weave_tool.py`](../examples/run_weave_tool.py)
is a complete Chat Completions loop: with `OPENAI_API_KEY` set it asks a real
model; without it, it replays a canned call so it runs offline.

`worker_pool=N` runs every function in a killable worker process, which is what
makes deadlines hard. Functions must then be module-level and picklable.

### A support or chat assistant (a person replies between turns)

```python
from weave.agent_tool import shape_of_samples

# Tell the model what each lookup returns. Without output schemas, derive it from a few real results:
get_order.__weave_returns__ = shape_of_samples([get_order(o) for o in some_order_ids])

tool = weave.WeaveTool([find_user, get_user, get_order, cancel_order],
                       conversational=True,      # fetch everything the next reply needs in one script;
                                                 # no state change before the person confirms it
                       persist_variables=True)   # later scripts can reuse `order` from an earlier one
```

`conversational=True` adds two rules to the tool description: chain dependent lookups in one script (find the id,
fetch the record, fetch its related records) and print a compact summary, and never call a function that changes
state before the person has confirmed that exact change. Your own policy still decides what needs confirmation.

`persist_variables=True` works like a notebook: each script sees the variables the earlier ones set, as snapshots
taken when those scripts ended. They reach the engine as literal assignments, so effects, scheduling and limits are
unchanged, and error positions still refer to the model's own script.

`shape_of_samples` merges several results into one short type, e.g.
`{"order_id": str, "items": list[{"item_id": str, "price": float, "options": dict[str, str]}]}`. Fields whose keys
differ between records (free-form options) become `dict[str, str]` instead of one sample's keys, which misled
GPT-4.1 in a round-6 dev run.

## Recipe 3: an MCP client, through weave-mcp

`weave-mcp` exposes `weave_check`, `weave_run` and `weave_replay`, plus the
tools of any stdio MCP servers you list as upstreams. Save `upstreams.json`:

```json
{
  "upstreams": [
    {
      "name": "filesystem",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/absolute/path/to/data"],
      "prefix": "fs_",
      "results": {"list_directory": "lines"}
    }
  ]
}
```

Then point your MCP client at the binary (absolute paths):

```json
{
  "mcpServers": {
    "weave": {
      "command": "/absolute/path/to/weave/target/release/weave-mcp",
      "args": ["--config", "/absolute/path/to/upstreams.json"]
    }
  }
}
```

A script sent to `weave_run` can now call `fs_read_text_file(...)`,
`fs_list_directory(...)` and so on. CI runs `weave-mcp` against the official
filesystem, git and memory reference servers with real writes.
`python examples/real_mcp_upstream.py` is a local smoke test with temporary
data. Upstreams get an allowlisted environment, and their stderr and error
payloads are withheld from scripts by default ([privacy](privacy.md)).

## Recipe 4: hard deadlines and cancelling a run

[`examples/cancel_run.py`](../examples/cancel_run.py) starts a slow tool in a
worker, cancels the awaiting asyncio task, and checks that the tool stopped:

```python
tool = weave.WeaveTool([slow_report], worker_pool=1)
task = asyncio.create_task(tool.run_async("print(slow_report())\n"))
...
task.cancel()   # cancels the script session; the worker process is killed
```

[`examples/hard_deadline.py`](../examples/hard_deadline.py) shows a per-tool
`timeout_ms` killing a non-cooperative tool. In-process tools (no worker pool)
and MCP upstream calls only get cooperative cancellation.

## Recipe 5: check, run and replay from a shell

```bash
weave check examples/hello.weave --tool name=weather,effect=read,arity=1
weave run examples/hello.weave --tool name=weather,effect=read,behavior=echo,arity=1 --trace-out /tmp/t.json
weave replay examples/hello.weave --trace /tmp/t.json --tool name=weather,effect=read,behavior=echo,arity=1
```

## Before you put it in front of users

Read [Running Weave in production](production.md). In short:

- Weave is supported in production when the host owns its tools, runs tools
  whose runtime matters in worker processes with deadlines, and treats
  anything a tool returns or prints as visible to the model.
- There is no rollback and no exactly-once: a timed-out write may have happened.
- Worker processes are not a security sandbox; they share the host user,
  network, filesystem and credentials. Windows cannot kill grandchild
  processes and has no `WorkerLimits`; macOS has no memory limit.
- Traces contain tool arguments and results; store them like logs with user data.

Found a bug? Open an issue with a small script and the tool declarations
(check traces for sensitive data first). Security issues: see
[SECURITY.md](../SECURITY.md).
