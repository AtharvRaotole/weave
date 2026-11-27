"""Replay recorded BFCL Weave-arm scripts against their recorded tool results, with and without `error_key`.

No model calls and no BFCL backends: each script from `bench/results/v1*/**/traces.jsonl` (rounds 11 to 14) that
recorded its calls (the `[calls]` or `[results so far]` section of the tool message) is run again with every tool
replaced by a stub that returns that tool's recorded results in order. Every BFCL function is declared a reversible
write, as in the arm, so calls run in program order and the replay is deterministic.

1. Plain replay (no `error_key`): a script is *faithful* when it makes exactly the recorded calls and fails or
   succeeds as the recorded run did. Only faithful scripts are counted below (results are truncated at 300 or 400
   characters in the record, so some scripts cannot be replayed).
2. The same script with `error_key="error"` on every tool. For each faithful script in which a call returned a
   dict with an `"error"` key, we count whether the script now fails, whether its own `try`/`except` handled it,
   and the recorded calls that no longer run (state-changing ones are those not in `effects.json`'s `reads`).

    PYTHONPATH=python python bench/bfcl/replay_error_key.py [--out bench/results/error-key-replay.json]
"""

from __future__ import annotations

import argparse
import ast
import collections
import glob
import json
import os
import re

import weave

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CALL = re.compile(r"^(\w+)\((.*)\) -> (.*)$")


def parse_result(out: str):
    """As `weave_arm.parse_result` (kept here so the replay needs no BFCL install): JSON, a Python literal, or text."""
    try:
        return json.loads(out)
    except (ValueError, TypeError):
        pass
    try:
        return ast.literal_eval(out)
    except (ValueError, SyntaxError, TypeError, MemoryError, RecursionError):
        return out


def recorded_scripts() -> list:
    out = []
    for path in sorted(glob.glob(os.path.join(ROOT, "bench/results/v1*/**/traces.jsonl"), recursive=True)):
        run = os.path.basename(os.path.dirname(path))
        if "bfcl" not in run:
            continue
        for line in open(path):
            trace = json.loads(line)
            if trace.get("arm") != "weave":
                continue
            pending = {}
            for m in trace["messages"]:
                for call in m.get("tool_calls") or []:
                    if call["function"]["name"] == "run_weave":
                        try:
                            pending[call["id"]] = json.loads(call["function"]["arguments"]).get("source")
                        except ValueError:
                            continue
                if m.get("role") == "tool" and m.get("tool_call_id") in pending:
                    source = pending.pop(m["tool_call_id"])
                    content = m.get("content") or ""
                    section = next((s for s in ("\n[calls]\n", "[results so far]\n") if s in content), None)
                    if isinstance(source, str) and section:
                        calls = [CALL.match(x) for x in content.split(section, 1)[1].splitlines()]
                        calls = [(c.group(1), c.group(3)) for c in calls if c]
                        out.append({"run": run, "task": trace.get("id"), "script": source, "response": content,
                                    "paused": content.startswith("[paused"), "calls": calls})
    return out


def failed(text: str) -> bool:
    return text.startswith("error:") or "\nerror:" in text


def replay(script: dict, error_key: str | None) -> dict:
    queues = collections.defaultdict(list)
    for name, out in script["calls"]:
        queues[name].append(out)
    made, extra = [], []
    e = weave.Engine()
    for name in {n for n, _ in script["calls"]} | set(re.findall(r"\b(\w+)\s*\(", script["script"])):
        def stub(*args, _name=name, **kwargs):
            if not queues[_name]:
                extra.append(_name)
                raise weave.ToolError(f"unrecorded call to {_name}")
            out = queues[_name].pop(0)
            made.append(_name)
            if out.startswith("Error during execution:") or out.startswith("error:"):
                raise weave.ToolError(out)
            return parse_result(out)
        e.tool(name, stub, effect="reversible", check_args=False, error_key=error_key)
    try:
        result = e.run(script["script"])
        error, stdout = None, result.stdout
    except Exception as exc:  # noqa: BLE001 - the script's failure, as the tool reports it
        error, stdout = str(exc), ""
    return {"made": made, "extra": extra, "error": error, "stdout": stdout}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=os.path.join(ROOT, "bench/results/error-key-replay.json"))
    args = ap.parse_args()
    reads = set(json.load(open(os.path.join(ROOT, "bench/bfcl/effects.json")))["reads"])
    counts = collections.Counter()
    cases = []
    for s in recorded_scripts():
        counts["scripts_with_recorded_calls"] += 1
        plain = replay(s, None)
        recorded = [n for n, _ in s["calls"]]
        faithful = (not plain["extra"] and plain["made"] == recorded
                    and (plain["error"] is not None) == (failed(s["response"]) and not s["paused"]))
        if not faithful:
            counts["not_faithful"] += 1
            continue
        counts["faithful"] += 1
        first = next((i for i, (_, out) in enumerate(s["calls"])
                      if isinstance(parse_result(out), dict) and "error" in parse_result(out)), None)
        if first is None:
            keyed = replay(s, "error")
            counts["no_error_value_unchanged" if keyed["made"] == plain["made"] and keyed["error"] == plain["error"]
                   else "no_error_value_CHANGED"] += 1
            continue
        counts["with_error_value"] += 1
        keyed = replay(s, "error")
        skipped = plain["made"][len(keyed["made"]):] if keyed["made"] == plain["made"][:len(keyed["made"])] else None
        if skipped is None:
            counts["with_error_value_took_another_path"] += 1
        else:
            writes = [n for n in skipped if n not in reads]
            counts["calls_no_longer_run"] += len(skipped)
            counts["state_changing_calls_no_longer_run"] += len(writes)
            counts["scripts_with_state_changing_calls_no_longer_run"] += bool(writes)
        if plain["error"] is None:
            counts["recorded_ok_" + ("now_fails" if keyed["error"] else "still_ok")] += 1
            if keyed["error"] is None:
                counts["recorded_ok_still_ok_" + ("handled_by_try" if "except" in s["script"] else "other")] += 1
        else:
            counts["recorded_failed_still_fails" if keyed["error"] else "recorded_failed_now_ok"] += 1
            if keyed["error"] and plain["error"] != keyed["error"]:
                counts["recorded_failed_message_now_names_the_failed_call"] += "failed:" in keyed["error"]
        cases.append({"run": s["run"], "task": s["task"], "script": s["script"],
                      "plain_error": plain["error"], "keyed_error": keyed["error"], "skipped": skipped})
    summary = dict(sorted(counts.items()))
    json.dump({"summary": summary, "cases": cases}, open(args.out, "w"), indent=1)
    print(json.dumps(summary, indent=1))


if __name__ == "__main__":
    main()
