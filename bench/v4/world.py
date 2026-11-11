"""Deterministic simulated operations backend shared by both modes.

One tool surface (a small ticketing / order / inventory API) for every task.
Each trial builds a fresh :class:`World` from the task's data. Both modes call
the *same* Python functions through :meth:`World.call`, which applies the
injected per-call latency, the shared concurrency cap and the call/write log
the grader inspects. Nothing here talks to the network.
"""

from __future__ import annotations

import random
import threading
import time

READ, WRITE = "read", "irreversible"

# name -> (effect, params {name: json type}, returns (JSON schema), description)
TOOLS = {
    "list_skus": (READ, {"category": "string"}, {"type": "array", "items": {"type": "string"}},
                  "List the SKU ids in a product category."),
    "get_stock": (READ, {"sku": "string"},
                  {"type": "object", "properties": {"sku": {"type": "string"}, "on_hand": {"type": "integer"}},
                   "required": ["sku", "on_hand"]},
                  "Units on hand for a SKU. Fails for discontinued SKUs."),
    "get_price": (READ, {"sku": "string"},
                  {"type": "object", "properties": {"sku": {"type": "string"}, "unit_price_cents": {"type": "integer"}},
                   "required": ["sku", "unit_price_cents"]},
                  "Unit price of a SKU in cents."),
    "list_orders": (READ, {"customer_id": "string"}, {"type": "array", "items": {"type": "string"}},
                    "List a customer's order ids."),
    "get_order": (READ, {"order_id": "string"},
                  {"type": "object", "properties": {"id": {"type": "string"}, "customer_id": {"type": "string"},
                                                     "sku": {"type": "string"}, "qty": {"type": "integer"}},
                   "required": ["id", "customer_id", "sku", "qty"]},
                  "One order: which SKU and how many units."),
    "get_customer": (READ, {"customer_id": "string"},
                     {"type": "object", "properties": {"id": {"type": "string"}, "name": {"type": "string"},
                                                        "tier": {"type": "string"}, "region": {"type": "string"}},
                      "required": ["id", "name", "tier", "region"]},
                     "A customer's name, tier (gold/silver/bronze) and region."),
    "list_tickets": (READ, {"status": "string"}, {"type": "array", "items": {"type": "string"}},
                     "List ticket ids with a status (open, closed, investigating)."),
    "get_ticket": (READ, {"ticket_id": "string"},
                   {"type": "object", "properties": {"id": {"type": "string"}, "customer_id": {"type": "string"},
                                                      "priority": {"type": "string"}, "age_days": {"type": "integer"},
                                                      "status": {"type": "string"}},
                    "required": ["id", "customer_id", "priority", "age_days", "status"]},
                   "One support ticket."),
    "get_record": (READ, {"record_id": "string"},
                   {"type": "object", "properties": {"id": {"type": "string"}, "value": {"type": "integer"},
                                                      "next": {"type": ["string", "null"]}},
                    "required": ["id", "value", "next"]},
                   "One linked record; `next` is the id of the following record or null."),
    "update_ticket": (WRITE, {"ticket_id": "string", "status": "string"},
                      {"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"]},
                      "Set a ticket's status. Irreversible: never repeat a call."),
    "send_message": (WRITE, {"customer_id": "string", "text": "string"},
                     {"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"]},
                     "Send a message to a customer. Irreversible: never repeat a call."),
}


class ToolFailure(Exception):
    """A backend error both modes see with the same message."""


