"""Protocol v4 runner: Weave (as shipped) vs native parallel tool calling.

    # offline, no keys: oracle agents (harness mechanics and interface floors only)
    python -m bench.v4.run --mock --suite dev

    # live: spends money; needs BOTH --allow-live and WEAVE_BENCH_LIVE=1
    WEAVE_BENCH_LIVE=1 python -m bench.v4.run --allow-live --provider openai \
        --model gpt-5.4-mini-2026-03-17 --effort low --suite held_out --repeats 3

Each trial gets a fresh simulated backend with injected per-call latency
(default 200 ms x uniform[0.5, 1.5], seeded) and a shared concurrency cap of 8
for both modes. Rows (including every failure) are appended to rows.jsonl as
they finish; summary.json is written at the end. See docs/paper/.
"""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import json
import os
from pathlib import Path
import random
import subprocess
import sys
import threading
import time

from . import stats
from .agent import trial, SYSTEM, MAX_TURNS
from .tasks import SUITES, manifest

ROOT = Path(__file__).resolve().parents[2]


def git(*args):
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True).strip()


def schedule(tasks, modes, repeats, latencies, seed):
    rng = random.Random(seed)
    plan = []
    for rep in range(repeats):
        for t in tasks:
            for lat in latencies:
                ms = list(modes)
                rng.shuffle(ms)
                plan.extend((t, m, rep, lat) for m in ms)
    blocks = [plan[i:i + len(modes)] for i in range(0, len(plan), len(modes))]
    rng.shuffle(blocks)
    return [x for b in blocks for x in b]


