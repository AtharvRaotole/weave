"""BFCL multi-turn (multi_turn_base): BFCL's own OpenAI function-calling handler against a Weave arm.

    WEAVE_BENCH_LIVE=1 python -m bench.bfcl.run --allow-live --model gpt-4.1-2025-04-14 --tasks @dev6

The tasks, function docs, backends and grader are BFCL's (`bfcl-eval`, Apache-2.0), unmodified. The native arm is
`OpenAICompletionsHandler.inference_multi_turn_FC`. The Weave arm (`weave_arm.py`) runs the same loop with one
`run_weave` tool. Both arms send the same request settings: BFCL's temperature (0.001) or, for reasoning models,
`reasoning_effort` and no temperature; OpenAI's default `parallel_tool_calls` (on); `store: false`. Both arms use
the handler's `generate_with_backoff` (the same rate-limit retries). Each trial is graded with BFCL's
`_evaluate_single_multi_turn_entry`. Token use (input, cached input, output, reasoning) is metered on every request
and priced at OpenAI list prices; a trial starts only if the dollars spent plus a reserve for every trial in flight
fit the run's cap.
"""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
import random
import subprocess
import threading
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BFCL_VERSION = "2026.3.23"
CATEGORY = "multi_turn_base"  # default; --category picks another multi-turn category (tasks and grader unchanged)
# $ per 1M tokens: (uncached input, output, cached input).
PRICES = {"gpt-4.1": (2.00, 8.00, 0.50), "gpt-5.4-mini": (0.75, 4.50, 0.075)}


def usd(model: str, t: dict) -> float:
    i, o, c = PRICES["gpt-5.4-mini" if "mini" in model else "gpt-4.1"]
    return ((t["input"] - t["cached"]) * i + t["cached"] * c + t["output"] * o) / 1e6


def manifest(ids):
    from bfcl_eval.utils import load_dataset_entry, load_ground_truth_entry
    entries = {e["id"]: e for e in load_dataset_entry(CATEGORY)}
    gts = {g["id"]: g for g in load_ground_truth_entry(CATEGORY)}
    blob = json.dumps([[entries[f"{CATEGORY}_{i}"]["question"], entries[f"{CATEGORY}_{i}"]["initial_config"],
                        gts[f"{CATEGORY}_{i}"]["ground_truth"]] for i in ids], sort_keys=True)
    return {"category": CATEGORY, "ids": list(ids), "bfcl_eval": BFCL_VERSION,
            "hash": hashlib.sha256(blob.encode()).hexdigest()[:16]}


def endpoint_client(args):
    """An OpenAI-compatible client for `--base-url` (e.g. an Azure AI Foundry `/openai/v1` endpoint), or None for
    BFCL's default client. Both arms get the same client settings; the key is read from the named env variable."""
    if not args.base_url:
        return None
    from openai import OpenAI
    key = os.environ.get(args.api_key_env)
    if not key:
        raise SystemExit(f"--base-url needs ${args.api_key_env}")
    return OpenAI(base_url=args.base_url, api_key=key, max_retries=2)


def metered(handler, meter, effort, cache_key=None, deployment=None):
    """Patch the handler's client so every request is metered and carries the same settings in both arms."""
    real = handler.client.chat.completions.create

    def create(**kw):
        if deployment:
            kw["model"] = deployment  # the endpoint's name for the same model version (recorded per response)
        if effort:
            kw.pop("temperature", None)
            kw["reasoning_effort"] = effort
        if cache_key:
            kw["prompt_cache_key"] = cache_key
        t0 = time.time()
        res = real(**kw)
        meter["llm_s"] += time.time() - t0
        meter["calls"] += 1
        u = res.usage
        meter["input"] += u.prompt_tokens or 0
        meter["output"] += u.completion_tokens or 0
        pd, cd = getattr(u, "prompt_tokens_details", None), getattr(u, "completion_tokens_details", None)
        meter["cached"] += (getattr(pd, "cached_tokens", 0) or 0) if pd else 0
        meter["reasoning"] += (getattr(cd, "reasoning_tokens", 0) or 0) if cd else 0
        served = getattr(res, "model", None)
        if served and served not in meter.setdefault("served_models", []):
            meter["served_models"].append(served)
        return res

    handler.client.chat.completions.create = create


