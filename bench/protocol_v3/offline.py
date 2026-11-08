"""Run deterministic DEVELOPMENT mechanics only; never access a provider API."""

import argparse
import json
from pathlib import Path
import subprocess
from .core import Config, digest, run, summarize
from .fakes import ManualClock, ScriptedProvider
from .tasks import DEVELOPMENT, validate_splits


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    commit = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=root, text=True
    ).strip()
    dirty = bool(
        subprocess.check_output(
            ["git", "status", "--porcelain"], cwd=root, text=True
        ).strip()
    )
    config = Config(
        provider="deterministic-fake",
        model_revision="fake-revision-001",
        resolved_model_revision="fake-revision-001",
        api_version="offline-v1",
        source_commit=commit,
        server_versions={"local-filesystem-fixture": "protocol-v3.1"},
        generation={"temperature": 0, "max_output_tokens": 256, "reasoning": "none"},
    )
    # Hand-authored decisions deliberately exercise the harness. These answers
    # are cassette contents, NOT demonstrations or strings injected into prompts.
    cassettes = {
        "dev_sum": {
            "weave": [
                {
                    "script": 'a=read_file(path="a.txt")\nb=read_file(path="b.txt")\nprint(int(a)+int(b))\n'
                },
                {"answer": "31"},
            ],
            "native": [
                {
                    "calls": [
                        {"name": "read_file", "arguments": {"path": "a.txt"}},
                        {"name": "read_file", "arguments": {"path": "b.txt"}},
                    ]
                },
                {"answer": "31"},
            ],
        },
        "dev_fallback": {
            "weave": [
                {
                    "script": 'try:\n    x=read_file(path="missing.txt")\nexcept ToolError:\n    x=read_file(path="backup.txt")\n'
                },
                {"answer": "safe-backup"},
            ],
            "native": [
                {
                    "calls": [
                        {"name": "read_file", "arguments": {"path": "missing.txt"}}
                    ]
                },
                {"calls": [{"name": "read_file", "arguments": {"path": "backup.txt"}}]},
                {"answer": "safe-backup"},
            ],
        },
        "dev_receipt": {
            "weave": [
                {
                    "script": 'x=read_file(path="item.txt")\nwrite_file(path="receipt.txt",text="RECEIVED "+x)\n'
                },
                {"answer": "recorded"},
            ],
            "native": [
                {"calls": [{"name": "read_file", "arguments": {"path": "item.txt"}}]},
                {
                    "calls": [
                        {
                            "name": "write_file",
                            "arguments": {
                                "path": "receipt.txt",
                                "text": "RECEIVED blue",
                            },
                        }
                    ]
                },
                {"answer": "recorded"},
            ],
        },
    }
    rows = []
    for i, task in enumerate(DEVELOPMENT):
        for mode in ("native", "weave") if i % 2 else ("weave", "native"):
            clock = ManualClock()
            provider = ScriptedProvider(cassettes[task.id][mode], clock)
            row = run(task, mode, provider, config, clock=clock)
            # Root paths are useful during tests but not portable evidence.
            row.pop("fixture_root")
            rows.append(row)
    payload = {
        "kind": "offline_mechanics",
        "measured_model_performance": False,
        "synthetic_clock": True,
        "response_origin": "hand-authored scripted decisions, not recorded live model responses",
        "source_dirty": dirty,
        "protocol_sources_hash": digest(
            {p.name: p.read_text() for p in sorted(Path(__file__).parent.glob("*.py"))}
        ),
        "split_manifest": validate_splits(),
        "summary": summarize(rows),
        "rows": rows,
        "interpretation": "Harness mechanics only. No model success-rate, speedup, cost or generalization evidence. Corrected live runs pending.",
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(payload, indent=2) + "\n")
    print(
        "Offline mechanics only:",
        len(rows),
        "scripted development rows; corrected live evaluation pending",
    )
    return 0 if all(r["passed"] for r in rows) else 1


if __name__ == "__main__":
    raise SystemExit(main())
