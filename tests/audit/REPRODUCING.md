# Reproducing the Weave audit

Audit date: 2026-11-08. Runtime baseline: `e1a8be544dd2bd1739b960aaed453fb9fd5dcd6e`.
README-only revision: `9798a2ce434ccb3d723184bd7e92c0e038a0c6bb`, in
[draft PR #10](https://github.com/AtharvRaotole/weave/pull/10).

The probes record current behavior. They are not a passing regression suite and
do not fix the defects. Some intentionally return successfully after observing a
bug; inspect their JSON output. The audit report gives acceptance criteria for
turning them into assertions after a fix.

## Contents

- `WEAVE_AUDIT.md`: findings, evidence, source links, remediation, and limits.
- `RESEARCH_AND_ROADMAP.md`: sourced README comparisons and proposed features.
- `README.md`: the rewritten repository README. Its relative links are intended
  for the repository root; the complete linked files are not copied here.
- `readme-improvements.patch`: the README/changelog change against the baseline.
- `probes/`: local synthetic reproductions; these need the built checkout.
- `evidence/`: actual results captured during this audit, including dependency
  checks, download-count sources, and README validation.
- `logs/`: selected build and test logs. Local workspace paths are replaced
  with `<workspace>`; no credentials or dependency directories are included.
- `MANIFEST.sha256`: integrity hashes for the other bundle files.

## Prepare a checkout

These commands assume macOS/Linux, a Rust installation, and Python 3.13.
The audit ran on macOS arm64 with Rust 1.99.1 and CPython 3.13.0.
Use a disposable checkout at either commit above; the README revision changes
no runtime code. Replace the two absolute paths below with your local paths.
Do not reset a checkout containing your own work.

```bash
WEAVE_REPO=/absolute/path/to/weave
WEAVE_AUDIT=/absolute/path/to/extracted/weave-audit
cd "$WEAVE_REPO"
git rev-parse HEAD
python3.13 -m venv .venv
.venv/bin/python -m pip install "maturin>=1.8,<2"
source .venv/bin/activate
.venv/bin/maturin develop --release --locked
cargo build --locked -p weave-mcp
WEAVE_PYTHON="$WEAVE_REPO/.venv/bin/python"
```

The checkout's `rust-toolchain.toml` pins the toolchain. The package/import name
`weave` also belongs to a different project on PyPI; install this checkout in a
separate virtual environment. Do not substitute `pip install weave`.

## Run the synthetic probes

Run these from the same shell as the setup. Redirect new output to different
files if you want to preserve the audit's original evidence.

```bash
"$WEAVE_PYTHON" "$WEAVE_AUDIT/probes/runtime_probe.py"
"$WEAVE_PYTHON" "$WEAVE_AUDIT/probes/run_deadline_probes.py"
"$WEAVE_PYTHON" "$WEAVE_AUDIT/probes/mcp_probe.py" "$WEAVE_REPO"
"$WEAVE_PYTHON" -m weave typo
```

| Probe | Findings / expected buggy baseline behavior |
| --- | --- |
| `runtime_probe.py` | W01: writes appear after the division error; W03: some replay outputs differ; W04: 4,096-byte values pass a 64-byte limit; W11–W13 and W17: keyword rejection, malformed source errors, lost try bindings, and Unicode index mismatch. Also demonstrates the missing async Python-tool capability. |
| `run_deadline_probes.py` | W02: pool contention exceeds the deadline and a worker may finish afterward; W15: rejected serialization leaves zero idle slots. |
| `mcp_probe.py` + `fake_mcp.py` | W05: direct/script calls route differently; W07: intermediate data remains in the response; W08–W10: missing second-page tools, invalid script identifiers, and dropped structured results. Also records current cancellation responsiveness. |
| `python -m weave typo` | W16: exits zero with no diagnostic. |

The deadline driver is **POSIX-only**: it creates an owned subprocess group per
probe and kills that group if its four-second watchdog expires. Each simulated
tool sleeps for a finite 800 ms. It deletes only `deadline-marker-*` files in
the probe directory. The marker is instrumentation for worker lifetime, not a
production write. MCP probes use local Python subprocesses, eight-second call
timeouts, and synthetic values. They generate `mcp-config.json`,
`cancel-marker`, and deadline markers beside the probe files. They do not
contact external servers or use an API key.

Scheduling and timing vary across machines. Exact replay mismatch counts,
marker names, and durations need not match the saved output. A successful exit
is not proof that the intended deadline or replay contract held.

To reproduce the optional Agents SDK event-loop probe (W14):

```bash
"$WEAVE_PYTHON" -m pip install openai-agents
"$WEAVE_PYTHON" "$WEAVE_AUDIT/probes/async_adapter_probe.py"
```

This calls the adapter locally without a model/API request. The original
result shows an approximately 167 ms heartbeat gap despite a 5 ms interval.
Package versions installed during the audit are recorded in
`evidence/python-environment.json`; future SDK behavior may differ.

## Existing project checks

```bash
cd "$WEAVE_REPO"
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
WEAVE_DIFF_SEEDS=5000 cargo test -p weave --test differential --locked
"$WEAVE_PYTHON" tests/python/test_agent_tool.py
"$WEAVE_PYTHON" examples/speedup.py
"$WEAVE_PYTHON" examples/footprints.py
"$WEAVE_PYTHON" examples/hard_deadline.py
"$WEAVE_PYTHON" examples/run_weave_tool.py
"$WEAVE_PYTHON" examples/mcp_client.py
```

The 5,000-seed differential run took approximately three minutes locally.
The recorded filesystem smoke test additionally used Node.js/npx and
`examples/real_mcp_upstream.py`; it downloads an external MCP server and uses
temporary data. It is separate from the fully local synthetic probes.

The saved RustSec JSON came from cargo-audit 0.22.2 against the database commit
shown in the result. A current scan can produce different advisory results.
The benchmark finding W06 comes from inspecting
`bench/real_tools_eval.py` and the repository's committed result JSONs.
No paid model evaluation was rerun during this audit.

## Review the documentation patch

On a clean checkout of the **baseline** commit:

```bash
git apply --check "$WEAVE_AUDIT/readme-improvements.patch"
git apply "$WEAVE_AUDIT/readme-improvements.patch"
```

Do not apply it again if the README PR is already present. The only tracked
changes in the PR are `README.md` and `CHANGELOG.md`. Runtime findings remain
open; green CI checks do not imply they have been fixed.
