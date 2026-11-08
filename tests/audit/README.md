# Audit evidence and acceptance fixtures

The original bundle was safely extracted (no absolute/traversal/symlink members)
from `weave-audit-bundle.zip`, SHA256
`4c083e608462609c5d0af81be4552a189ba309fffc568e167d344954c1f63b60`.

`original-probes/*.py` are byte-for-byte retained, not changed to make tests pass.
They record observations, not assertions. They may create synthetic markers and
config files beside themselves. The baseline was independently run against
main `e1a8be5`, Linux x86_64, CPython 3.13, Rust 1.99.1. The supplied audit ran
on macOS; its timings are not substituted for this box's measurements.

`evidence/baseline-*` and `evidence/fixed-*` retain independent outputs.
`test_*.py` add acceptance assertions and are offline: no model requests or
arbitrary server sampling. The MCP tests use the retained local fake server.
Run them with the source-built extension and `cargo build -p weave-mcp`.
`test_workers.py` must run under a bounded watchdog (CI uses `timeout 35`).
The SDK adapter test requires the optional `openai-agents` package.

PR10's README and changelog improvements were reviewed and integrated as source
changes rather than cherry-picking its author metadata. The audit reports here
are the owner's supplied work, retained as evidence. All new commits obey the
required Atharv author/committer identity, without attribution trailers.

The verification ledger is [`docs/audit-status.md`](../../docs/audit-status.md).
