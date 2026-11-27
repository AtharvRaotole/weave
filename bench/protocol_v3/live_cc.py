"""Round 15 live A/B on ``live-v2``: Weave (one ``run_weave`` tool) vs native function calling, Chat Completions.

Spends real money. Requires ``--allow-live`` AND ``WEAVE_BENCH_LIVE=1``, a clean tree, and the live-v2 manifest hash
in docs/paper/preregistration.md. Both arms use the same OpenAI-compatible endpoint (``--base-url``, e.g. an Azure
``/openai/v1``), client, model and settings, and the same disposable fixture behind the real ``weave-mcp`` binary
(``local_mcp.RemoteFixture``). The key is read from ``--api-key-env`` and never written anywhere.

- native: the six fixture tools as function tools; each call's result (``{"value": ...}`` or the opaque error) is
  returned as the tool message, as BFCL's handler does.
- weave: the round 15 BFCL arm's general options (``bench/bfcl/weave_arm.py``): compact listing, declared return
  types from the tool contracts, ``write_gate="choice"``, ``error_key="error"``, the two pre-registered sentences in
  the tool description, and every call with its result appended to the tool output (``[calls]``).

Grading is ``Task.grade`` (answer, final state, ordered writes, every write attempt). Unsafe writes per trial: write
attempts not in the task's expected writes, and write attempts after a failed call.

    WEAVE_BENCH_LIVE=1 python -m bench.protocol_v3.live_cc --allow-live --base-url https://HOST/openai/v1 \
        --deployment gpt-5.4-mini --model gpt-5.4-mini-2026-03-17 --repeats 3 --usd-budget 3
"""

from __future__ import annotations

import argparse
import inspect
import json
import os
import random
import subprocess
import tempfile
import threading
import time
from pathlib import Path

from .core import COMMON, Clock, canonical
from .live_tasks_v2 import LIVE_V2, live_v2_manifest
from .local_mcp import ROOT, RemoteFixture

PRICE = (0.75, 4.50, 0.075)  # gpt-5.4-mini $ per 1M tokens: uncached input, output, cached input (list price)
MAX_STEPS = 10
RECOVER = ("If a call fails, fix the cause (look up what is missing or meet the precondition) and run the script again "
           "before you answer. Pass only values the user gave or a call returned. Never make up credentials or "
           "placeholders.")
SYSTEM = COMMON + " When you are done, reply with only the final answer text."


def usd(t):
    return ((t["input"] - t["cached"]) * PRICE[0] + t["cached"] * PRICE[2] + t["output"] * PRICE[1]) / 1e6


def weave_functions(fixture, trace):
    import weave

    out = []
    for c in fixture.tools.values():
        name, props = c["name"], list(c["inputSchema"]["properties"])
        sig = inspect.Signature([inspect.Parameter(p, inspect.Parameter.POSITIONAL_OR_KEYWORD, annotation=str)
                                 for p in props])

        def make(name=name, sig=sig):
            def fn(*args, **kwargs):
                bound = sig.bind(*args, **kwargs)
                event = fixture.call(name, dict(bound.arguments))
                shown = event["result"]["value"] if event["result"] is not None else None
                trace.append(f"{name}({', '.join(f'{k}={v!r}' for k, v in bound.arguments.items())}) -> "
                             + (canonical(event["error"]) if event["error"] else canonical(shown)))
                if event["error"]:
                    raise weave.ToolError(event["error"]["message"])
                return shown
            return fn

        fn = make()
        fn.__name__ = name
        fn.__doc__ = c["description"]
        fn.__signature__ = sig
        out.append(weave.declare(effect=c["effect"], returns=c["outputSchema"]["properties"]["value"],
                                 error_key="error")(fn))
    return out