def one_trial(args, arm, entry, gt, repeat):
    from bfcl_eval.eval_checker.eval_runner import _evaluate_single_multi_turn_entry
    from bfcl_eval.model_handler.api_inference.openai_completion import OpenAICompletionsHandler

    from bench.bfcl.weave_arm import inference_multi_turn_weave

    entry = copy.deepcopy(entry)
    handler = OpenAICompletionsHandler(args.model, args.temperature, args.model + "-FC", True)
    tag = f"{handler.model_name_underline_replaced}_{arm}_r{repeat}"
    handler.model_name_underline_replaced = tag  # instance namespace for this arm's backends
    client = endpoint_client(args)
    if client is not None:
        handler.client = client
    meter = {"calls": 0, "input": 0, "output": 0, "cached": 0, "reasoning": 0, "llm_s": 0.0}
    metered(handler, meter, args.effort, f"{tag}-{entry['id']}" if args.cache_key else None, args.deployment)
    t0 = time.time()
    error, extra, result = None, {}, None
    try:
        if arm == "native":
            result, meta = handler.inference_multi_turn_FC(entry, False, True)
        else:
            result, extra = inference_multi_turn_weave(
                handler, entry, tag, lambda **kw: handler.generate_with_backoff(**kw)[0], args.weave_config)
    except Exception as e:  # noqa: BLE001 - a provider or harness error fails the trial
        error = f"{type(e).__name__}: {e}"[:500]
    wall = time.time() - t0
    messages = extra.pop("messages", None)
    if result is not None:
        graded = _evaluate_single_multi_turn_entry(handler, entry["id"], result, gt["ground_truth"],
                                                   copy.deepcopy(entry), tag, CATEGORY)
    else:
        graded = {"valid": False, "error": {"error_type": "harness:exception"}}
    tool_calls = sum(len(step) for turn in (result or []) for step in turn if isinstance(step, list))
    toks = {k: meter[k] for k in ("input", "output", "cached", "reasoning")}
    return {
        "id": entry["id"], "task_index": int(entry["id"].rsplit("_", 1)[1]), "arm": arm, "repeat": repeat,
        "model": args.model, "effort": args.effort, "temperature": None if args.effort else args.temperature,
        "passed": bool(graded["valid"]), "error_type": (graded.get("error") or {}).get("error_type"),
        "error": error, "agent_calls": meter["calls"], "turns": len(entry["question"]),
        "tool_calls": tool_calls, "tokens": toks, "total_tokens": toks["input"] + toks["output"],
        "usd": usd(args.model, toks), "wall_s": wall, "llm_s": meter["llm_s"],
        "weave": extra.get("weave"), "force_quit": extra.get("force_quit"),
        "weave_config": args.weave_config if arm == "weave" else None, "cache_key": bool(args.cache_key),
        "endpoint": args.base_url.split("/")[2] if args.base_url else "api.openai.com",
        "served_models": meter.get("served_models", []),
    }, {"result": result, "messages": json.loads(json.dumps(messages, default=lambda o: getattr(o, "model_dump", str)()))
        if messages is not None else None}


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--allow-live", action="store_true")
    ap.add_argument("--model", required=True)
    ap.add_argument("--effort")
    ap.add_argument("--temperature", type=float, default=0.001)
    ap.add_argument("--category", default="multi_turn_base",
                    choices=["multi_turn_base", "multi_turn_miss_func", "multi_turn_miss_param", "multi_turn_long_context"])
    ap.add_argument("--base-url", default=os.environ.get("WEAVE_BENCH_BASE_URL"),
                    help="OpenAI-compatible endpoint for both arms (default: $WEAVE_BENCH_BASE_URL, else api.openai.com)")
    ap.add_argument("--api-key-env", default="AZURE_OPENAI_API_KEY", help="env variable holding the key for --base-url")
    ap.add_argument("--deployment", help="model name the endpoint expects (e.g. an Azure deployment); --model stays "
                                         "the model version recorded and priced")
    ap.add_argument("--tasks", required=True, help="comma-separated indices, or @name of a frozen subset")
    ap.add_argument("--arms", default="weave,native")
    ap.add_argument("--repeats", type=int, default=1)
    ap.add_argument("--workers", type=int, default=4)
    ap.add_argument("--usd-budget", type=float, required=True)
    ap.add_argument("--reserve-usd", type=float, default=0.25, help="$ reserved for each trial in flight")
    ap.add_argument("--out", type=Path, default=ROOT / "bench/results/v13")
    ap.add_argument("--tag", default="")
    ap.add_argument("--seed", type=int, default=2026)
    ap.add_argument("--weave-config", default="doc",
                    help="comma list: compact, reply, learned|doc|noshapes (round 8 = doc)")
    ap.add_argument("--cache-key", action="store_true",
                    help="send prompt_cache_key=<arm tag>-<task id> in BOTH arms (routing hint for prompt caching)")
    args = ap.parse_args(argv)
    global CATEGORY
    CATEGORY = args.category
    if not (args.allow_live and os.environ.get("WEAVE_BENCH_LIVE") == "1"):
        raise SystemExit("refusing: pass --allow-live AND set WEAVE_BENCH_LIVE=1")
    from bfcl_eval.utils import load_dataset_entry, load_ground_truth_entry
    import bfcl_eval
    version = getattr(bfcl_eval, "__version__", None)
    from importlib.metadata import version as dist_version
    if dist_version("bfcl-eval") != BFCL_VERSION:
        raise SystemExit(f"refusing: bfcl-eval {dist_version('bfcl-eval')} installed, expected {BFCL_VERSION}")
    split = "adhoc"
    if args.tasks.startswith("@"):
        from bench.bfcl.subsets import SUBSETS
        split, ids = SUBSETS[args.tasks[1:]]
    else:
        ids = [int(x) for x in args.tasks.split(",")]
    man = manifest(ids)
    commit = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True).stdout.strip()
    dirty = bool(subprocess.run(["git", "status", "--porcelain", "--untracked-files=no"], cwd=ROOT,
                                capture_output=True, text=True).stdout.strip())
    if split != "dev":
        if dirty:
            raise SystemExit("refusing: commit first")
        if man["hash"] not in (ROOT / "docs/paper/preregistration.md").read_text():
            raise SystemExit(f"refusing: manifest {man['hash']} is not pre-registered")
    entries = {e["id"]: e for e in load_dataset_entry(CATEGORY)}
    gts = {g["id"]: g for g in load_ground_truth_entry(CATEGORY)}
    arms = args.arms.split(",")
    rng = random.Random(args.seed)
    units = [(i, r) for r in range(args.repeats) for i in ids]
    rng.shuffle(units)
    plan = []
    for i, r in units:
        pair = list(arms)
        rng.shuffle(pair)
        plan.extend((arm, i, r) for arm in pair)
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    out = args.out / f"{stamp}-bfcl-{CATEGORY}-{split}-{args.model}{args.tag}"
    out.mkdir(parents=True)
    meta = {"started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "source_commit": commit,
            "source_dirty": dirty, "split": split, "manifest": man, "model": args.model, "effort": args.effort,
            "temperature": None if args.effort else args.temperature, "arms": arms, "repeats": args.repeats,
            "planned": len(plan), "usd_budget": args.usd_budget, "reserve_usd": args.reserve_usd, "seed": args.seed,
            "handler": "bfcl_eval OpenAICompletionsHandler (FC)", "weave_config": args.weave_config,
            "cache_key": bool(args.cache_key),
            "endpoint": (args.base_url.split("/")[2] if args.base_url else "api.openai.com"),
            "deployment": args.deployment}
    (out / "run.json").write_text(json.dumps(meta, indent=2) + "\n")
    rows_f, traces_f = (out / "rows.jsonl").open("a"), (out / "traces.jsonl").open("a")
    room = threading.Condition()
    spent, in_flight, done, skipped = [0.0], [0], [0], []

    def go(arm, i, r):
        with room:
            while spent[0] + (in_flight[0] + 1) * args.reserve_usd > args.usd_budget:
                if in_flight[0] == 0:
                    skipped.append((arm, i, r))
                    return
                room.wait()
            in_flight[0] += 1
        try:
            row, trace = one_trial(args, arm, entries[f"{CATEGORY}_{i}"], gts[f"{CATEGORY}_{i}"], r)
        finally:
            with room:
                in_flight[0] -= 1
                room.notify_all()
        with room:
            spent[0] += row["usd"]
            done[0] += 1
            rows_f.write(json.dumps(row, sort_keys=True, default=str) + "\n")
            rows_f.flush()
            traces_f.write(json.dumps({"id": row["id"], "arm": arm, "repeat": r, **trace}, default=str) + "\n")
            traces_f.flush()
            print(f"[{done[0]}/{len(plan)}] {args.model} {row['id']:<20} {arm:<6} pass={row['passed']!s:<5} "
                  f"calls={row['agent_calls']:<3} wall={row['wall_s']:6.1f}s ${row['usd']:.4f} spent=${spent[0]:.4f}"
                  f"{' ERR ' + row['error'] if row['error'] else ''}", flush=True)

    with ThreadPoolExecutor(max_workers=args.workers) as pool:
        for f in as_completed([pool.submit(go, *p) for p in plan]):
            f.result()
    rows_f.close()
    traces_f.close()
    if skipped:
        meta["not_run_budget"] = [{"arm": a, "task_index": i, "repeat": r} for a, i, r in skipped]
        (out / "run.json").write_text(json.dumps(meta, indent=2) + "\n")
    print(f"results: {out} (spent ${spent[0]:.4f}; {len(skipped)} trials not run for budget)")


if __name__ == "__main__":
    main()
