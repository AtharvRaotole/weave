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
- `for` over a list or string, with `break` and `continue` (a loop that uses
  them runs in order; calls within one iteration still overlap)
- lists, dicts, f-strings, comparisons, boolean operators, and arithmetic
- subscripts and methods such as `str.strip`, `str.find`, `str.split(sep, maxsplit)`, `str.rsplit`, `str.title`, `dict.get`, `dict.items`, `list.index`, and `" ".join`
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

### Real-model eval (OpenAI `gpt-5.4-mini` and `gpt-6.1-sol`)

Measured 2026-11-08 with `OPENAI_MODELS=gpt-5.4-mini,gpt-6.1-sol python bench/llm_eval.py`: 16 tasks × 3 modes × 3 repeats × 2 models = 288 graded runs, every tool call sleeping 40 ms in every mode. Raw rows: [`bench/results/llm-20261108T151237Z.json`](bench/results/llm-20261108T151237Z.json).

The tasks ([`bench/dataset.py`](bench/dataset.py)) cover parallel reads (2), chained dependencies (2), branching on a result (3), loops over a list result (3), tool failures that must be caught (2), and write ordering (4). Each run gets fresh tool state. Write tasks pass only if the printed answer matches and the log of write calls matches exactly, in order, with nothing extra. For example, `reserve_if_in_stock` fails if `reserve` is called at all.

- `weave`: the model writes one Weave script using the prompt pack and few-shot examples, and Weave runs it.
- `native_tools`: standard OpenAI function calling through the Responses API. Each turn is one model round trip, and the requested tools run one after another. A failing tool returns `ERROR: ...` to the model.
- `json_tool_calls`: the model writes one ordered JSON list of literal calls, using its own prompt. It has no variables, so it cannot pass a result into a later call or branch on one. It is a lower bound, not a fair competitor.

Weave and JSON get up to 3 attempts. A retry after a wrong answer shows the model what its own run printed, never the expected answer or the expected writes. (The earlier version leaked the expected answer.) `gpt-6.1-sol` ran at `reasoning_effort=low` in every mode. Chat Completions rejects function tools while that model reasons, which is why native mode uses the Responses API for both models.

| Model | Mode | Passed | First try | Median latency (s) | Median model turns | Input tokens | Output tokens |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| gpt-5.4-mini | json_tool_calls | 11/48 | 3/48 | 2.120 | 3 | 40,397 | 5,546 |
| gpt-5.4-mini | native_tools | 48/48 | 48/48 | 3.176 | 3 | 29,643 | 3,870 |
| gpt-5.4-mini | weave | 48/48 | 48/48 | 0.719 | 1 | 29,940 | 1,985 |
| gpt-6.1-sol | json_tool_calls | 22/48 | 0/48 | 6.626 | 3 | 36,988 | 10,263 |
| gpt-6.1-sol | native_tools | 48/48 | 48/48 | 4.332 | 3 | 29,643 | 3,870 |
| gpt-6.1-sol | weave | 48/48 | 48/48 | 1.563 | 1 | 29,940 | 2,125 |

Pass counts and median latency by category:

| Model | Mode | parallel | chain | branch | loop | tool_error | write_order |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| gpt-5.4-mini | weave | 6/6, 0.71s | 6/6, 0.63s | 9/9, 0.71s | 9/9, 1.00s | 6/6, 0.75s | 12/12, 0.73s |
| gpt-5.4-mini | native_tools | 6/6, 3.17s | 6/6, 3.67s | 9/9, 2.94s | 9/9, 3.23s | 6/6, 2.15s | 12/12, 3.34s |
| gpt-5.4-mini | json_tool_calls | 0/6, 2.62s | 1/6, 2.11s | 3/9, 2.00s | 0/9, 2.12s | 3/6, 1.65s | 4/12, 2.16s |
| gpt-6.1-sol | weave | 6/6, 1.53s | 6/6, 1.42s | 9/9, 1.49s | 9/9, 1.83s | 6/6, 1.64s | 12/12, 1.53s |
| gpt-6.1-sol | native_tools | 6/6, 4.17s | 6/6, 4.34s | 9/9, 4.08s | 9/9, 5.00s | 6/6, 3.37s | 12/12, 4.81s |
| gpt-6.1-sol | json_tool_calls | 5/6, 7.71s | 6/6, 5.51s | 3/9, 6.55s | 0/9, 9.53s | 3/6, 4.78s | 5/12, 7.22s |

What this shows:

- **Accuracy:** Weave and native function calling both passed 96/96, all on the first attempt.
- **Latency:** Weave's median latency was 4.4× lower than native on `gpt-5.4-mini` (0.719 s vs 3.176 s) and 2.8× lower on `gpt-6.1-sol` (1.563 s vs 4.332 s). The input-token cost was about the same: native's per-turn prompts are short here, and there are only 4–6 tools.
- **JSON baseline:** it passed 33/96. Most failures were placeholders such as `$1`, `{"ref": 0}`, or `<result of geo>` where a dependent value belonged, plus calls to a nonexistent `print` tool. Some passes came only on a retry: after seeing its own printed output, the model guessed the literal value (for example `geo:Paris`). That is why it has 3/48 and 0/48 first-try passes.
- **Scope:** these are mock tools with deterministic results, two models, and one machine (Linux x86_64, 8 CPUs, Python 3.13.5). They measure round trips and control flow, not open-ended task accuracy.

