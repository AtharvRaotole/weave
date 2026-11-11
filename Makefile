# Reproduce the v4 benchmark. `bench-mock` is offline and free. `bench-live` spends API tokens and
# needs BOTH `WEAVE_BENCH_LIVE=1` in the environment and the --allow-live flag the target passes.
PY ?= python3
export PYTHONPATH := python

.PHONY: build bench-mock bench-live charts test-bench

build:
	cargo build --release -p weave-py
	cp target/release/libweave_py.so python/weave/_native$$($(PY) -c 'import sysconfig;print(sysconfig.get_config_var("EXT_SUFFIX"))')

bench-mock: build
	$(PY) -m bench.v4.run --mock --suite held_out --repeats 1 --latency-ms 0,200 --out /tmp/weave-v4-mock

test-bench: build
	$(PY) bench/v4/tests/test_v4.py

# Example: WEAVE_BENCH_LIVE=1 make bench-live MODEL=gpt-5.4-mini-2026-03-17 EFFORT=low
MODEL ?= gpt-5.4-mini-2026-03-17
PROVIDER ?= openai
EFFORT ?= low
SUITE ?= held_out
REPEATS ?= 3
bench-live: build
	$(PY) -m bench.v4.run --allow-live --provider $(PROVIDER) --model $(MODEL) $(if $(EFFORT),--effort $(EFFORT)) \
	  --suite $(SUITE) --repeats $(REPEATS) --latency-ms 200,0

charts:
	$(PY) bench/v4/charts.py
