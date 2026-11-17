# Roadmap

Ordered by what the benchmark says matters. Items marked **good first issue** are self-contained, with a clear test;
they are also filed under the [`good first issue`](https://github.com/AtharvRaotole/weave/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22)
label.

## Measured weaknesses

1. **Footprint-aware read/write scheduling.** In a loop that writes, every read waits for the previous write, so
   Weave's backend time on the `triage` family is about 2× native's ([paper §5.3](docs/paper/weave.md)). Tools
   already declare `reads`/`writes` footprints, which are currently used only for speculation. Gate a read on the
   writes whose footprint overlaps its own. Done when the `triage` oracle's exec time in `bench/v4` matches native
   and the replay tests still pass.
2. **More providers.** Run the pre-registered `confirm` recipe on Anthropic, Gemini (paid tier) and an open-weights
   model. The adapters exist in `bench/v4/providers.py`.
3. **Real backends with latency.** Protocol v3 runs real MCP servers at near-zero latency, and v4 runs injected
   latency against a simulator. Add a suite of real HTTP APIs, recorded and replayed with realistic latency.

## Good first issues

Tracked as GitHub issues. A few that are still open:

- [#40](https://github.com/AtharvRaotole/weave/issues/40) — make `weave-mcp`'s script tool echo a final bare expression as `WeaveTool` does (`python/weave/agent_tool.py::echo_last_expression`).
- [#41](https://github.com/AtharvRaotole/weave/issues/41) — `docs/language.html`: a table of every static error message with an example and its fix.
- [#42](https://github.com/AtharvRaotole/weave/issues/42) — `bench/v4/charts.py`: per-trial scatter of tokens against backend calls.
- [#43](https://github.com/AtharvRaotole/weave/issues/43) — `bench/v4`: add a `dedupe_writes` family where the same update appears twice in the input, so it tests idempotent writes.
- [#44](https://github.com/AtharvRaotole/weave/issues/44) — official CPython 3.14 wheels (`help wanted`).
- [#45](https://github.com/AtharvRaotole/weave/issues/45) — `weave mcp-config` client paths for VS Code and Claude Code.
- [#46](https://github.com/AtharvRaotole/weave/issues/46) — an Anthropic Messages API adapter on `WeaveTool`.
- [#47](https://github.com/AtharvRaotole/weave/issues/47) — `str.startswith` / `str.endswith` should accept a list of prefixes or suffixes.

Shipped since this list was first written, so they are no longer open tasks: `key=` on `sorted` / `min` / `max` (including `key=lambda p: p[1]`), and nested `for` / `if` in comprehensions.

## Not planned

- General Python (imports, classes, nested or recursive `def`). Top-level helpers already exist. The restricted language is what makes the static check, the effect
  rules and exact replay possible.
