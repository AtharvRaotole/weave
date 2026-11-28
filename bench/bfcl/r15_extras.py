"""Round 15 descriptive extras, not pre-registered endpoints. Reads merged lc100 rep0 (A+A2), C, and prints
per-repeat rates, the repeat 1 McNemar, a split by the 47 ids never run live, failure types per arm, and a
sensitivity check. Usage: python bench/bfcl/r15_extras.py MERGED_REP0_DIR C_DIR [--out extras.json]"""
import collections
import json
import os
import sys

from bench.bfcl.report_r15 import boot, load, mcnemar
from bench.bfcl.subsets import R15_LC, R15_LC_NEVER_RUN

dirs = [a for a in sys.argv[1:] if not a.startswith("--")][:2]
rows, _ = load(dirs)
out = {}
tasks = sorted(R15_LC)


def rate(arm, rep, ts):
    return sum(rows[(t, arm, rep)]["passed"] for t in ts) / len(ts)


for rep in (0, 1):
    w = [rows[(t, "weave", rep)]["passed"] for t in tasks]
    n = [rows[(t, "native", rep)]["passed"] for t in tasks]
    b = sum(1 for x, y in zip(w, n) if x and not y)
    c = sum(1 for x, y in zip(w, n) if y and not x)
    d = [int(x) - int(y) for x, y in zip(w, n)]
    out[f"rep{rep}"] = dict(weave=sum(w), native=sum(n), weave_only=b, native_only=c, mcnemar_p=mcnemar(b, c),
                            diff=sum(d) / 100, ci=boot(d))


def split(name, ts):
    d = [(sum(rows[(t, "weave", r)]["passed"] for r in (0, 1)) - sum(rows[(t, "native", r)]["passed"] for r in (0, 1))) / 2 for t in ts]
    w = sum(rows[(t, "weave", r)]["passed"] for t in ts for r in (0, 1))
    n = sum(rows[(t, "native", r)]["passed"] for t in ts for r in (0, 1))
    out[name] = dict(tasks=len(ts), weave_pass=w, native_pass=n, trials=2 * len(ts), diff=sum(d) / len(ts), ci=boot(d))


split("never_run_live_47", [t for t in tasks if t in R15_LC_NEVER_RUN])
split("run_before_53", [t for t in tasks if t not in R15_LC_NEVER_RUN])
ft = collections.Counter()
for (t, arm, r), row in rows.items():
    ft[(arm, "pass" if row["passed"] else (row["error_type"] or "").split(":")[-1])] += 1
out["outcomes"] = {f"{a}/{k}": v for (a, k), v in sorted(ft.items())}
# sensitivity: drop every (task, repeat) where either arm ended in empty_turn_model_response
keep = [t for t in tasks]
d = []
for t in keep:
    ps = [(rows[(t, "weave", r)]["passed"], rows[(t, "native", r)]["passed"]) for r in (0, 1)
          if "empty_turn" not in (rows[(t, "weave", r)]["error_type"] or "") + (rows[(t, "native", r)]["error_type"] or "")]
    d.append(sum(int(a) - int(b) for a, b in ps) / len(ps) if ps else None)
dd = [x for x in d if x is not None]
out["drop_empty_turn_pairs"] = dict(tasks=len(dd), diff=sum(dd) / len(dd), ci=boot(dd))
# sign test over tasks
pos = sum(1 for t in tasks if sum(rows[(t, "weave", r)]["passed"] for r in (0, 1)) > sum(rows[(t, "native", r)]["passed"] for r in (0, 1)))
neg = sum(1 for t in tasks if sum(rows[(t, "weave", r)]["passed"] for r in (0, 1)) < sum(rows[(t, "native", r)]["passed"] for r in (0, 1)))
out["tasks_weave_better"] = pos
out["tasks_native_better"] = neg
out["task_sign_test_p"] = mcnemar(pos, neg)
# source runs
print(json.dumps(out, indent=1))
if "--out" in sys.argv:
    json.dump(out, open(sys.argv[sys.argv.index("--out") + 1], "w"), indent=1)
