"""Where do the Weave arm's extra tokens go on BFCL? Reads committed traces only (no API calls).

For each evidence trial: tool-definition tokens per request (native JSON schemas vs the one `run_weave` description),
message tokens (scripts the model wrote, tool outputs returned), and model calls; then splits the per-trial input
difference into "tool definitions x calls" and "conversation". Token counts use tiktoken's o200k_base encoding, an
approximation of what the API bills (it adds a small per-message overhead we do not model).

    OPENAI_API_KEY=x PYTHONPATH=python:. /tmp/bfcl312/bin/python bench/bfcl/diagnose.py [run-dir ...]
"""

from __future__ import annotations

import collections
import glob
import json
import os
import statistics as st
import sys

import tiktoken

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, ROOT)
ENC = tiktoken.get_encoding("o200k_base")


def ntok(x):
    return len(ENC.encode(x if isinstance(x, str) else json.dumps(x)))


def native_tools(entry):
    from bfcl_eval.constants.type_mappings import GORILLA_TO_OPENAPI
    from bfcl_eval.constants.enums import ModelStyle
    from bfcl_eval.model_handler.utils import convert_to_tool
    return convert_to_tool(entry["function"], GORILLA_TO_OPENAPI, ModelStyle.OPENAI_COMPLETIONS)


def weave_tool(entry, builder):
    return builder(entry)


def default_builder(entry):
    import weave
    from bench.bfcl.weave_arm import make_functions
    return weave.WeaveTool(make_functions(entry, "diag", [], collections.Counter()), max_wall_time_ms=60_000).chat_tool()


def main(dirs):
    from bfcl_eval.utils import load_dataset_entry
    entries = {e["id"]: e for e in load_dataset_entry("multi_turn_base")}
    dirs = dirs or sorted(glob.glob(os.path.join(ROOT, "bench/results/v11/*-evidence-*")))
    for d in dirs:
        rows = {(r["id"], r["arm"]): r for r in map(json.loads, open(os.path.join(d, "rows.jsonl")))}
        traces = {(t["id"], t["arm"]): t for t in map(json.loads, open(os.path.join(d, "traces.jsonl")))}
        agg = collections.defaultdict(list)
        for (tid, arm), r in rows.items():
            if arm != "weave" or (tid, "native") not in rows:
                continue
            n = rows[(tid, "native")]
            e = entries[tid]
            nt, wt = ntok(native_tools(e)), ntok(default_builder(e))
            msgs = traces[(tid, "weave")]["messages"] or []
            script_toks = sum(ntok(tc["function"]["arguments"]) for m in msgs if isinstance(m, dict) and m.get("tool_calls")
                              for tc in m["tool_calls"])
            out_toks = sum(ntok(m["content"]) for m in msgs if isinstance(m, dict) and m.get("role") == "tool")
            err_outs = sum(1 for m in msgs if isinstance(m, dict) and m.get("role") == "tool" and m["content"].startswith("error:"))
            agg["native_tool_defs"].append(nt)
            agg["weave_tool_def"].append(wt)
            agg["weave_calls"].append(r["agent_calls"])
            agg["native_calls"].append(n["agent_calls"])
            agg["defs_x_calls_diff"].append(wt * r["agent_calls"] - nt * n["agent_calls"])
            agg["input_diff"].append(r["tokens"]["input"] - n["tokens"]["input"])
            agg["uncached_diff"].append((r["tokens"]["input"] - r["tokens"]["cached"]) - (n["tokens"]["input"] - n["tokens"]["cached"]))
            agg["output_diff"].append(r["tokens"]["output"] - n["tokens"]["output"])
            agg["weave_script_tokens"].append(script_toks)
            agg["weave_tool_output_tokens"].append(out_toks)
            agg["weave_error_outputs"].append(err_outs)
            agg["weave_cache_rate"].append(r["tokens"]["cached"] / max(1, r["tokens"]["input"]))
            agg["native_cache_rate"].append(n["tokens"]["cached"] / max(1, n["tokens"]["input"]))
            agg["weave_tool_calls_per_script"].append(r["weave"]["tool_calls"] / max(1, r["weave"]["scripts"]))
        print(os.path.basename(d), f"({len(agg['input_diff'])} pairs)")
        for k, v in agg.items():
            print(f"  {k:<30} mean {st.fmean(v):10.2f}   median {st.median(v):10.2f}")


if __name__ == "__main__":
    main(sys.argv[1:])
