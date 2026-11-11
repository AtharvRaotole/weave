"""Run tau-bench retail with a native arm (tau-bench's ToolCallingAgent, unchanged) and a Weave arm.

    WEAVE_BENCH_LIVE=1 python -m bench.taubench.run --allow-live --model gpt-4.1-2025-04-14 --split train --tasks 0,1

Both arms share tau-bench's environment, tasks, user simulator and grader (`Env.calculate_reward`). One wrapper
around litellm's `completion` meters tokens for the agent and the user simulator separately and applies the same
request settings to both arms: `parallel_tool_calls` (one value for every arm in a run, see --parallel-tool-calls),
`reasoning_effort` for reasoning models, and HTTP retries. Provider errors end the trial with reward 0 and keep the row.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import random
import subprocess
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEV_SPLITS = ("train", "dev")
TAU_COMMIT = "59a200c6d575d595120f1cb70fea53cef0632f6b"

_local = threading.local()


def meter():
    m = getattr(_local, "m", None)
    if m is None:
        m = _local.m = {"agent": {"calls": 0, "input": 0, "output": 0, "cached": 0, "reasoning": 0, "llm_s": 0.0},
                        "user": {"calls": 0, "input": 0, "output": 0, "cached": 0, "reasoning": 0, "llm_s": 0.0},
                        "finish": []}
    return m


def make_completion(role, real, settings):
    def completion(*args, **kwargs):
        if role == "agent":
            if kwargs.get("tools"):
                kwargs["parallel_tool_calls"] = settings["parallel_tool_calls"]
            if settings.get("effort"):
                kwargs.pop("temperature", None)
                kwargs["reasoning_effort"] = settings["effort"]
        kwargs.setdefault("num_retries", 4)
        t0 = time.monotonic()
        res = real(*args, **kwargs)
        m = meter()[role]
        m["llm_s"] += time.monotonic() - t0
        m["calls"] += 1
        u = getattr(res, "usage", None)
        if u is not None:
            m["input"] += int(getattr(u, "prompt_tokens", 0) or 0)
            m["output"] += int(getattr(u, "completion_tokens", 0) or 0)
            pd = getattr(u, "prompt_tokens_details", None)
            cd = getattr(u, "completion_tokens_details", None)
            m["cached"] += int(getattr(pd, "cached_tokens", 0) or 0) if pd else 0
            m["reasoning"] += int(getattr(cd, "reasoning_tokens", 0) or 0) if cd else 0
        if role == "agent":
            meter()["finish"].append(res.choices[0].finish_reason)
        return res
    return completion


def install_meter(settings):
    import litellm
    import tau_bench.agents.tool_calling_agent as tca
    import tau_bench.envs.user as user
    from bench.taubench import weave_agent

    litellm.drop_params = True
    real = litellm.completion
    tca.completion = make_completion("agent", real, settings)
    weave_agent.completion = make_completion("agent", real, settings)
    user.completion = make_completion("user", real, settings)


def manifest(split, tasks, user_model, env_name="retail"):
    body = {"benchmark": "tau-bench", "commit": TAU_COMMIT, "env": env_name, "split": split, "tasks": list(tasks),
            "user_model": user_model, "user_strategy": "llm", "max_num_steps": 30}
    body["hash"] = hashlib.sha256(json.dumps(body, sort_keys=True).encode()).hexdigest()[:16]
    return body


def one_trial(args, arm, task_index, repeat):
    from tau_bench.envs import get_env
    from tau_bench.agents.tool_calling_agent import ToolCallingAgent
    from bench.taubench.weave_agent import WeaveAgent

    _local.m = None
    env = get_env(args.env, user_strategy="llm", user_model=args.user_model, user_provider="openai",
                  task_split=args.split, task_index=task_index)
    cls = WeaveAgent if arm == "weave" else ToolCallingAgent
    agent = cls(tools_info=env.tools_info, wiki=env.wiki, model=args.model, provider="openai",
                temperature=args.temperature)
    t0 = time.monotonic()
    status, error, reward, info, messages = "done", None, 0.0, {}, []
    try:
        res = agent.solve(env=env, task_index=task_index, max_num_steps=30)
        reward, info, messages = res.reward, res.info, res.messages
        if "reward_info" not in info:
            status = "step_limit"
    except Exception as exc:  # provider failure: reward 0, row kept
        status, error = "error:" + type(exc).__name__, str(exc)[:300]
    wall = time.monotonic() - t0
    m = meter()
    actions = [a.name for a in env.actions]
    return {
        "env": args.env, "split": args.split, "task_index": task_index, "arm": arm, "repeat": repeat,
        "model": args.model, "effort": args.effort, "temperature": args.temperature, "user_model": args.user_model,
        "parallel_tool_calls": args.parallel_tool_calls, "status": status, "error": error, "reward": reward,
        "passed": reward == 1.0, "reward_info": info.get("reward_info"),
        "agent_calls": m["agent"]["calls"], "user_calls": m["user"]["calls"],
        "agent_tokens": {k: m["agent"][k] for k in ("input", "output", "cached", "reasoning")},
        "user_tokens": {k: m["user"][k] for k in ("input", "output", "cached", "reasoning")},
        "agent_total_tokens": m["agent"]["input"] + m["agent"]["output"],
        "total_tokens": sum(m[r]["input"] + m[r]["output"] for r in ("agent", "user")),
        "agent_llm_s": m["agent"]["llm_s"], "user_llm_s": m["user"]["llm_s"], "wall_s": wall,
        "finish_reasons": m["finish"], "tool_actions": [a for a in actions if a != "respond"],
        "respond_actions": sum(a == "respond" for a in actions), "weave": info.get("weave"),
        "messages": messages,
    }


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--allow-live", action="store_true")
    ap.add_argument("--model", required=True)
    ap.add_argument("--effort")
    ap.add_argument("--temperature", type=float, default=0.0)
    ap.add_argument("--user-model", default="gpt-4.1-2025-04-14")
    ap.add_argument("--env", default="retail")
    ap.add_argument("--split", default="train")
    ap.add_argument("--tasks", required=True, help="comma-separated task indices, or @name of a frozen subset")
    ap.add_argument("--arms", default="weave,native")
    ap.add_argument("--parallel-tool-calls", choices=("true", "false"), default="false")
    ap.add_argument("--repeats", type=int, default=1)
    ap.add_argument("--workers", type=int, default=6)
    ap.add_argument("--token-budget", type=int, default=500_000)
    ap.add_argument("--out", type=Path, default=ROOT / "bench/results/v8")
    ap.add_argument("--tag", default="")
    ap.add_argument("--seed", type=int, default=2026)
    args = ap.parse_args(argv)
    args.parallel_tool_calls = args.parallel_tool_calls == "true"
    if not (args.allow_live and os.environ.get("WEAVE_BENCH_LIVE") == "1"):
        raise SystemExit("refusing: pass --allow-live AND set WEAVE_BENCH_LIVE=1")
    if args.tasks.startswith("@"):
        from bench.taubench.subsets import SUBSETS
        args.split, tasks = SUBSETS[args.tasks[1:]]
    else:
        tasks = [int(x) for x in args.tasks.split(",")]
    man = manifest(args.split, tasks, args.user_model, args.env)
    commit = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True).stdout.strip()
    dirty = bool(subprocess.run(["git", "status", "--porcelain", "--untracked-files=no"], cwd=ROOT,
                                capture_output=True, text=True).stdout.strip())
    if args.split not in DEV_SPLITS:
        if dirty:
            raise SystemExit("refusing: commit first")
        if man["hash"] not in (ROOT / "docs/paper/preregistration.md").read_text():
            raise SystemExit(f"refusing: manifest {man['hash']} is not pre-registered")
    import tau_bench
    tau_dir = Path(tau_bench.__file__).resolve().parents[1]
    tau_head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=tau_dir, capture_output=True, text=True).stdout.strip()
    if tau_head != TAU_COMMIT:
        raise SystemExit(f"refusing: tau-bench checkout is {tau_head}, expected {TAU_COMMIT}")
    install_meter({"parallel_tool_calls": args.parallel_tool_calls, "effort": args.effort})
    arms = args.arms.split(",")
    plan = [(arm, t, r) for r in range(args.repeats) for t in tasks for arm in arms]
    random.Random(args.seed).shuffle(plan)
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    out = args.out / f"{stamp}-tau-{args.env}-{args.split}-{args.model}{args.tag}"
    out.mkdir(parents=True)
    meta = {"started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "source_commit": commit,
            "source_dirty": dirty, "manifest": man, "model": args.model, "effort": args.effort,
            "temperature": args.temperature, "parallel_tool_calls": args.parallel_tool_calls, "arms": arms,
            "repeats": args.repeats, "planned": len(plan), "token_budget": args.token_budget, "seed": args.seed}
    (out / "run.json").write_text(json.dumps(meta, indent=2) + "\n")
    rows_f, traces_f = (out / "rows.jsonl").open("a"), (out / "traces.jsonl").open("a")
    lock, spent, done = threading.Lock(), [0], [0]

    def go(arm, t, r):
        with lock:
            if spent[0] >= args.token_budget:
                return
        row = one_trial(args, arm, t, r)
        messages = row.pop("messages")
        with lock:
            spent[0] += row["total_tokens"]
            done[0] += 1
            rows_f.write(json.dumps(row, sort_keys=True, default=str) + "\n")
            rows_f.flush()
            traces_f.write(json.dumps({"task_index": t, "arm": arm, "repeat": r, "messages": messages}, default=str) + "\n")
            traces_f.flush()
            print(f"[{done[0]}/{len(plan)}] {args.model} task {t:<3} {arm:<7} reward={row['reward']:.0f} {row['status']:<10} "
                  f"agent_calls={row['agent_calls']:<3} wall={row['wall_s']:6.1f}s tok={row['total_tokens']:>7} spent={spent[0]}",
                  flush=True)

    with ThreadPoolExecutor(max_workers=args.workers) as pool:
        for f in as_completed([pool.submit(go, *p) for p in plan]):
            f.result()
    rows_f.close()
    traces_f.close()
    print("results:", out)


if __name__ == "__main__":
    main()