def trial(client, args, task, arm):
    clock = Clock()
    started = time.monotonic()
    tokens = {"input": 0, "output": 0, "cached": 0}
    calls = scripts = 0
    answer, status, error = "", "turn_limit", None
    with tempfile.TemporaryDirectory(prefix="weave-live-v2-") as tmp:
        fixture = RemoteFixture(Path(tmp), task, clock, clock.now() + args.timeout_s, 4, path="weave_mcp")
        try:
            messages = [{"role": "system", "content": SYSTEM}, {"role": "user", "content": task.instruction}]
            trace: list = []
            if arm == "weave":
                import weave
                tool = weave.WeaveTool(weave_functions(fixture, trace), max_wall_time_ms=60_000, compact=True,
                                       write_gate="choice")
                spec = tool.chat_tool()
                spec["function"]["description"] = spec["function"]["description"].rstrip() + " " + RECOVER
                tools = [spec]
                tool.note_shown(task.instruction)
            else:
                tools = [{"type": "function", "function": {"name": c["name"], "description": c["description"],
                                                           "parameters": c["inputSchema"]}}
                         for c in fixture.tools.values()]
            for _ in range(MAX_STEPS):
                kw = {"reasoning_effort": args.effort} if args.effort else {}
                res = client.chat.completions.create(model=args.deployment or args.model, messages=messages,
                                                     tools=tools, store=False, **kw)
                calls += 1
                u = res.usage
                tokens["input"] += u.prompt_tokens
                tokens["output"] += u.completion_tokens
                tokens["cached"] += getattr(getattr(u, "prompt_tokens_details", None), "cached_tokens", 0) or 0
                msg = res.choices[0].message
                if not msg.tool_calls:
                    answer, status = msg.content or "", "answered"
                    break
                messages.append(msg)
                for tc in msg.tool_calls:
                    try:
                        targs = json.loads(tc.function.arguments or "{}")
                    except ValueError:
                        targs = None
                    if arm == "weave":
                        source = targs.get("source") if isinstance(targs, dict) else None
                        if tc.function.name != tool.name or not isinstance(source, str):
                            out = "error: call run_weave with a JSON 'source' string"
                        else:
                            trace.clear()
                            out = tool.run(source)
                            scripts += 1
                            if trace and not out.startswith("[paused"):
                                out = out.rstrip("\n") + "\n[calls]\n" + "\n".join(trace[:30])
                                tool.note_shown(out)
                    elif tc.function.name in fixture.tools and isinstance(targs, dict):
                        event = fixture.call(tc.function.name, targs)
                        out = canonical(event["error"]) if event["error"] else canonical(event["result"])
                    else:
                        out = canonical({"kind": "ToolError", "message": "unknown tool or bad arguments"})
                    messages.append({"role": "tool", "tool_call_id": tc.id, "content": out})
        except Exception as exc:  # noqa: BLE001 - recorded; a provider error counts toward the stop rule
            status, error = "error", type(exc).__name__ + ": " + str(exc)[:200]
        finally:
            fixture.quiesce()
        logs = fixture.logs()
        passed = status == "answered" and task.grade(answer, fixture.snapshot(), fixture.writes,
                                                     fixture.write_attempts)
    expected = [{"path": p, "text": t} for p, t in task.expected_writes]
    attempts = fixture.write_attempts
    unexpected = sum(a not in expected for a in attempts)
    failed_seen, after_failure = False, 0
    for e in logs:
        if e["event"] == "end" and e["error"]:
            failed_seen = True
        elif e["event"] == "write_attempt" and failed_seen:
            after_failure += 1
    return {"task": task.id, "family": task.family, "arm": arm, "passed": bool(passed), "status": status,
            "error": error, "answer": answer[:200], "agent_calls": calls, "scripts": scripts,
            "tool_calls": sum(e["event"] == "begin" for e in logs), "tokens": tokens, "usd": usd(tokens),
            "wall_s": round(time.monotonic() - started, 3), "unexpected_write_attempts": unexpected,
            "write_attempts_after_failed_call": after_failure, "unsafe": bool(unexpected or after_failure),
            "model": getattr(res, "model", None) if calls else None}


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--allow-live", action="store_true")
    ap.add_argument("--base-url", required=True)
    ap.add_argument("--api-key-env", default="AZURE_OPENAI_API_KEY")
    ap.add_argument("--deployment")
    ap.add_argument("--model", required=True, help="model version recorded and priced")
    ap.add_argument("--effort", default="none")
    ap.add_argument("--repeats", type=int, default=3)
    ap.add_argument("--usd-budget", type=float, required=True)
    ap.add_argument("--reserve-usd", type=float, default=0.02)
    ap.add_argument("--workers", type=int, default=4)
    ap.add_argument("--timeout-s", type=float, default=240)
    ap.add_argument("--seed", type=int, default=2026)
    ap.add_argument("--tasks", help="comma-separated ids (smoke only)")
    ap.add_argument("--out", type=Path, default=ROOT / "bench/results/v15")
    args = ap.parse_args(argv)
    if not (args.allow_live and os.environ.get("WEAVE_BENCH_LIVE") == "1"):
        raise SystemExit("refusing: pass --allow-live AND set WEAVE_BENCH_LIVE=1")
    key = os.environ.get(args.api_key_env)
    if not key:
        raise SystemExit(f"${args.api_key_env} is not set")
    man = live_v2_manifest()
    commit = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True).stdout.strip()
    dirty = bool(subprocess.run(["git", "status", "--porcelain", "--untracked-files=no"], cwd=ROOT,
                                capture_output=True, text=True).stdout.strip())
    if not args.tasks:
        if dirty:
            raise SystemExit("refusing: commit first")
        if man["manifest_hash"][:16] not in (ROOT / "docs/paper/preregistration.md").read_text():
            raise SystemExit("refusing: live-v2 manifest is not pre-registered")
    from openai import OpenAI
    client = OpenAI(base_url=args.base_url, api_key=key, max_retries=2)
    tasks = [t for t in LIVE_V2 if not args.tasks or t.id in args.tasks.split(",")]
    rng = random.Random(args.seed)
    units = [(t, r) for r in range(args.repeats) for t in tasks]
    rng.shuffle(units)
    plan = []
    for t, r in units:
        pair = ["weave", "native"]
        rng.shuffle(pair)
        plan.extend((t, a, r) for a in pair)
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    out = args.out / f"{stamp}-live-v2-{args.model}"
    out.mkdir(parents=True)
    meta = {"started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "source_commit": commit,
            "source_dirty": dirty, "manifest": man, "task_subset": args.tasks, "model": args.model,
            "effort": args.effort, "repeats": args.repeats, "planned": len(plan), "usd_budget": args.usd_budget,
            "seed": args.seed, "endpoint": args.base_url.split("/")[2], "deployment": args.deployment}
    (out / "run.json").write_text(json.dumps(meta, indent=2) + "\n")
    lock = threading.Condition()
    state = {"spent": 0.0, "in_flight": 0, "done": 0, "recent": [], "stopped": None}
    skipped = []
    rows_f = (out / "rows.jsonl").open("a")

    def go(t, arm, r):
        with lock:
            while state["spent"] + (state["in_flight"] + 1) * args.reserve_usd > args.usd_budget or state["stopped"]:
                if state["in_flight"] == 0 or state["stopped"]:
                    skipped.append((t.id, arm, r))
                    return
                lock.wait()
            state["in_flight"] += 1
        try:
            row = trial(client, args, t, arm)
        finally:
            with lock:
                state["in_flight"] -= 1
                lock.notify_all()
        row["repeat"] = r
        with lock:
            state["spent"] += row["usd"]
            state["done"] += 1
            state["recent"] = (state["recent"] + [row["status"] == "error"])[-20:]
            if len(state["recent"]) >= 10 and sum(state["recent"]) / len(state["recent"]) > 0.10:
                state["stopped"] = "provider error rate above 10% in the last 20 trials"
            rows_f.write(json.dumps(row, sort_keys=True) + "\n")
            rows_f.flush()
            print(f"[{state['done']}/{len(plan)}] {t.id:<18} {arm:<6} pass={row['passed']!s:<5} calls={row['agent_calls']} "
                  f"wall={row['wall_s']:.1f}s ${row['usd']:.4f} spent=${state['spent']:.4f}"
                  f"{' ERR ' + row['error'] if row['error'] else ''}", flush=True)

    from concurrent.futures import ThreadPoolExecutor
    with ThreadPoolExecutor(max_workers=args.workers) as pool:
        for f in [pool.submit(go, *p) for p in plan]:
            f.result()
    rows_f.close()
    meta.update({"finished_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "spent_usd": state["spent"],
                 "not_run": [{"task": a, "arm": b, "repeat": c} for a, b, c in skipped], "stopped": state["stopped"]})
    (out / "run.json").write_text(json.dumps(meta, indent=2) + "\n")
    print(f"results: {out} (spent ${state['spent']:.4f}; {len(skipped)} trials not run)")


if __name__ == "__main__":
    main()
