# Weave

Weave is a small language and runtime for LLM agents. An agent writes a short
Python-like script of tool calls instead of issuing those calls one after
another. A Rust engine runs independent pure and read calls together, holds
irreversible calls until the script is complete, and writes a JSON trace that
replays the same output.

## Quickstart

```bash
cargo test --workspace
python3 -m venv .venv
.venv/bin/pip install 'maturin>=1.8,<2'
.venv/bin/maturin develop --release
.venv/bin/python examples/speedup.py
```

```python
from weave import Engine

def weather(city: str) -> str:
    return f"18C {city}"

engine = Engine()
engine.tool("weather", weather, effect="read", timeout_ms=1000, retries=1)
result = engine.run('temp = weather("Paris")\nprint(temp)\n')
print(result.stdout)
engine.replay('temp = weather("Paris")\nprint(temp)\n', result.trace_json())
```

`check` returns one diagnostic per line, with a column and a `hint` an LLM
can apply. `try` / `except ToolError` catches a host-tool failure, timeout, or
exhausted retry. A bare `except` also catches ordinary runtime errors.
Cancellation is not caught.

## Cancellation and timeouts

`timeout_ms` and `Session.cancel()` abandon the in-flight attempt:

- **Async host tools** (`Engine::register_async`) are a future. On timeout or
  cancel that future is dropped, so work waiting on `.await` stops. Select on
  `token.cancelled()` as well if the tool spawned its own tasks.
- **Cooperative tools**, including Python functions that take a `cancel`
  argument or `**kwargs`, receive a `CancelToken`. Poll `cancel.is_cancelled()`
  and return. Weave fires the token and stops waiting; it does not kill the
  thread. A tool that never looks at the token can keep running after Weave
  has already moved on.
- **Plain sync tools** that do not take a token only have the wait abandoned.
  Their thread is not signalled.
- **Hard deadlines** opt in with `Engine(worker_pool=N)` and
  `hard_deadline=True`. The call runs in a spawned Python worker. Timeout and
  cancel `SIGKILL` that worker and start a replacement. `finally` inside the
  worker does not run, and side effects that already left the process are not
  undone. The function must be pickleable and defined at module scope; create
  the engine under `if __name__ == "__main__"` so workers do not start another
  pool on import. Closures are not sendable. See `python/weave/workers.py`.

```python
def slow(name, cancel):
    for _ in range(100):
        if cancel.is_cancelled():
            raise RuntimeError("stopped")
        time.sleep(0.01)
    return name
```

## Language

Scripts are a subset of Python, parsed with `ruff_python_parser`. Supported:

- assignment, including unpacking and augmented assignment (`+=`, `-=`, …)
- calls, including keyword arguments on `print`
- `if` / `elif` / `else` and conditional expressions
- `for` over a list or string
- lists, dicts, f-strings, comparisons, boolean operators, and arithmetic
- subscripts and methods such as `str.strip`, `str.find`, `str.title`, `dict.get`, `dict.items`, `list.index`, and `" ".join`
- builtins `len`, `range`, `sum`, `min`, `max`, `sorted`, `reversed`, `enumerate`, `zip`, `any`, `all`, and `list`
- `try` / `except ToolError` and a bare `except`

Not supported, with a one-line error an LLM can patch: `import`, `def`,
`class`, `while`, `with`, `lambda`, comprehensions, and `await`.
There is no import system. Host tools are registered by the runtime.

```python
city = "Paris"
temp = weather(city)
air = air_quality(city)
print(f"{city}: {temp}, {air}")
if "bad" in air:
    page(city)
```

`weather` and `air_quality` start together. `page` is irreversible, so it runs
only after the whole script has been received and its inputs are ready.

## Effects

Every tool has an effect. A tool registered with no effect is irreversible.

| Effect | When it runs |
| --- | --- |
| `pure` | As soon as its arguments are ready. Independent pure calls overlap. |
| `read` | As soon as its arguments are ready and earlier writes that could change what it observes have finished. Independent reads overlap. |
| `reversible` | After earlier reads and writes it could conflict with. It may run before the script is finished, because the host can undo it. |
| `irreversible` | Only after the script is complete, and after earlier observable work. |

Calls that do not depend on each other are scheduled together. `and`, `or`,
and conditional expressions stay lazy, so a call on the untaken side does not
run. Loop iterations with no carried assignment and no writes run together;
`total = total + n` stays sequential.

## Streaming

`Session.push` executes each completed top-level statement while later source
is still arriving. An open `if` or `for` stays buffered until a later line
closes it, or until `finish`. Irreversible calls queued by `push` do not start
until `finish`.

## Traces

A run records every host call in program order:

```json
{
  "version": 1,
  "events": [
    {
      "seq": 0,
      "line": 1,
      "tool": "weather",
      "effect": "read",
      "args": [{"t": "str", "v": "Paris"}],
      "kwargs": {},
      "result": {"t": "str", "v": "18C"}
    }
  ]
}
```

