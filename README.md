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
can apply. `Session.cancel()` stops irreversible calls that have not started.
`try` / `except ToolError` catches a host-tool failure, timeout, or exhausted
retry. A bare `except` also catches ordinary runtime errors. Cancellation is
not caught.

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
`class`, `while`, `try`, `with`, `lambda`, comprehensions, and `await`.
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
bindings. If a call's tool or arguments differ, replay stops with
`replay diverged at call N`.

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

## Layout

- `crates/weave` — parser, checker, scheduler, trace
- `crates/weave-py` — PyO3 module
- `crates/weave-mcp` — MCP server
- `examples/speedup.py` — measured comparison with sequential calls
- `bench/` — mock suite, optional real-model eval, and `bench/results`
