# Weave — code and launch audit

**Recommendation: continue as a read-only developer preview; fix the five runtime blockers below before promoting write-capable production use.** There is a useful product here, but the headline guarantees currently exceed the implementation. More features will not compensate for a workflow that repeats an action, changes its answer on replay, or continues after its deadline.

Audited on **2026-11-08**, against [`e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e`](https://github.com/AtharvRaotole/weave/commit/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e), the initial checkout of `main`. The repository was private. Findings and test data use synthetic local fixtures, not production accounts. Runtime code was not changed by this audit. The README and changelog were rewritten in [draft PR #10](https://github.com/AtharvRaotole/weave/pull/10).

## What is already working

The project has a coherent architecture: restricted parsing and static checking, a dependency scheduler, effect ordering, streaming, Python bindings, a stdio MCP proxy, and tool traces. The independent differential oracle is a particularly valuable foundation. The repository also preserves unsuccessful benchmark experiments instead of presenting only a final chart.

Validation on macOS arm64, Rust 1.99.1, CPython 3.13.0:

| Check | Result |
| --- | --- |
| `cargo test --workspace --locked` | **86 passed**, including the default differential run |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Passed |
| `WEAVE_DIFF_SEEDS=5000 cargo test -p weave --test differential --locked` | Passed; 5,000 generated programs, approximately 177 seconds |
| Source installation with `maturin develop --release --locked` | Passed |
| Python adapter suite | Passed without the optional SDK, then all six checks passed with it installed |
| `speedup.py`, `footprints.py`, `hard_deadline.py`, `run_weave_tool.py`, `mcp_client.py` | Passed; agent example used its offline mode |
| Real filesystem MCP example | Passed against temporary data; 17 exposed tools |
| Cargo audit | No known advisories or warnings in 104 locked packages; RustSec database commit `309392b18f6b17022f82c701f300433126901068` |
| New README | Both Python blocks executed, JSON blocks parsed, relative targets existed, whitespace diff clean |
| Basic credential-pattern scan of checkout | No matching private keys/provider tokens; not a complete history or secret audit |

The **new bounded probes still reproduce failures**. This is why passing the existing tests is insufficient for launch. The differential generator does not establish arbitrary Python equivalence, contended-worker safety, or deterministic replay for changing external reads.

The bundle contains the probes and their actual JSON output. No paid model evaluations were rerun. Existing benchmark arithmetic and harness behavior were inspected; the previous model measurements were not independently reproduced. Linux/Windows, every release wheel, Rust 1.97, remote HTTP integrations, and production load were not exercised locally. This is a targeted engineering audit, not a proof that no other defects exist.

## Five runtime blockers

### W01 · P1 · An irreversible write executes after an earlier expression fails

**Confirmed: 12/12 bounded runs.** Register `write()` as `irreversible`, then execute:

```python
try:
    x = 1 / 0
    write()
except:
    print("caught")
```

The result prints `caught`, and the synthetic write also happens. Sequential Python would jump to the handler before reaching the write. A validation or calculation error can therefore coexist with an externally committed action.

**Cause:** the try body is scheduled as a group; write barriers track observable calls but not all preceding computations that may fail. [`exec.rs:797`](https://github.com/AtharvRaotole/weave/blob/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e/crates/weave/src/exec.rs#L797), [`exec.rs:1554`](https://github.com/AtharvRaotole/weave/blob/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e/crates/weave/src/exec.rs#L1554).

**Fix:** give exception regions an ordered failure boundary. A write must not start until every earlier operation whose failure would skip it has succeeded. Keep independent reads concurrent where semantics allow it. Add failure cases to the independent oracle and assert the actual external write log, not only stdout.

**Release gate:** the example above produces zero writes; cover errors in arguments, nested branches, loops, and caught tool failures.

### W02 · P1 · Hard deadlines fail under worker-pool contention

**Confirmed twice.** With one Python worker, three concurrent calls, a 50 ms tool timeout, and a 100 ms run budget, `run()` returned a timeout after approximately **353–360 ms**. A worker nevertheless wrote a synthetic completion marker afterward; the marker only measures worker lifetime. Each probe tool slept for a finite 800 ms; a four-second process-group watchdog bounded the test.

The normal one-call hard-deadline example passes. The defect appears when calls queue for a worker.

**Cause:** `invoke_isolated` calls the potentially blocking `start()` before entering the cancellation/timeout select. Python `Pool.start()` blocks on `idle.get()`. This can occupy the runtime workers responsible for timers and permit queued work to start after cancellation. [`exec.rs:1991`](https://github.com/AtharvRaotole/weave/blob/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e/crates/weave/src/exec.rs#L1991), [`workers.py:113`](https://github.com/AtharvRaotole/weave/blob/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e/python/weave/workers.py#L113).

**Fix:** make worker acquisition cancellation-aware and move blocking acquisition off runtime worker threads. Establish the deadline before queueing; recheck cancellation before dispatch; retain ownership of every active/queued job through shutdown.

**Release gate:** test more calls than workers, simultaneous run/tool deadlines, dropped sessions, and worker replacement. After cancellation settles, no queued call starts and no supposedly killed local worker continues.

### W03 · P1 · Replay can return different output

**Confirmed: 11/12 runs in the final probe.** Two identical read calls sample the monotonic clock; the first deliberately responds later than the second. Replaying their trace swaps the recorded results in most runs. Host-call count stays at two, so replay correctly avoids live calls but does not preserve output.

**Cause:** replay finds the earliest-completed unused event with matching tool and arguments. That does not identify the original call site/iteration, and live scheduling can differ during replay. [`exec.rs:1712`](https://github.com/AtharvRaotole/weave/blob/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e/crates/weave/src/exec.rs#L1712).

**Fix:** assign a stable logical invocation identity, including control-flow and loop occurrence, independent of task completion order. Version the trace format. Exact replay should validate the source/tool manifest; exploratory replay of changed programs should be a separate, explicitly weaker mode.

**Release gate:** identical calls with different results and errors replay the original bindings/stdout across batch, streaming, loops, and reordered completions.

### W04 · P1 · The configured value-size limit is bypassed

**Confirmed.** With `max_value_size=64`, a 4,096-byte string literal succeeds, as does a tool returning 4,096 bytes. The values reach the returned bindings. Small legal values can also accumulate into output without a total-output limit; that latter issue is a missing budget rather than a violation of a documented output cap.

**Cause:** string literals go directly to `ready(...)`; tool/replay results are recorded and returned without the same `checked(...)` path used by many operators. [`exec.rs:1243`](https://github.com/AtharvRaotole/weave/blob/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e/crates/weave/src/exec.rs#L1243), [`exec.rs:1636`](https://github.com/AtharvRaotole/weave/blob/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e/crates/weave/src/exec.rs#L1636), [`exec.rs:1531`](https://github.com/AtharvRaotole/weave/blob/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e/crates/weave/src/exec.rs#L1531).

**Fix:** validate every value ingress, including literals, tool returns, speculative hits, and replay values. Bound source/frame size before parsing, and add total output, trace, in-flight-call, and memory budgets. Python conversion also needs depth/cycle protection before recursion; that code concern was inspected, not crash-tested. Use process isolation as an additional boundary.

**Release gate:** limit-minus-one, exact-limit, and limit-plus-one tests at every ingress; bounded subprocess tests for large/deep values. Do not market the current language restrictions as complete host resource isolation.

### W05 · P1 · Duplicate MCP tool names route to different servers

**Confirmed.** Two upstreams expose `same` without distinct prefixes. A direct MCP call returns server A's result; `weave_run('print(same())')` returns server B's result. Both are listed with the same name.

**Cause:** direct routing takes the first matching upstream; engine registration overwrites the previous registry entry with the later one. [`weave-mcp/lib.rs:275`](https://github.com/AtharvRaotole/weave/blob/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e/crates/weave-mcp/src/lib.rs#L275), [`tools.rs:235`](https://github.com/AtharvRaotole/weave/blob/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e/crates/weave/src/tools.rs#L235).

**Fix:** reject duplicate final names at configuration/discovery time, including collisions with builtins, scripted tools, and `weave_*`. Use one validated routing map for listing, direct calls, and script registration.

**Release gate:** collisions fail startup with both upstream names; accepted configurations route identically through every interface. Unique prefixes are the immediate workaround.

## Benchmark and product-trust findings

### W06 · P1 for launch claims · The headline benchmark is not an equal endpoint comparison

The recorded numbers are arithmetically correct:

| Model | Weave median | Native median | Pass rate in each mode |
| --- | ---: | ---: | ---: |
| gpt-5.4-mini | 0.848856 s | 3.108429 s | 48/48 |
| gpt-6.1-sol | 1.833318 s | 4.923120 s | 48/48 |

However, Weave stops when script stdout passes the grader. Native stops after the model consumes tool results and produces its final answer. Native also executes multiple calls from a turn serially. This measures two valid implementations, but does not isolate the runtime advantage or establish a general 2.7–3.7× agent speedup. [`real_tools_eval.py:281`](https://github.com/AtharvRaotole/weave/blob/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e/bench/real_tools_eval.py#L281), [`real_tools_eval.py:314`](https://github.com/AtharvRaotole/weave/blob/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e/bench/real_tools_eval.py#L314).

The Weave harness also uses a custom script-generation prompt instead of testing the complete public `WeaveTool`/MCP client interaction. Retries, mode order, prompt tuning on the evaluation tasks, small sample size, and unpinned upstream versions limit generalization. Result metadata still lists two servers even where task names cover four.

**Fix before publishing a performance headline:** compare both modes to final answer; allow bounded parallel native calls; include the actual shipped integration; randomize/interleave order; add held-out tasks and unfamiliar tool schemas; report first-pass and eventual success, p50/p95, total tokens, and actual side effects. Pin source/model/upstream versions and record retries/rate-limit time separately. Include negative cases where native is better. Compare current code-execution alternatives too.

**Already corrected in the README:** the measured rows remain, their different completion boundaries are labeled, and the general speedup headline is removed. The historical benchmark document and SVGs still need the same contextualization before reuse elsewhere.

### W07 · P2 design issue · The MCP response defeats selective-output behavior

**Confirmed.** A script reads `AUDIT_SYNTHETIC_PRIVATE_VALUE` and prints only `summary only`. The MCP tool response still contains the entire value in both bindings and trace. Discovery also includes every original upstream tool in addition to the three Weave tools.

This is the current API behavior, not evidence of data being sent to an unauthorized remote service. It does mean a host forwarding the MCP result to a model also forwards intermediate data, and the default tool list is not reduced to a single tool. The Python `WeaveTool` wrapper has different output behavior.

**Fix:** make model-facing responses compact (`stdout`, status, opaque trace ID), with explicitly requested debug output and bounded/redacted host-side traces. Offer a script-only exposure mode and optional deferred discovery. [`weave-mcp/lib.rs:339`](https://github.com/AtharvRaotole/weave/blob/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e/crates/weave-mcp/src/lib.rs#L339), [`weave-mcp/lib.rs:171`](https://github.com/AtharvRaotole/weave/blob/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e/crates/weave-mcp/src/lib.rs#L171).

**Acceptance:** a synthetic secret absent from stdout is absent from the model-facing payload by default. This addresses the intermediate-context problem described in [Anthropic's MCP code-execution article](https://www.anthropic.com/research/code-execution-with-mcp).

## Additional confirmed defects and compatibility gaps

These are P2 unless the affected integration is the primary launch path. They are distinct fixes, not evidence that every deployment encounters every issue.

| ID | Reproduction and observed behavior | Fix / acceptance criterion | Location |
| --- | --- | --- | --- |
| W08 | Local server returns `nextCursor`; its second-page tool is missing and rejected by `weave_check`. | Follow cursors with loop/size bounds; test more than one page. Pagination is part of [MCP tool discovery](https://modelcontextprotocol.io/specification/2025-06-18/server/tools). | [`upstream.rs:193`](https://github.com/AtharvRaotole/weave/blob/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e/crates/weave-mcp/src/upstream.rs#L193) |
| W09 | Discovered `a_read-file` is listed but parsed as subtraction when called from a script. | Create deterministic valid aliases with reverse mapping; reject alias collisions and reserved names; test dots, hyphens, keywords. | [`upstream.rs:285`](https://github.com/AtharvRaotole/weave/blob/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e/crates/weave-mcp/src/upstream.rs#L285) |
| W10 | Upstream returns `structuredContent: {"answer":42}` with empty text content; proxy returns a successful empty string. | Preserve the result envelope, prefer validated structured values for script use, and explicitly handle unsupported content. Do not silently discard image/resource blocks. | [`upstream.rs:229`](https://github.com/AtharvRaotole/weave/blob/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e/crates/weave-mcp/src/upstream.rs#L229) |
| W11 | Python `echo(x="ok")` is rejected, even though the registered function accepts `x`. With `arity=1`, it also reports zero positional arguments. | Set keyword support from the Python signature; validate/bind mixed arguments correctly and exclude injected cancellation parameters. | [`weave-py/lib.rs:76`](https://github.com/AtharvRaotole/weave/blob/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e/crates/weave-py/src/lib.rs#L76), [`check.rs:218`](https://github.com/AtharvRaotole/weave/blob/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e/crates/weave/src/check.rs#L218) |
| W12 | Model arguments `{"source":null}`, `42`, `[]`, or `{}` as source cause `AttributeError` to escape the wrapper. | Validate the decoded object and string type before calling `run`; return the same repairable-error contract for malformed input. | [`agent_tool.py:123`](https://github.com/AtharvRaotole/weave/blob/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e/python/weave/agent_tool.py#L123) |
| W13 | `x=0; try: x=1; fail(); except ToolError: print(x); print(x)` prints `0,0` on separate lines. Python preserves the completed assignment and prints `1,1`. | Track successfully completed bindings within exception regions; do not restart the handler from the pre-try snapshot. Include this in the oracle. | [`exec.rs:767`](https://github.com/AtharvRaotole/weave/blob/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e/crates/weave/src/exec.rs#L767) |
| W14 | The optional Agents SDK adapter blocks an asyncio heartbeat for approximately 167 ms during a 150 ms tool. | Provide a true asynchronous wrapper or offload the blocking run with cancellation propagation; event loop remains responsive under tool latency. | [`agent_tool.py:134`](https://github.com/AtharvRaotole/weave/blob/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e/python/weave/agent_tool.py#L134) |
| W15 | Sending an unpickleable function to a one-slot worker pool fails and leaves zero idle slots. The unsupported function should fail without destroying pool capacity. | On serialization/send failure, recycle or replace the acquired slot in an exception-safe path. Next valid call must work. | [`workers.py:113`](https://github.com/AtharvRaotole/weave/blob/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e/python/weave/workers.py#L113) |
| W16 | `python -m weave typo` exits **0**, with no output. | Dedent the unreachable unknown-command error; CLI typos must exit nonzero and show usage. Rust CLI is a separate path. | [`cli.py:54`](https://github.com/AtharvRaotole/weave/blob/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e/python/weave/cli.py#L54) |
| W17 | `"éx".find("x")` returns **2**; slicing from that index returns empty text. Python's character index is **1**. | Convert UTF-8 byte offsets to character offsets and test multilingual text across find/index/slice operations. | [`exec.rs:2409`](https://github.com/AtharvRaotole/weave/blob/e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e/crates/weave/src/exec.rs#L2409) |

The reports retain a separate compatibility probe showing that a Python `async def` tool is treated as a normal function and its coroutine return cannot be converted. This is a missing Python integration capability, not a claim that Rust async tools are broken.

## Further launch gaps

- **MCP cancellation and responsiveness:** the stdio loop handles a call synchronously. A queued cancellation notification and ping are processed only after it finishes; the synthetic write completed. Cancellation forwarding, concurrent request dispatch, progress, and disconnect cleanup need a defined contract. [MCP cancellation is optional](https://modelcontextprotocol.io/specification/2025-06-18/basic/utilities/cancellation), so this is an integration limitation, not a blanket protocol-violation claim.
- **Authorization:** `irreversible` waits for code completion, not human consent. `reversible` has no undo callback. Read annotations are trusted. These are scheduling labels, not an authorization system; the [MCP schema](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-06-18/schema.mdx) explicitly cautions against trusting annotations from untrusted servers.
- **Retry policy:** retries are effect-agnostic. A timed-out write can complete and then be retried. Require host-declared idempotency and idempotency keys for automatic write retries; represent uncertain completion explicitly. Do not promise exactly-once execution for arbitrary APIs.
- **Diagnostics on failure:** an unhandled failed run returns an error, not a partial run record. Traces lack per-attempt start/end timestamps, queue time, cancellation outcomes, and stable invocation IDs. Those are prerequisites for the most valuable debugging feature.
- **Packaging and first run:** `weave` conflicts with W&B's distribution and import namespace. Choose and verify a distinct distribution/import/CLI identity together. Ship signed/checksummed standalone MCP binaries; the existing Python wheel does not provide `weave-mcp`.
- **Documentation/discovery:** GitHub HTML links display source rather than a hosted docs site. The README now explains local serving, but a public docs URL is still needed. `docs/index.html` has stale release wording; align all docs with current release behavior. GitHub description/homepage were empty at audit time.
- **Release confidence:** add actual installed-wheel smoke tests for supported interpreters/OSes, a minimum-Rust-version job, bounded failure-path tests, and SDK compatibility tests. Existing release artifacts and CI are real strengths; expand what they verify.

## Recommended remediation sequence

1. **Runtime correctness:** W01–W05, plus worker lifecycle W15 and exception state W13. Keep their reproductions as regressions; extend the differential oracle to generate these patterns.
2. **The user-facing integration:** W07–W12, W14, W16–W17, then concurrent MCP request handling and explicit cancellation behavior.
3. **Trustworthy evidence:** replace the benchmark comparison with equivalent final-answer endpoints and parallel baselines; publish failures, uncertainty, and limits alongside the success data.
4. **Distribution and product differentiation:** resolve naming/install friction, host the docs, then build the replay/debugging experience in `RESEARCH_AND_ROADMAP.md`.

A credible release claim is “an experimental runtime for scheduling and inspecting agent tool workflows.” “Safe by construction,” “deterministic replay” without qualifications, and a universal “drop-in MCP” claim should wait for the corresponding acceptance criteria.
