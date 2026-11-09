# Security policy

Weave runs code written by a language model, so the sandbox boundary matters.

## Supported versions

| Version | Supported |
| --- | --- |
| 1.0.x | Yes: security fixes and correctness fixes for documented guarantees |
| 0.1.0 | No. Historic experimental release with the defects listed in [docs/audit-status.md](docs/audit-status.md); upgrade to 1.0 |

The supported production posture, guarantees and threat model are in
[docs/production.md](docs/production.md).

## Reporting a vulnerability

Please do not open a public issue. Report it privately through
[GitHub private vulnerability reporting](https://github.com/AtharvRaotole/weave/security/advisories/new),
with a script and tool setup that reproduces it. The aim is to reply within 7 days and to credit the report once a fix ships,
unless you ask otherwise.

## What counts

In scope:

- A script that reaches the host without going through a registered tool:
  imports, file or network access, process control, or Python objects.
- A script that gets around the step, value-size, nesting, source, stdout or
  trace budgets and so exhausts the host's memory or CPU.
- An isolated tool that outlives its timeout, run deadline or cancellation, or
  a `WorkerLimits` value that is accepted but not enforced.
- Host exception text, tracebacks, upstream stderr, upstream error payloads or
  host environment variables reaching a script or MCP client without the host
  opting in (`weave.ToolError`, `debug`, `inherit_env`, `stderr`).
- An `irreversible` call that starts before the script is complete, or a write
  that runs out of the order the effect rules promise.
- Replay that calls a real tool, or a trace that makes the host do something
  other than return recorded values.
- `weave-mcp` forwarding to an upstream tool that the config did not expose.

Out of scope: what your own host tools do with their arguments. A tool that
deletes files is as dangerous inside Weave as outside it. Declare its effect
honestly and validate its inputs.

Also out of scope, because they are documented non-guarantees: rollback of
effects already started, soft deadlines of in-process tools, Windows
grandchild processes, and values a tool itself returns or prints.

The script sandbox review is in
[docs/runtime.html](docs/runtime.html#sandbox-and-limits).
