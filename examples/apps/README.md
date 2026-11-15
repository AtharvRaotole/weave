# Demo apps

Two small programs where one Weave script replaces dozens of tool calls. Both run offline by default (CI runs them).

| app | what it does | offline | live |
|---|---|---|---|
| [`dep_audit.py`](dep_audit.py) | For every pinned package in a requirements file: latest release and advisories that affect the pinned version; prints only what needs attention | 12 **synthetic** packages (invented names and `SYNTH-` advisory ids), 80 ms simulated latency per lookup | `--live requirements.txt`: PyPI JSON API and OSV.dev, read-only |
| [`repo_scan.py`](repo_scan.py) | Across many git repositories: current branch, dirty files, stale branches (90 days), most recent committers | 8 throwaway repos created with real `git` in a temp dir | `repo_scan.py ~/src/a ~/src/b ...`: read-only git commands on your repos |

```bash
python examples/apps/dep_audit.py
python examples/apps/dep_audit.py --live requirements.txt
python examples/apps/repo_scan.py
python examples/apps/repo_scan.py ~/src/*/
```

The scripts are what a model would send through `run_weave`: give an agent the same functions with
`weave.WeaveTool([...])` and it can write them itself. The timing line in the offline auditor compares one script with
the same lookups made one by one; it shows concurrency on simulated latency, not a model-in-the-loop benchmark (see
[benchmarks](../../docs/benchmarks.md) for those).
