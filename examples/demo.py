"""The README demo: one model turn of Weave against tool-at-a-time calling. Offline, no API key.

    python examples/demo.py

Five fake API tools, each sleeping 300 ms. The script below is the kind of script models
write for the benchmark's top-customer task: a comprehension, a `key=` lambda and a helper.
Weave runs it in one tool call, overlapping the independent reads; the irreversible
`send_email` runs after the reads it depends on. Every number printed is measured here.
"""

import sys
import threading
import time

import weave

LATENCY_S = 0.3
CUSTOMERS = {"c1": "Ada", "c2": "Grace", "c3": "Linus", "c4": "Barbara"}
ORDERS = {"c1": ["o1", "o2"], "c2": ["o3"], "c3": ["o4", "o5", "o6"], "c4": ["o7"]}
TOTALS = {"o1": 120, "o2": 80, "o3": 310, "o4": 40, "o5": 95, "o6": 60, "o7": 150}
LOG, START, LOCK = [], [0.0], threading.Lock()
SLOW = "--fast" not in sys.argv


def _call(name, arg):
    t0 = time.perf_counter() - START[0]
    if SLOW:
        time.sleep(LATENCY_S)
    with LOCK:
        LOG.append((t0, name, arg))


@weave.declare(effect="read", arity=0)
def list_customers() -> list:
    """Customer ids."""
    _call("list_customers", "")
    return list(CUSTOMERS)


@weave.declare(effect="read", arity=1)
def get_orders(customer_id: str) -> list:
    """Order ids of one customer."""
    _call("get_orders", customer_id)
    return ORDERS[customer_id]


@weave.declare(effect="read", arity=1)
def order_total(order_id: str) -> int:
    """Total of one order, in dollars."""
    _call("order_total", order_id)
    return TOTALS[order_id]


@weave.declare(effect="irreversible", arity=2)
def send_email(customer_id: str, text: str) -> str:
    """Email a customer."""
    _call("send_email", customer_id)
    return "sent"


SCRIPT = '''\
def spend(cid):
    return sum(order_total(o) for o in get_orders(cid))

totals = {cid: spend(cid) for cid in list_customers()}
best = max(totals, key=lambda c: totals[c])
send_email(best, f"Thanks! You spent ${totals[best]} with us.")
print(best, totals[best])
'''


def say(text="", pause=0.0):
    print(text, flush=True)
    if SLOW and pause:
        time.sleep(pause)


def main():
    tool = weave.WeaveTool([list_customers, get_orders, order_total, send_email])
    say(f"Task: email the customer who spent the most. 4 tools, each a {int(LATENCY_S * 1000)} ms API call.", 0.8)
    say("The model sends ONE run_weave call with this script:\n", 0.4)
    for line in SCRIPT.splitlines():
        say("    " + line, 0.12)
    say()
    say("Weave runs it:", 0.4)
    START[0] = time.perf_counter()
    out = tool.run(SCRIPT)
    wall = time.perf_counter() - START[0]
    waves = []
    for t0, name, arg in sorted(LOG):
        if not waves or t0 - waves[-1][0] > LATENCY_S / 2:
            waves.append((t0, []))
        waves[-1][1].append(f"{name}({arg})")
    for i, (t0, calls) in enumerate(waves, 1):
        names = sorted(set(c.split("(")[0] for c in calls))
        say(f"  wave {i} at {t0:4.2f}s: {len(calls)} call{'s together' if len(calls) > 1 else ''}  ({', '.join(names)})", 0.35)
    say(f"  output: {out.strip()}", 0.3)
    calls = len(LOG)
    say()
    say(f"Weave:              1 model turn, {calls} tool calls in {len(waves)} waves, {wall:.2f} s", 0.3)
    say(f"One call at a time: {calls} round trips x {LATENCY_S:.1f} s = {calls * LATENCY_S:.1f} s of tool time alone", 0.3)
    say(f"Native parallel tool calls: a model turn per wave, {len(waves)} turns + the answer", 0.3)
    assert out.strip() == "c2 310", out
    say("\ndemo ok")


if __name__ == "__main__":
    main()