Values are tagged (`none`, `bool`, `int`, `float`, `str`, `list`, `dict`) so
integers and dict order round-trip. Replay runs the same script, returns the
recorded results instead of calling tools, and reproduces the prints and
bindings. Replay matches each call by tool and arguments (identical calls in
the order they finished), so a trace recorded from a streamed run replays
against the full script. If the script makes a call the trace doesn't have,
replay stops with `replay diverged`.

## Correctness tests

`crates/weave/tests/differential.rs` generates random programs over a small
key-value store (pure, read, reversible, and irreversible tools, `if`, `for`,
`+=`, `print`) and runs each one five ways: an independent tree-walking oracle
in the test file, Weave with every tool forced sequential, normal parallel
Weave with jittered tool delays, a streamed session fed in random chunks, and
a speculative run that pre-runs every read the program makes. Prints,
bindings, the final store, the irreversible log, and the multiset of calls must
match. Each successful trace, including the streamed one, must replay. 300
seeds run in `cargo test`; set `WEAVE_DIFF_SEEDS` for more.

## Sandbox

Scripts can't import, open files, reach the network, define functions, or
touch Python objects; host tools are the only way out. Runs are limited to
1,000,000 steps, 16 MiB per value, and nesting depth 100 by default. See
[docs/runtime.html](docs/runtime.html#sandbox-and-limits) for the review and
what is not covered.

## Static check

`check` walks the script before any tool runs. Diagnostics are one line and
include the location:

```text
L1:C1: `import` is not supported; inline values or fetch them with a tool
L2:C1: unknown tool `serch`; did you mean `fetch`?
L2:C1: tool `fetch` takes 1 argument(s), got 2
L3:C7: name `missing` is used before assignment
```

## Rust

```bash
cargo test
```

The crate is `weave`. Register a tool with `Engine::register`. `ToolSpec::default()`
is irreversible and accepts any arity. `Engine::run`, `Engine::replay`, and
`Engine::session` share one scheduler.

Requires Rust 1.97 or newer (`rust-toolchain.toml` pins 1.99.1) because that is
the MSRV of `ruff_python_parser`.

## Python

Bindings are a PyO3 extension built with maturin. The module releases the GIL
around `run`, `replay`, `push`, and `finish`, so tools that block in C (including
`time.sleep`) can overlap.

```bash
python3 -m venv .venv
.venv/bin/pip install 'maturin>=1.8,<2'
.venv/bin/maturin develop --release
.venv/bin/python examples/speedup.py
```

`examples/speedup.py` times four delayed reads sequentially and through Weave,
checks that replay matches, and prints the timings it measured. It does not
embed a speedup figure.

```python
from weave import Engine

engine = Engine()
engine.tool("weather", weather, effect="read")
result = engine.run('temp = weather("Paris")\nprint(temp)\n')
print(result.stdout)
engine.replay(source, result.trace_json())
```

## MCP

`weave-mcp` speaks MCP over stdio with `Content-Length` framing.

```bash
cargo run -p weave-mcp --release
```

Tools:

- `weave_check` — `{ "source": "..." }` returns `ok`, or one diagnostic per line
- `weave_run` — runs `source` with scripted tools (`behavior` `echo` or `const`, optional `effect`, `arity`, `delay_ms`, `result`)
- `weave_replay` — replays `source` from `trace`; pass the same `tools` list so the checker knows their arity

`echo` returns the single positional argument. `const` returns the tagged `result`.

## Measured results

Mock tools sleep for a configurable base latency (40 ms in this run, three
repeats, median wall clock). Raw samples, machine info, and the round-trip
definition are in [`bench/results/mock-20261108T021733Z.json`](bench/results/mock-20261108T021733Z.json)
([`mock-latest.json`](bench/results/mock-latest.json) is the same file).
Rounds are waves observed from start/end timestamps, not a planned schedule.
Every runner below was correct on every repeat.

| Task | Runner | Median wall (s) | Rounds | Tool calls |
| --- | --- | ---: | ---: | ---: |
| research_brief | sequential | 0.180959 | 5 | 5 |
| research_brief | asyncio.gather | 0.063764 | 2 | 5 |
| research_brief | DAG | 0.063927 | 2 | 5 |
| research_brief | Weave | 0.062249 | 2 | 5 |
| research_brief | Monty | 0.185408 | 5 | 5 |
| trip_plan | sequential | 0.220662 | 6 | 6 |
| trip_plan | asyncio.gather | 0.143810 | 4 | 6 |
| trip_plan | DAG | 0.103826 | 3 | 6 |
| trip_plan | Weave | 0.102194 | 3 | 6 |
| trip_plan | Monty | 0.225473 | 6 | 6 |
| support_ticket | sequential | 0.180494 | 5 | 5 |
| support_ticket | asyncio.gather | 0.143868 | 4 | 5 |
| support_ticket | DAG | 0.143551 | 4 | 5 |
| support_ticket | Weave | 0.142729 | 4 | 5 |
| support_ticket | Monty | 0.185144 | 5 | 5 |

Machine: Linux 6.12.113+ x86_64, 4 CPUs, Python 3.12.3, hostname `cursor`.
Pydantic Monty 1.5.0 executed the same Python calls one at a time inside a warm worker.
The hand-written `asyncio.gather` for `trip_plan` waits for weather before flights and hotels; the DAG runner and Weave overlap those three calls.

No `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, or `GOOGLE_API_KEY` was set, so the real-model eval did not call a provider and did not record a pass rate. See [`bench/results/llm-latest.json`](bench/results/llm-latest.json). To measure tokens, pass rate, and retries of models writing Weave versus ordered JSON tool calls:

```bash
export OPENAI_API_KEY=...   # or ANTHROPIC_API_KEY, or GEMINI_API_KEY
# optional: OPENAI_MODEL, ANTHROPIC_MODEL, GEMINI_MODEL
python bench/llm_eval.py
```

Re-run the mock suite with `python bench/run_mock.py`. `BENCH_LATENCY_MS` and `BENCH_REPEATS` change the sleep and the number of repeats. `pydantic-monty` is optional; without it the Monty column is recorded as skipped.

### Streaming into a live session

`bench/streaming.py` turns OpenAI chat chunks (`choices[0].delta.content`) and Anthropic events (`content_block_delta` / `delta.text`) into text and pushes each piece into `Session.push`. Safe calls start once their statement is complete, before the rest of the script arrives.

A deterministic demo sleeps 12 ms between 4-character chunks and 80 ms inside each `slow` tool. Three repeats, medians, from [`bench/results/stream-20261108T022244Z.json`](bench/results/stream-20261108T022244Z.json):

| Mode | Median time to first tool (s) | Median total (s) |
| --- | ---: | ---: |
| streaming `Session.push` | 0.049319 | 0.166217 |
| `Engine.run` after the same gaps | 0.146334 | 0.227328 |

`bench/prompts.py` is the system prompt and few-shot examples. `bench/llm_eval.py` uses them, plus `repair_message`, on the tasks in `bench/dataset.py`. Weave and an ordered JSON tool-call baseline are graded the same way. Without an API key the eval writes a skip record and no pass rate.

### Speculative pre-execution

An optional predictor pre-runs pure and read calls and stores each result under the tool name plus canonical arguments, the tool cache from Nichols et al., [arXiv 2512.15834](https://arxiv.org/abs/2512.15834). Reversible and irreversible calls are rejected. Before a write runs, cached reads whose `reads` footprint overlaps the write's `writes` footprint are dropped and re-run live; footprints default to every resource. Data-dependent calls are not guessed. One unused prediction per task is included so wasted work is measured. Generation is an 80 ms sleep; the predictor returns after 10 ms; tool sleep is 40 ms. Three repeats, medians, from [`bench/results/spec-20261108T023548Z.json`](bench/results/spec-20261108T023548Z.json):

| Task | After generate (s) | Speculative (s) | Hits | Misses | Wasted | Hit rate |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| research_brief | 0.142365 | 0.101583 | 4 | 1 | 1 | 0.800 |
| trip_plan | 0.182730 | 0.142256 | 2 | 4 | 1 | 0.333 |
| support_ticket | 0.223358 | 0.182742 | 2 | 3 | 1 | 0.400 |

Hit rate is `hits / (hits + misses)` over the pure and read calls the program made. Every repeat was correct. Machine: Linux 6.12.113+ x86_64, 4 CPUs, Python 3.12.3, hostname `cursor`.

## CLI and docs

`docs/` is a small static site. After `maturin develop` or a wheel install:

```bash
weave check examples/hello.weave --tool name=weather,effect=read,arity=1
weave run examples/hello.weave --tool name=weather,effect=read,behavior=echo
weave trace trace.json
```

`cargo run -p weave-cli --` is the same four commands (`check`, `run`, `replay`, `trace`) for echo and const tools. `@weave.declare(...)` marks a function; `engine.install([fn])` registers it. `@engine.declare(...)` registers it immediately.

## MCP

`weave-mcp --config examples/tools.json` lists `weave_check`, `weave_run`, `weave_replay`, and each configured tool. A request that omits `tools` uses the config. `examples/mcp-client-config.json` is a client entry. `python examples/mcp_client.py` performs the handshake, calls `weather`, and runs a script.

## Layout

- `crates/weave` — parser, checker, scheduler, trace
- `crates/weave-py` — PyO3 module
- `crates/weave-mcp` — MCP server
- `examples/speedup.py` — measured comparison with sequential calls
- `bench/` — mock suite, speculation measurement, optional real-model eval, and `bench/results`
- `docs/` — static docs
- `crates/weave-cli` — `weave` command for scripted tools