Weaknesses the eval exposed and fixed:

- **Missing list methods:** the model used `xs.append(v)`, which Weave rejected (`list has no method append`). Weave now rewrites `xs.append(v)` and `xs.extend(ys)` on a name to `xs += ...`, with tests.
- **Gaps in the prompt:** the prompt did not mention `try/except ToolError` or list results, so `price_fallback` failed on its first attempt. The prompt now covers both, plus the builtins, with a loop-plus-try few-shot example.
- **Before and after:** before these fixes, a one-repeat `gpt-5.4-mini` Weave run passed 16/16, but only 13/16 on the first attempt ([`llm-20261108T144107Z.json`](bench/results/llm-20261108T144107Z.json)). After them, Weave passed 48/48 on the first attempt.
- **Harness bugs:** the JSON baseline had been sent Weave few-shot examples, and native mode for `gpt-6.1-sol` returned HTTP 400 on every call. A run made with those bugs is kept in [`bench/results/discarded/`](bench/results/discarded/) and is not reported above.

Spend: the recorded run cost about $0.48 at list prices ($0.75 / $0.075 cached / $4.50 per 1M tokens for `gpt-5.4-mini`, and $2.00 / $0.10 / $10.00 for `gpt-6.1-sol`, checked 2026-11-08). Including the probes and discarded runs, the milestone cost about $1.20.

To run it yourself:

```bash
export OPENAI_API_KEY=...   # or ANTHROPIC_API_KEY, or GEMINI_API_KEY
# optional: OPENAI_MODELS (comma-separated), WEAVE_EVAL_BUDGET (USD, default 2), WEAVE_EVAL_TASKS, WEAVE_EVAL_MODES, ANTHROPIC_MODEL, GEMINI_MODEL, WEAVE_EVAL_REPEATS, WEAVE_EVAL_TOOL_DELAY
python bench/llm_eval.py
```

Re-run the mock suite with `python bench/run_mock.py`. `BENCH_LATENCY_MS` and `BENCH_REPEATS` change the sleep and the number of repeats. `pydantic-monty` is optional; without it the Monty column is recorded as skipped.

### Real MCP servers (`bench/real_tools_eval.py`)

Six tasks against two real MCP servers, `@modelcontextprotocol/server-filesystem`
and `mcp-server-fetch`, proxied through `weave-mcp` `upstreams`. Model
`gpt-5.4-mini`, 3 repeats, up to 3 attempts per run. Native mode gives the model
the same tools as function tools. All rows are measured on one Linux box on
2026-11-08; raw JSON is in `bench/results/real-*.json`.

| run (UTC) | change | Weave passed (first try) | Weave median | native passed | native median | cost |
|---|---|---|---|---|---|---|
| 15:33:44 | M8 baseline | 17/18 (14) | 0.877 s | 18/18 | 2.912 s | $0.0430 |
| 16:26:57 | + `break`/`continue`, `split(sep, maxsplit)` | 16/18 (16) | 0.980 s | 18/18 | 3.751 s | $0.0599 |
| 16:29:45 | + prompt: `for x in text` loops over characters | 16/18 (14) | 1.206 s | 18/18 | 3.575 s | $0.0653 |
| 16:32:39 | + `split(sep=, maxsplit=)` keywords | 15/18 (14) | 1.120 s | 18/18 | 3.500 s | $0.0637 |
| 16:36:28 | + hint when a tool fails on a 1-character argument | 15/18 (14) | 0.935 s | 18/18 | 3.624 s | $0.0635 |

The M9 runs spent $0.2524 in total. Weave did not get better on this suite:
the 15 to 17 out of 18 spread is within run-to-run noise for 18 runs. The
failures are not missing language features any more:

- `count_urgent` (1/3 Weave in the last run, 3/3 native): the model joins a
  bare file name from `fs_list_directory` onto the wrong directory, or loops
  over the newline-separated text that `fs_search_files` returns as if it
  were a list. After the first error it wraps the read in `try/except
  ToolError: pass` and prints 0.
- `example_heading` (2/3 in the last run): one run took the first line of the
  fetched page (`Contents of https://example.com/:`) as the heading.

Native function calling stays correct at 18/18. Weave takes 1 model turn on
the median against 2 to 5 for native. Its per-task median wall time in the last
run ranged from 0.95x of native (`example_heading`, slightly slower) to 6.9x
faster (`write_version`); overall median 0.935 s against 3.624 s.

### Closing the gap with native (M10)

A Weave script has to parse tool outputs that the model has never seen, while
native function calling sees each output before it acts. M10 tried four fixes,
each one measured alone with `WEAVE_EVAL_MODES=weave`. Model `gpt-5.4-mini`,
18 runs per row; first try means passed in one model turn. Every row is a
measured run with its JSON in `bench/results/`.

