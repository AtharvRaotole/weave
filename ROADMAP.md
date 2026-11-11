# Roadmap

Ordered by what the benchmark says matters. Items marked **good first issue** are self-contained, with a clear test.

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

- **good first issue:** make `weave-mcp`'s script tool echo a final bare expression as `WeaveTool` does
  (`python/weave/agent_tool.py::echo_last_expression`), with a test in `crates/weave-mcp`.
- **good first issue:** support `key=` in `sorted`/`min`/`max` when the key is a subscript of the item
  (`key=lambda p: p[1]` is common in model scripts). It could also be accepted as `key="1"`. Today the check suggests a
  loop instead.
- **good first issue:** `bench/v4`: add a `dedupe_writes` family where the same update appears twice in the input,
  so it tests idempotent writes.
- **good first issue:** `bench/v4/charts.py`: per-trial scatter of tokens against backend calls.
- **good first issue:** `docs/language.html`: a table of every static error message with an example and its fix.
- Dict and list comprehensions limited to one `for` and one `if`, desugared to a loop (the scheduler already
  pipelines loops).

## Not planned

- General Python (imports, `def`, classes). The restricted language is what makes the static check, the effect
  rules and exact replay possible.