def summarize(rows, seed=0):
    out = {}
    groups = sorted({(r["split"], r["latency_ms"]) for r in rows})
    for split, lat in groups:
        g = [r for r in rows if r["split"] == split and r["latency_ms"] == lat]
        key = f"{split}@{lat}ms"
        modes = sorted({r["mode"] for r in g})
        out[key] = {m: stats.describe([r for r in g if r["mode"] == m], seed) for m in modes}
        if {"weave", "native"} <= set(modes):
            pair_rows = [r for r in g if r["mode"] in ("weave", "native")]
            out[key]["paired"] = {
                "wall_s": stats.paired(pair_rows, lambda r: r["wall_s"], seed),
                "total_tokens": stats.paired(pair_rows, lambda r: r["total_tokens"], seed + 1),
                "turns": stats.paired(pair_rows, lambda r: r["turns"], seed + 2),
                "pass_mcnemar": stats.mcnemar_exact(pair_rows),
            }
        out[key]["by_family"] = {
            fam: {m: stats.describe([r for r in g if r["family"] == fam and r["mode"] == m], seed) for m in modes}
            for fam in sorted({r["family"] for r in g})
        }
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--mock", action="store_true", help="offline oracle agents, no network")
    ap.add_argument("--allow-live", action="store_true")
    ap.add_argument("--provider", default="openai")
    ap.add_argument("--model")
    ap.add_argument("--effort", help="reasoning effort (reasoning models)")
    ap.add_argument("--temperature", type=float)
    ap.add_argument("--max-tokens", type=int, default=4096)
    ap.add_argument("--suite", choices=sorted(SUITES), default="dev")
    ap.add_argument("--tasks", help="comma-separated task ids (subset)")
    ap.add_argument("--modes", default="weave,native", help="weave,native[,weave_nopipe]")
    ap.add_argument("--repeats", type=int, default=3)
    ap.add_argument("--latency-ms", default="200", help="comma-separated per-call latency means")
    ap.add_argument("--workers", type=int, default=4, help="trials in flight")
    ap.add_argument("--token-budget", type=int, default=3_000_000, help="stop scheduling after this many tokens")
    ap.add_argument("--request-timeout-s", type=float, default=180)
    ap.add_argument("--retries", type=int, default=4, help="HTTP retries per request (429/5xx/timeouts)")
    ap.add_argument("--seed", type=int, default=2026)
    ap.add_argument("--out", type=Path, default=ROOT / "bench/results/v4")
    ap.add_argument("--allow-dirty", action="store_true", help="dev suite only")
    ap.add_argument("--tag", default="")
    args = ap.parse_args(argv)

    commit, dirty = git("rev-parse", "HEAD"), bool(git("status", "--porcelain", "--untracked-files=no"))
    if not args.mock:
        if not (args.allow_live and os.environ.get("WEAVE_BENCH_LIVE") == "1"):
            raise SystemExit("refusing: live runs spend money; pass --allow-live AND set WEAVE_BENCH_LIVE=1")
        if not args.model:
            raise SystemExit("--model is required")
        if dirty and (not args.allow_dirty or args.suite != "dev"):
            raise SystemExit("refusing: commit first so results pin a clean source commit")
        if args.suite != "dev":
            prereg = ROOT / "docs/paper/preregistration.md"
            if not prereg.exists() or manifest(args.suite)["hash"] not in prereg.read_text():
                raise SystemExit(f"refusing: {args.suite} manifest hash is not in docs/paper/preregistration.md")
    tasks = SUITES[args.suite]
    if args.tasks:
        want = args.tasks.split(",")
        tasks = tuple(t for t in tasks if t.id in want)
        if len(tasks) != len(want):
            raise SystemExit("unknown task id")
    modes = args.modes.split(",")
    latencies = [float(x) for x in args.latency_ms.split(",")]
    plan = schedule(tasks, modes, args.repeats, latencies, args.seed)

    name = args.model or "oracle"
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    out = args.out / f"{stamp}-{args.suite}-{args.provider if not args.mock else 'mock'}-{name.replace('/', '_')}{args.tag}"
    out.mkdir(parents=True, exist_ok=False)
    import weave

    meta = {
        "protocol": "weave-benchmark-protocol-v4", "kind": "mock_oracle" if args.mock else "live",
        "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "source_commit": commit,
        "source_dirty": dirty, "weave_version": weave.__version__, "python": sys.version.split()[0],
        "provider": "mock" if args.mock else args.provider, "model": name, "effort": args.effort,
        "temperature": args.temperature, "max_tokens": args.max_tokens, "suite": args.suite,
        "manifest": manifest(args.suite), "task_subset": [t.id for t in tasks] if args.tasks else None,
        "modes": modes, "repeats": args.repeats, "latency_ms": latencies, "concurrency_cap": 8,
        "max_turns": MAX_TURNS, "system_prompt": SYSTEM, "planned_trials": len(plan), "workers": args.workers,
        "token_budget": args.token_budget, "seed": args.seed,
    }
    (out / "run.json").write_text(json.dumps(meta, indent=2) + "\n")
    rows, lock, spent = [], threading.Lock(), [0]
    rows_file = (out / "rows.jsonl").open("a")

    def client_for(task, mode):
        if args.mock:
            from .mock import OracleClient
            return OracleClient(task, "native" if mode == "native" else "weave")
        from .providers import ChatClient
        return ChatClient(args.provider, args.model, live=True, reasoning_effort=args.effort,
                          temperature=args.temperature, max_tokens=args.max_tokens,
                          timeout_s=args.request_timeout_s, retries=args.retries)

    def go(i, task, mode, rep, lat):
        with lock:
            if spent[0] >= args.token_budget:
                return None
        real_mode = "weave" if mode.startswith("weave") else "native"
        row = trial(task, real_mode, client_for(task, mode), latency_ms=lat, repeat=rep,
                    pipeline_loops=(mode != "weave_nopipe"), seed=hash((task.id, rep, lat)) & 0xFFFF)
        row["mode"] = mode
        row["schedule_index"] = i
        with lock:
            spent[0] += row["total_tokens"]
            rows.append(row)
            rows_file.write(json.dumps(row, sort_keys=True) + "\n")
            rows_file.flush()
            print(f"[{len(rows)}/{len(plan)}] {name} {task.id:<24} {mode:<12} {lat:>5.0f}ms "
                  f"{'PASS' if row['passed'] else 'fail':<4} {row['status']:<14} turns={row['turns']:<2} "
                  f"wall={row['wall_s']:6.1f}s tok={row['total_tokens']:>6} spent={spent[0]}", flush=True)
        return row

    with ThreadPoolExecutor(max_workers=args.workers) as pool:
        futures = [pool.submit(go, i, *p) for i, p in enumerate(plan)]
        for f in as_completed(futures):
            f.result()
    rows_file.close()
    rows.sort(key=lambda r: r["schedule_index"])
    summary = {**meta, "finished_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
               "completed_trials": len(rows), "skipped_for_budget": len(plan) - len(rows),
               "tokens_spent": spent[0], "summary": summarize(rows, args.seed)}
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    for key, block in summary["summary"].items():
        for m in modes:
            if m in block:
                d = block[m]
                print(f"{key:<22} {m:<12} pass {d['passed']}/{d['trials']}  turns {d['mean_turns']:.2f}  "
                      f"wall {d['mean_wall_s']:.2f}s  tokens {d['mean_tokens']:.0f}")
    print("results:", out)


if __name__ == "__main__":
    main()
