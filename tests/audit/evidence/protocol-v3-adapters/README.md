# Protocol v3 adapter-stage evidence

Local box, Linux x86_64, Rust 1.99.1, synthetic loopback HTTP and owned local MCP
subprocesses only. No provider API calls or spending.

- `adapter-stage-initial-overlapping-build-tests.log`: one failure in
  `test_retry_after_mutations_does_not_repeat_tools_and_includes_wait` when the
  suite ran concurrently with `cargo test`/`cargo build` rebuilding the pinned
  `weave-mcp` binary. Retained, not deleted. Root cause was not proven beyond that
  overlap; the test now reports the failing row status.
- `adapter-stage-retry-repeat.log`: the same retry scenario 40/40 after the build.
- Final sequential gates: 54/54 protocol+adapter tests, 93 Rust tests (including
  the 5000-seed differential), clippy `-D warnings`, eight audit scripts, SDK,
  six examples, CLI check/run, original probe checksums.
