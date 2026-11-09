"""Protocol-v3 LIVE runner: Weave script mode vs native tool calling, OpenAI Responses.

Spends real money. Requires BOTH ``--allow-live`` here (runner opt-in) and the
adapter's ``live_opt_in`` (set only by this entrypoint). The API key is read
from ``OPENAI_API_KEY`` and is never written to results, ledgers or logs.

    python -m bench.protocol_v3.live --allow-live \
        --model gpt-5.4-mini-2026-03-17:low --repeats 3

Every trial: a fresh disposable file/git/memory fixture behind the real
``weave-mcp`` binary (both modes use that same backend), a fresh provider
conversation, equal tool contracts/context, and grading of the provider's
final answer plus the actual on-disk state and ordered write log. Failures stay
in every denominator. See docs/benchmark-protocol-v3.md.
"""

from __future__ import annotations

import argparse
from functools import partial
import json
import math
import os
from pathlib import Path
import random
import statistics
import subprocess
import sys
import time
import uuid

from .accounting import TrialLedger
from .core import Config, run
from .live_tasks import LIVE_V1, live_manifest
from .local_mcp import ROOT, RemoteFixture, execute_shipped, file_hash
from .responses import ResponsesAdapter

ENDPOINT = "https://api.openai.com/v1/responses"


def wilson(k, n, z=1.96):
    if n == 0:
        return None
    p = k / n
    d = 1 + z * z / n
    c = (p + z * z / (2 * n)) / d
    h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return [max(0.0, c - h), min(1.0, c + h)]


def bootstrap_mean(values, seed, reps=4000):
    if not values:
        return None
    rng = random.Random(seed)
    n = len(values)
    means = sorted(sum(rng.choice(values) for _ in range(n)) / n for _ in range(reps))
    return [means[int(0.025 * reps)], means[int(0.975 * reps) - 1]]


def nearest_rank(values, q):
    v = sorted(values)
    return v[max(0, math.ceil(q * len(v)) - 1)] if v else None


def describe(rows, seed):
    n = len(rows)
    passed = sum(r["passed"] for r in rows)
    turns = [r["model_turns"] for r in rows]
    wall = [r["wall_s"] for r in rows]
    toks = [
        r["known_tokens"]["input_tokens"] + r["known_tokens"]["output_tokens"]
        for r in rows
    ]
    return {
        "trials": n,
        "passed": passed,
        "pass_rate": passed / n if n else None,
        "pass_rate_wilson95": wilson(passed, n),
        "first_pass": sum(r["first_pass"] for r in rows),
        "failures": {
            s: sum(r["status"] == s for r in rows)
            for s in sorted({r["status"] for r in rows if not r["passed"]})
        },
        "mean_model_turns": statistics.fmean(turns) if n else None,
        "mean_model_turns_boot95": bootstrap_mean(turns, seed),
        "provider_attempts": sum(len(r["provider_attempts"]) for r in rows),
        "p50_wall_s": statistics.median(wall) if n else None,
        "p95_wall_s": nearest_rank(wall, 0.95),
        "mean_wall_s": statistics.fmean(wall) if n else None,
        "mean_wall_s_boot95": bootstrap_mean(wall, seed + 1),
        "mean_total_tokens": statistics.fmean(toks) if n else None,
        "mean_total_tokens_boot95": bootstrap_mean(toks, seed + 2),
        "known_tokens": {
            k: sum(r["known_tokens"][k] for r in rows)
            for k in ("input_tokens", "output_tokens", "cached_input_tokens")
        },
        "unknown_usage_attempts": sum(r["unknown_usage_attempts"] for r in rows),
        "token_totals_complete": all(r["token_totals_complete"] for r in rows),
        "unexpected_write_runs": sum(r["unexpected_write"] for r in rows),
        "unexpected_write_attempt_runs": sum(
            r["unexpected_write_attempt"] for r in rows
        ),
        "state_mismatch_runs": sum(not r["state_matches"] for r in rows),
        "execution_error_turns": sum(r["execution_errors"] for r in rows),
        "transport_retries": sum(
            a["retry"] > 0 for r in rows for a in r["provider_attempts"]
        ),
        "rate_limit_wait_s": sum(r["rate_limit_wait_s"] for r in rows),
    }


def summarize_live(rows, seed):
    out = {}
    for mode in ("weave", "native"):
        group = [r for r in rows if r["mode"] == mode]
        out[mode] = describe(group, seed)
        out[mode]["by_task"] = {
            t.id: {
                "trials": len(g := [r for r in group if r["task"] == t.id]),
                "passed": sum(r["passed"] for r in g),
                "mean_model_turns": statistics.fmean(r["model_turns"] for r in g)
                if g
                else None,
                "p50_wall_s": statistics.median(r["wall_s"] for r in g) if g else None,
                "statuses": sorted(r["status"] for r in g),
            }
            for t in LIVE_V1
            if any(r["task"] == t.id for r in group)
        }
    return out


def schedule(tasks, repeats, seed):
    """Seeded interleaved schedule; per task/repeat the mode order is randomised."""
    rng = random.Random(seed)
    blocks = []
    for rep in range(repeats):
        for t in tasks:
            modes = ["weave", "native"]
            rng.shuffle(modes)
            blocks.append([(t, m, rep) for m in modes])
    rng.shuffle(blocks)
    return [x for b in blocks for x in b]


