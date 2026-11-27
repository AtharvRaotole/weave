#!/bin/bash
# The plan preview recording: assets/plan.gif (see scripts/record-demo.sh).
cd "$(dirname "$0")/.."
export PYTHONPATH=python
printf '\033[1;32m$\033[0m '
cmd="python examples/plan_preview.py"
for ((i=0;i<${#cmd};i++)); do printf '%s' "${cmd:$i:1}"; sleep 0.05; done
sleep 0.4; echo
WEAVE_DEMO_PAUSE=1.5 python3 examples/plan_preview.py
sleep 3
