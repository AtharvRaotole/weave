"""Offline oracle "models" for both modes, so anyone can run the harness without keys.

They are hand-written optimal agents: the native one issues every independent
call of a dependency level in one turn (maximal parallel tool calling); the
Weave one writes one script. Their numbers measure harness mechanics and the
turn/latency floor of each interface, NOT any model's ability.
"""

from __future__ import annotations

import json
import re

_IDS = re.compile(r"\b(?:C|R|T|SKU)-\d+\b")


def _q(text, pattern):
    return re.search(pattern, text).group(1)


def native_plan(task):
    i = task.instruction
    f = task.family
    if f in ("fanout_sum", "partial_failure"):
        cat = _q(i, r"category '([^']+)'")
        (skus,) = yield [("list_skus", {"category": cat})]
        res = yield [("get_stock", {"sku": s}) for s in skus]
        ok = [r["on_hand"] for r in res if "error" not in r]
        return str(sum(ok)) if f == "fanout_sum" else f"{sum(ok)},{len(res) - len(ok)}"
    if f == "order_value":
        cust = _q(i, r"customer (C-\d+)")
        (orders,) = yield [("list_orders", {"customer_id": cust})]
        details = yield [("get_order", {"order_id": o}) for o in orders]
        skus = sorted({d["sku"] for d in details})
        prices = yield [("get_price", {"sku": s}) for s in skus]
        p = {r["sku"]: r["unit_price_cents"] for r in prices}
        return str(sum(d["qty"] * p[d["sku"]] for d in details))
    if f == "chain":
        rid, total = _q(i, r"record (R-\d+)"), 0
        while True:
            (r,) = yield [("get_record", {"record_id": rid})]
            total += r["value"]
            if r["next"] is None:
                return f"{r['value']},{total}"
            rid = r["next"]
    if f == "triage":
        limit = int(_q(i, r"age_days > (\d+)"))
        (ids,) = yield [("list_tickets", {"status": "open"})]
        ts = yield [("get_ticket", {"ticket_id": t}) for t in ids]
        stale = [t["id"] for t in ts if t["age_days"] > limit]
        if stale:
            yield [("update_ticket", {"ticket_id": t, "status": "closed"}) for t in stale]
        return str(len(stale))
    if f == "notify_gold":
        (ids,) = yield [("list_tickets", {"status": "open"})]
        ts = yield [("get_ticket", {"ticket_id": t}) for t in ids]
        owners = sorted({t["customer_id"] for t in ts if t["priority"] == "high"})
        cs = yield [("get_customer", {"customer_id": c}) for c in owners]
        gold = [c["id"] for c in cs if c["tier"] == "gold"]
        if gold:
            yield [("send_message", {"customer_id": c, "text": "We are on it."}) for c in gold]
        return str(len(gold))
    if f == "top_customer":
        cust = _IDS.findall(i)
        lists = yield [("list_orders", {"customer_id": c}) for c in cust]
        best = max(zip(cust, lists), key=lambda x: len(x[1]))[0]
        (c,) = yield [("get_customer", {"customer_id": best})]
        return c["name"]
    raise ValueError(f)


def weave_script(task):
    i = task.instruction
    f = task.family
    if f in ("fanout_sum", "partial_failure"):
        cat = _q(i, r"category '([^']+)'")
        return (f"total = 0\nskipped = 0\nfor s in list_skus('{cat}'):\n    try:\n        total += get_stock(s)['on_hand']\n"
                "    except ToolError:\n        skipped += 1\n"
                + ("print(total)\n" if f == "fanout_sum" else "print(f'{total},{skipped}')\n"))
    if f == "order_value":
        cust = _q(i, r"customer (C-\d+)")
        return (f"total = 0\nfor o in list_orders('{cust}'):\n    d = get_order(o)\n"
                "    total += d['qty'] * get_price(d['sku'])['unit_price_cents']\nprint(total)\n")
    if f == "chain":
        rid = _q(i, r"record (R-\d+)")
        return (f"rid = '{rid}'\ntotal = 0\nfor _ in range(100):\n    r = get_record(rid)\n    total += r['value']\n"
                "    if r['next'] is None:\n        break\n    rid = r['next']\nprint(f\"{r['value']},{total}\")\n")
    if f == "triage":
        limit = int(_q(i, r"age_days > (\d+)"))
        return (f"n = 0\nfor t in list_tickets('open'):\n    if get_ticket(t)['age_days'] > {limit}:\n"
                "        update_ticket(t, 'closed')\n        n += 1\nprint(n)\n")
    if f == "notify_gold":
        return ("owners = []\nfor t in list_tickets('open'):\n    x = get_ticket(t)\n    if x['priority'] == 'high' and x['customer_id'] not in owners:\n"
                "        owners.append(x['customer_id'])\nn = 0\nfor c in owners:\n    if get_customer(c)['tier'] == 'gold':\n"
                "        send_message(c, 'We are on it.')\n        n += 1\nprint(n)\n")
    if f == "top_customer":
        cust = _IDS.findall(i)
        return (f"best = None\nmost = -1\nfor c in {cust!r}:\n    k = len(list_orders(c))\n    if k > most:\n        most = k\n"
                "        best = c\nprint(get_customer(best)['name'])\n")
    raise ValueError(f)


class OracleClient:
    provider = "mock"

    def __init__(self, task, mode):
        self.model = f"oracle-{mode}"
        self.task, self.mode, self.n = task, mode, 0
        self.plan = native_plan(task) if mode == "native" else None
        self.pending = None
        self.done = False

    def complete(self, messages, tools):
        self.n += 1
        usage = {"input": 0, "output": 0, "cached": 0, "reasoning": 0}
        if self.mode != "native":
            if self.n == 1:
                src = weave_script(self.task)
                return {"content": None, "tool_calls": [{"id": "w1", "type": "function", "function": {
                    "name": "run_weave", "arguments": json.dumps({"source": src})}}]}, usage, [{}]
            return {"content": messages[-1]["content"].strip()}, usage, [{}]
        try:
            if self.pending is None:
                calls = next(self.plan)
            else:
                k = len(self.pending)
                results = [json.loads(m["content"]) for m in messages[-k:]]
                calls = self.plan.send(results)
        except StopIteration as stop:
            return {"content": stop.value}, usage, [{}]
        self.pending = calls
        return {"content": None, "tool_calls": [
            {"id": f"c{self.n}-{j}", "type": "function", "function": {"name": n, "arguments": json.dumps(a)}}
            for j, (n, a) in enumerate(calls)]}, usage, [{}]