def pins():
    import weave._native
    import weave.agent_tool

    binary = ROOT / "target/debug/weave-mcp"
    if not binary.exists():
        raise SystemExit("build weave-mcp first: cargo build -p weave-mcp")
    return {
        "fixture_server": "sha256:"
        + file_hash(ROOT / "bench/protocol_v3/fixture_server.py"),
        "weave_mcp": "sha256:" + file_hash(binary),
        "weave_native": "sha256:" + file_hash(weave._native.__file__),
        "weave_tool": "sha256:" + file_hash(weave.agent_tool.__file__),
    }


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--allow-live", action="store_true", help="runner opt-in (required)")
    ap.add_argument(
        "--model",
        required=True,
        help="pinned snapshot[:reasoning_effort], e.g. gpt-5.4-mini-2026-03-17:low; "
        "omit effort for non-reasoning models (temperature 0 is then sent)",
    )
    ap.add_argument("--repeats", type=int, default=3)
    ap.add_argument("--timeout-s", type=float, default=240)
    ap.add_argument("--request-timeout-s", type=float, default=120)
    ap.add_argument("--max-output-tokens", type=int, default=4096)
    ap.add_argument("--max-requests", type=int, default=400, help="hard cap on HTTP requests for this run")
    ap.add_argument("--seed", type=int, default=73)
    ap.add_argument("--out", type=Path, default=ROOT / "bench/results/protocol-v3-live")
    ap.add_argument("--allow-dirty", action="store_true")
    ap.add_argument("--tasks", help="comma-separated subset (smoke runs only; not the protocol run)")
    args = ap.parse_args(argv)
    if not args.allow_live:
        raise SystemExit("refusing: live runs spend money; pass --allow-live")
    key = os.environ.get("OPENAI_API_KEY")
    if not key:
        raise SystemExit("OPENAI_API_KEY is not set")
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    dirty = bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT, text=True).strip())
    if dirty and not args.allow_dirty:
        raise SystemExit("refusing: commit the harness first so results pin a clean source commit")
    model, _, effort = args.model.partition(":")
    generation = {
        "temperature": None if effort else 0,
        "max_output_tokens": args.max_output_tokens,
        "reasoning": effort or None,
    }
    config = Config(
        provider="openai-responses",
        model_revision=model,
        resolved_model_revision=model,
        api_version="responses-v1",
        source_commit=commit,
        server_versions=pins(),
        generation=generation,
        timeout_s=args.timeout_s,
        seed=args.seed,
    )
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    run_id = f"{stamp}-{model}"
    out = args.out / run_id
    out.mkdir(parents=True, exist_ok=False)
    ledger = TrialLedger(out / "trials.jsonl")
    tasks = LIVE_V1
    if args.tasks:
        wanted = args.tasks.split(",")
        tasks = tuple(t for t in LIVE_V1 if t.id in wanted)
        if len(tasks) != len(wanted):
            raise SystemExit("unknown task id")
    plan = schedule(tasks, args.repeats, args.seed)
    meta = {
        "protocol": "weave-benchmark-protocol-v3",
        "kind": "live_explicit_opt_in",
        "run_id": run_id,
        "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "source_commit": commit,
        "source_dirty": dirty,
        "config": config.report(),
        "task_manifest": live_manifest(),
        "task_subset": [t.id for t in tasks] if args.tasks else None,
        "repeats": args.repeats,
        "planned_trials": len(plan),
        "max_requests": args.max_requests,
        "request_timeout_s": args.request_timeout_s,
        "fixture": "local_mcp.RemoteFixture(path='weave_mcp') for BOTH modes",
        "endpoint": ENDPOINT,
        "python": sys.version.split()[0],
    }
    (out / "run.json").write_text(json.dumps(meta, indent=2) + "\n")
    rows, sent, skipped = [], 0, 0
    with (out / "rows.jsonl").open("a") as rows_file:
        for i, (task, mode, rep) in enumerate(plan):
            remaining = args.max_requests - sent
            if remaining < 1:
                skipped += 1
                continue
            adapter = ResponsesAdapter(
                ENDPOINT,
                live_opt_in=True,
                api_key=key,
                max_live_requests=min(
                    remaining, config.max_turns * (config.provider_retries + 1)
                ),
                request_timeout_s=args.request_timeout_s,
            )
            trial_id = f"{task.id}-{mode}-r{rep}-{uuid.uuid4().hex[:8]}"
            try:
                row = run(
                    task,
                    mode,
                    adapter,
                    config,
                    fixture_factory=partial(RemoteFixture, path="weave_mcp"),
                    execution=execute_shipped,
                    ledger=ledger,
                    trial_id=trial_id,
                    allow_live=True,
                )
            finally:
                sent += adapter.sent
            row.pop("fixture_root", None)
            row.pop("requests", None)  # full request bodies are in trials.jsonl
            row["repeat"] = rep
            row["schedule_index"] = i
            rows.append(row)
            rows_file.write(json.dumps(row, sort_keys=True) + "\n")
            rows_file.flush()
            print(
                f"[{i + 1}/{len(plan)}] {model} {task.id:<18} {mode:<6} "
                f"{row['status']:<22} turns={row['model_turns']} wall={row['wall_s']:.1f}s",
                flush=True,
            )
    summary = {
        **meta,
        "finished_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "completed_trials": len(rows),
        "skipped_for_request_cap": skipped,
        "http_requests_sent": sent,
        "summary": summarize_live(rows, args.seed),
        "rows_sha256": file_hash(out / "rows.jsonl"),
        "ledger_sha256": file_hash(out / "trials.jsonl"),
    }
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps({m: {k: summary["summary"][m][k] for k in ("trials", "passed", "mean_model_turns", "p50_wall_s", "mean_total_tokens")} for m in ("weave", "native")}, indent=2))
    print("results:", out)


if __name__ == "__main__":
    main()