| run (UTC) | variant | Weave passed (first try) | median wall | mean turns | cost |
|---|---|---|---|---|---|
| 17:11:27 | none (baseline) | 14/18 (13) | 1.318 s | 1.50 | $0.0391 |
| 17:12:30 | (a) examples, first version | 18/18 (8) | 1.604 s | 1.56 | $0.0434 |
| 17:13:13 | (b) structured listings | 17/18 (16) | 1.060 s | 1.17 | $0.0172 |
| 17:13:56 | (c) hybrid `# continue` | 16/18 (16) | 0.871 s | 1.22 | $0.0359 |
| 17:14:44 | (d) caught-error lint | 15/18 (13) | 1.094 s | 1.44 | $0.0382 |
| 17:16:07 | (a) examples, fixed | 17/18 (16) | 1.003 s | 1.17 | $0.0366 |
| 17:17:02 | (a) + (b) | 18/18 (16) | 0.922 s | 1.17 | $0.0384 |
| 17:19:54 | (a) + (b), with native | **18/18 (17)** | 1.020 s | 1.06 | $0.0656 |

The native mode in the last run also passed 18/18, with a median of 3.700 s
and 2.89 mean turns (Weave: 1 median turn, 1.06 mean). The M10 runs cost
$0.3144 in total.

- **(a) Example outputs (kept).** MCP `outputSchema` does not help here.
  `server-filesystem` declares `{content: string}` for every tool, and
  `structuredContent` only wraps the same text. `mcp-server-fetch` declares
  nothing. Instead, `weave-mcp` takes per-upstream `examples`
  (`{"tool": {args}}`), calls each listed read-only tool once at startup, and
  adds the truncated output to the script tool listing. The first version
  printed the sample call with a JSON dict argument, and the model copied that
  form (`fs_read_text_file({"path": ...})`), which failed validation. That is
  why only 8 runs passed on the first try. Writing the sample as a Weave call
  with keyword arguments fixed it. The samples use paths and a page
  (`example.org`) that no task asks about.
- **(b) Structured results (kept).** A per-upstream `results` map
  (`lines` or `json`) turns `list_directory`, `search_files` and
  `list_allowed_directories` into lists of lines, and the listing says
  `returns: list of str`. This removes the "loop over a text listing
  character by character" failure.
- **(c) Hybrid (dropped).** The model could end a script with `# continue`
  to see its output and then write another one. In 18 runs it never did, so the
  result is just the baseline plus noise.
- **(d) Lint for swallowed errors (dropped).** On a wrong answer, the feedback
  listed the tool errors that the script had caught. It did not raise the pass
  rate (15/18), so it was removed.

With 18 runs per row, a difference of one or two passes is within noise. The
combined variant passed 18/18 in both of its runs (36 of 36), against 14 to 16
of 18 without it. Config example:

```json
{"name": "fs", "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "/data"],
 "prefix": "fs_", "results": {"list_directory": "lines"},
 "examples": {"list_directory": {"path": "/data"}}}
```

### Streaming into a live session

`bench/streaming.py` turns OpenAI chat chunks (`choices[0].delta.content`) and Anthropic events (`content_block_delta` / `delta.text`) into text and pushes each piece into `Session.push`. Safe calls start once their statement is complete, before the rest of the script arrives.

A deterministic demo sleeps 12 ms between 4-character chunks and 80 ms inside each `slow` tool. Three repeats, medians, from [`bench/results/stream-20261108T022244Z.json`](bench/results/stream-20261108T022244Z.json):

| Mode | Median time to first tool (s) | Median total (s) |
| --- | ---: | ---: |
| streaming `Session.push` | 0.049319 | 0.166217 |
| `Engine.run` after the same gaps | 0.146334 | 0.227328 |

`bench/prompts.py` is the system prompt and few-shot examples. `bench/llm_eval.py` uses them, plus `repair_message`, on the tasks in `bench/dataset.py`. Weave, an ordered JSON tool-call baseline, and (for OpenAI) native function calling are graded on the same tasks. Without an API key the eval writes a skip record and no pass rate.

### Speculative pre-execution

An optional predictor pre-runs pure and read calls and stores each result under the tool name plus canonical arguments, the tool cache from Nichols et al., [arXiv 2512.15834](https://arxiv.org/abs/2512.15834). Reversible and irreversible calls are rejected. Before a write runs, cached reads whose `reads` footprint overlaps the write's `writes` footprint are dropped and re-run live; footprints default to every resource and can name call arguments, e.g. `reads=["user:{user_id}"]`, so a write to one key drops only that key's cached reads. Scripts also have a wall-clock limit (default 30 s, `set_limits(max_wall_time_ms=...)`). Data-dependent calls are not guessed. One unused prediction per task is included so wasted work is measured. Generation is an 80 ms sleep; the predictor returns after 10 ms; tool sleep is 40 ms. Three repeats, medians, from [`bench/results/spec-20261108T023548Z.json`](bench/results/spec-20261108T023548Z.json):

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
