"""Cumulative list-price spend of rounds 15 and 16 (all rows.jsonl under the given result dirs plus the r15 smoke
runs). Usage: python r16_spend.py DIR [DIR ...]  -> prints the total in USD."""
import glob
import json
import sys

tot = 0.0
files = []
for d in sys.argv[1:]:
    files += glob.glob(d.rstrip("/") + "/*/rows.jsonl")
files += glob.glob("/workspace/weave-notes/r15-smoke/*/rows.jsonl")
for f in files:
    for line in open(f):
        tot += json.loads(line).get("usd") or 0.0
print(round(tot, 4))
