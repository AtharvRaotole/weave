#!/bin/bash
# Round 16 chain (pre-registered, docs/research/round16-preregistration.md). Idempotent: a finished step leaves
# $L/stepN.done and is skipped on a rerun. Stops starting steps once cumulative r15+r16 spend reaches $20.
W=/workspace/weave-r16
L=/workspace/weave-notes/r16-logs
cd $W
export WEAVE_BENCH_LIVE=1 PYTHONPATH=python:.
PY=/workspace/bfcl312/bin/python
COMMON="--allow-live --model gpt-5.4-mini-2026-03-17 --effort none --base-url https://atharvraotole-4504-resource.services.ai.azure.com/openai/v1 --deployment gpt-5.4-mini --arms weave,native --repeats 1 --reserve-usd 0.05 --out bench/results/v16 --weave-config compact,learned,trace,typecheck,gatechoice,errkey,recover --cache-key"
step() {  # N CATEGORY SUBSET WORKERS BUDGET SEED TAG
  [ -f $L/step$1.done ] && return 0
  S=$($PY bench/bfcl/r16_spend.py /workspace/weave-r15b/bench/results/v15 $W/bench/results/v16)
  if $PY -c "import sys; sys.exit(0 if float('$S') < 20.0 else 1)"; then :; else echo "STOP: cumulative spend $S >= 20" > $L/chain.stopped; return 1; fi
  echo "$(date +%T) step $1 start, cumulative \$$S" >> $L/chain.log
  $PY -m bench.bfcl.run $COMMON --category $2 --tasks @$3 --workers $4 --usd-budget $5 --seed $6 --tag=$7 > $L/step$1.log 2>&1
  echo "$(date +%T) step $1 exit $?" >> $L/chain.log
  touch $L/step$1.done
}
step 1 multi_turn_long_context lc39r16 2 2.00 2026 -r16-lc-rep0
step 2 multi_turn_miss_param   mp57r16 3 1.20 2026 -r16-mp-rep0
step 3 multi_turn_miss_func    mf67r16 3 1.60 2026 -r16-mf-rep0
step 4 multi_turn_long_context lc39r16 2 2.00 2027 -r16-lc-rep1
step 5 multi_turn_miss_param   mp57r16 3 1.20 2027 -r16-mp-rep1
step 6 multi_turn_miss_func    mf67r16 3 1.60 2027 -r16-mf-rep1
echo CHAIN_DONE > $L/chain.done