class World:
    def __init__(self, data: dict, *, latency_ms: float = 0.0, concurrency: int = 8, seed: int = 0):
        self.data = data
        self.latency_ms = latency_ms
        self.sem = threading.BoundedSemaphore(concurrency)
        self.lock = threading.Lock()
        self.rng = random.Random(seed)
        self.calls: list[dict] = []
        self.writes: list[tuple] = []
        self.tickets = {k: dict(v) for k, v in data.get("tickets", {}).items()}
        self.peak = 0
        self.active = 0
        self.tool_busy_s = 0.0

    def _delay(self):
        if self.latency_ms <= 0:
            return 0.0
        with self.lock:
            jitter = self.rng.uniform(0.5, 1.5)
        return self.latency_ms * jitter / 1000.0

    def call(self, name: str, args: dict):
        if name not in TOOLS:
            raise ToolFailure(f"unknown tool {name}")
        effect, params, _, _ = TOOLS[name]
        if set(args) != set(params):
            raise ToolFailure(f"{name} takes arguments {sorted(params)}")
        for k, v in args.items():
            if not isinstance(v, str):
                raise ToolFailure(f"argument {k} must be a string")
        delay = self._delay()
        with self.sem:
            with self.lock:
                self.active += 1
                self.peak = max(self.peak, self.active)
                entry = {"name": name, "args": dict(args), "t": time.monotonic()}
                self.calls.append(entry)
            try:
                if delay:
                    time.sleep(delay)
                result = self._dispatch(name, args)
                entry["ok"] = True
                return result
            except ToolFailure as exc:
                entry["ok"] = False
                entry["error"] = str(exc)
                raise
            finally:
                with self.lock:
                    self.active -= 1
                    self.tool_busy_s += delay

    def _dispatch(self, name, a):
        d = self.data
        if name == "list_skus":
            return list(d.get("categories", {}).get(a["category"], []))
        if name == "get_stock":
            sku = d.get("skus", {}).get(a["sku"])
            if sku is None:
                raise ToolFailure(f"unknown sku {a['sku']}")
            if sku.get("discontinued"):
                raise ToolFailure(f"sku {a['sku']} is discontinued")
            return {"sku": a["sku"], "on_hand": sku["on_hand"]}
        if name == "get_price":
            sku = d.get("skus", {}).get(a["sku"])
            if sku is None:
                raise ToolFailure(f"unknown sku {a['sku']}")
            return {"sku": a["sku"], "unit_price_cents": sku["price"]}
        if name == "list_orders":
            if a["customer_id"] not in d.get("customers", {}):
                raise ToolFailure(f"unknown customer {a['customer_id']}")
            return [o for o, v in d.get("orders", {}).items() if v["customer_id"] == a["customer_id"]]
        if name == "get_order":
            o = d.get("orders", {}).get(a["order_id"])
            if o is None:
                raise ToolFailure(f"unknown order {a['order_id']}")
            return {"id": a["order_id"], **o}
        if name == "get_customer":
            c = d.get("customers", {}).get(a["customer_id"])
            if c is None:
                raise ToolFailure(f"unknown customer {a['customer_id']}")
            return {"id": a["customer_id"], **c}
        if name == "list_tickets":
            with self.lock:
                return [t for t, v in self.tickets.items() if v["status"] == a["status"]]
        if name == "get_ticket":
            with self.lock:
                t = self.tickets.get(a["ticket_id"])
                if t is None:
                    raise ToolFailure(f"unknown ticket {a['ticket_id']}")
                return {"id": a["ticket_id"], **t}
        if name == "get_record":
            r = d.get("records", {}).get(a["record_id"])
            if r is None:
                raise ToolFailure(f"unknown record {a['record_id']}")
            return {"id": a["record_id"], **r}
        if name == "update_ticket":
            with self.lock:
                if a["ticket_id"] not in self.tickets:
                    raise ToolFailure(f"unknown ticket {a['ticket_id']}")
                if a["status"] not in ("open", "closed", "investigating"):
                    raise ToolFailure("status must be open, closed or investigating")
                self.tickets[a["ticket_id"]]["status"] = a["status"]
                self.writes.append(("update_ticket", a["ticket_id"], a["status"]))
            return {"ok": True}
        if name == "send_message":
            with self.lock:
                if a["customer_id"] not in d.get("customers", {}):
                    raise ToolFailure(f"unknown customer {a['customer_id']}")
                self.writes.append(("send_message", a["customer_id"], a["text"]))
            return {"ok": True}
        raise ToolFailure(f"unknown tool {name}")
