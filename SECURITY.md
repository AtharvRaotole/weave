# Security policy

Weave runs code written by a language model, so the sandbox boundary matters.

## Supported versions

Weave is experimental. v0.1.0 has confirmed correctness defects documented in its release notes. Audit fixes are currently source-only; downloads have not been rebuilt. Use isolated, read-only fixtures, not production write tools. See docs/audit-status.md.

## Reporting a vulnerability

Please do not open a public issue. Report it privately through
[GitHub private vulnerability reporting](https://github.com/AtharvRaotole/weave/security/advisories/new),
with a script and tool setup that reproduces it. The aim is to reply within 7 days and to credit the report once a fix ships,
unless you ask otherwise.

## What counts

In scope:

- A script that reaches the host without going through a registered tool:
  imports, file or network access, process control, or Python objects.
- A script that gets around the step, value-size, nesting, or wall-clock limits
  and so exhausts the host's memory or CPU.
- An `irreversible` call that starts before the script is complete, or a write
  that runs out of the order the effect rules promise.
- Replay that calls a real tool, or a trace that makes the host do something
  other than return recorded values.
- `weave-mcp` forwarding to an upstream tool that the config did not expose.

Out of scope: what your own host tools do with their arguments. A tool that
deletes files is as dangerous inside Weave as outside it. Declare its effect
honestly and validate its inputs.

The sandbox review and its known gaps are in
[docs/runtime.html](docs/runtime.html#sandbox-and-limits).
