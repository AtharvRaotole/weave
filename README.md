# Weave

Weave is a small language and runtime for LLM agents. An agent writes a short
Python-like script of tool calls instead of issuing those calls one after
another. A Rust engine runs independent pure and read calls together, holds
irreversible calls until the script is complete, and writes a JSON trace that
replays the same output.

## Language

Scripts are a subset of Python, parsed with `ruff_python_parser`. Supported:

- assignment, including unpacking and augmented assignment (`+=`, `-=`, …)
- calls, including keyword arguments on `print`
- `if` / `elif` / `else` and conditional expressions
- `for` over a list or string
- lists, dicts, f-strings, comparisons, boolean operators, and arithmetic
- subscripts and a few methods (`str.strip`, `dict.get`, `" ".join`, …)

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

## Layout

- `crates/weave` — parser, checker, scheduler, trace
- `crates/weave-py` — PyO3 module
- `crates/weave-mcp` — MCP server
- `examples/speedup.py` — measured comparison with sequential calls
