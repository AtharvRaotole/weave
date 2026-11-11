#!/bin/bash
cd "$(dirname "$0")/.."
export PYTHONPATH=python
printf '\033[1;32m$\033[0m '
cmd="python examples/demo.py"
for ((i=0;i<${#cmd};i++)); do printf '%s' "${cmd:$i:1}"; sleep 0.05; done
sleep 0.4; echo
python3 examples/demo.py
sleep 2.5
