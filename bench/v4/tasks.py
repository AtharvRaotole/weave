"""Task families for protocol v4 and the frozen dev / held-out / scaling suites.

Every task is generated deterministically from (family, size, seed). The
oracle (expected answer and expected writes) is computed here from the same
data the backend serves and never reaches the model. Instructions are the
only task text either mode sees.

Families follow the Weave thesis: work whose tool calls fan out over many
items, depend on each other, branch on results, aggregate, mix reads with
writes, or partially fail. ``chain`` is included on purpose: its calls are
inherently sequential, so neither mode can parallelize it (Weave can only
save model turns there).
"""

from __future__ import annotations

from dataclasses import dataclass, field
import hashlib
import json
import random

WORDS = ["amber", "birch", "cobalt", "delta", "ember", "fjord", "garnet", "harbor", "indigo", "juniper",
         "kestrel", "lumen", "maple", "nimbus", "onyx", "pebble", "quartz", "raven", "sierra", "tundra",
         "umber", "vesper", "willow", "xenon", "yarrow", "zephyr"]
NAMES = ["Ava Chen", "Ben Ortiz", "Cara Singh", "Dan Novak", "Eve Haddad", "Finn Larsen", "Gia Rossi",
         "Hugo Mensah", "Ines Duarte", "Jon Okafor", "Kira Tanaka", "Leo Brandt", "Mia Kowalski",
         "Noah Petrov", "Omar Farouk", "Pia Lindqvist", "Quinn Murphy", "Rosa Alves", "Sam Whitfield",
         "Tara Nair", "Uma Patel", "Vik Rao", "Wen Li", "Xia Moreau", "Yara Haddad", "Zane Cooper"]


@dataclass(frozen=True)
class Task:
    id: str
    family: str
    split: str
    size: int
    seed: int
    instruction: str
    data: dict = field(repr=False)
    answer: str = field(repr=False)
    writes: tuple = field(repr=False, default=())  # expected multiset of writes
    ordered: bool = False  # writes must also appear in this order
    min_tool_calls: int = 0  # calls a correct solution needs
    min_dependent_rounds: int = 0  # length of the longest dependency chain of calls

    def fingerprint(self):
        blob = json.dumps({"i": self.instruction, "d": self.data, "a": self.answer, "w": list(self.writes)},
                          sort_keys=True)
        return hashlib.sha256(blob.encode()).hexdigest()[:16]


def _ids(rng, prefix, n, width=4):
    seen = set()
    while len(seen) < n:
        seen.add(f"{prefix}-{rng.randrange(10 ** (width - 1), 10 ** width)}")
    return sorted(seen, key=lambda _: rng.random())


def fanout_sum(n, seed, split):
    rng = random.Random(f"fanout_sum:{n}:{seed}")
    cat = f"{rng.choice(WORDS)}-{rng.choice(WORDS)}"
    skus = _ids(rng, "SKU", n)
    data = {"categories": {cat: skus, "misc": _ids(rng, "SKU", 3)},
            "skus": {s: {"on_hand": rng.randrange(0, 500), "price": rng.randrange(100, 9999)} for s in skus}}
    for s in data["categories"]["misc"]:
        data["skus"][s] = {"on_hand": rng.randrange(0, 500), "price": 100}
    total = sum(data["skus"][s]["on_hand"] for s in skus)
    return Task(f"fanout_sum-n{n}-s{seed}", "fanout_sum", split, n, seed,
                f"What is the total number of units on hand across all SKUs in the category '{cat}'? "
                "Answer with only the integer.", data, str(total), min_tool_calls=n + 1, min_dependent_rounds=2)


def partial_failure(n, seed, split):
    rng = random.Random(f"partial_failure:{n}:{seed}")
    cat = f"{rng.choice(WORDS)}-{rng.choice(WORDS)}"
    skus = _ids(rng, "SKU", n)
    k = max(1, n // 4)
    dead = set(rng.sample(skus, k))
    data = {"categories": {cat: skus},
            "skus": {s: {"on_hand": rng.randrange(0, 500), "price": 500, "discontinued": s in dead} for s in skus}}
    total = sum(v["on_hand"] for s, v in data["skus"].items() if s not in dead)
    return Task(f"partial_failure-n{n}-s{seed}", "partial_failure", split, n, seed,
                f"Sum the units on hand across the SKUs in category '{cat}'. Some SKUs are discontinued and their "
                "stock lookup fails; skip those. Answer exactly as '<total>,<number of SKUs skipped>' "
                "(for example 120,2).", data, f"{total},{k}", min_tool_calls=n + 1, min_dependent_rounds=2)


def order_value(n, seed, split):
    """Two dependent fan-out levels: orders -> order details -> unit prices."""
    rng = random.Random(f"order_value:{n}:{seed}")
    cust = _ids(rng, "C", 2)
    skus = _ids(rng, "SKU", max(2, n - 1))
    data = {"customers": {c: {"name": rng.choice(NAMES), "tier": "silver", "region": "north"} for c in cust},
            "skus": {s: {"on_hand": 10, "price": rng.randrange(100, 5000)} for s in skus}, "orders": {}}
    target = cust[0]
    for oid in _ids(rng, "O", n):
        data["orders"][oid] = {"customer_id": target, "sku": rng.choice(skus), "qty": rng.randrange(1, 9)}
    for oid in _ids(rng, "Q", 3):
        data["orders"][oid] = {"customer_id": cust[1], "sku": rng.choice(skus), "qty": 1}
    mine = [o for o in data["orders"].values() if o["customer_id"] == target]
    total = sum(o["qty"] * data["skus"][o["sku"]]["price"] for o in mine)
    distinct = len({o["sku"] for o in mine})
    return Task(f"order_value-n{n}-s{seed}", "order_value", split, n, seed,
                f"What is the total value, in cents, of all orders placed by customer {target}? Each order's value is "
                "its quantity times the SKU's unit price. Answer with only the integer.", data, str(total),
                min_tool_calls=1 + n + distinct, min_dependent_rounds=3)


def chain(n, seed, split):
    rng = random.Random(f"chain:{n}:{seed}")
    ids = _ids(rng, "R", n + 3)
    data = {"records": {}}
    for i in range(n):
        data["records"][ids[i]] = {"value": rng.randrange(1, 999), "next": ids[i + 1] if i + 1 < n else None}
    for extra in ids[n:]:
        data["records"][extra] = {"value": rng.randrange(1, 999), "next": None}
    last = data["records"][ids[n - 1]]["value"]
    total = sum(data["records"][ids[i]]["value"] for i in range(n))
    return Task(f"chain-n{n}-s{seed}", "chain", split, n, seed,
                f"Start at record {ids[0]} and follow each record's `next` link until `next` is null. Answer exactly "
                "as '<value of the last record>,<sum of the values of every record visited>'.", data,
                f"{last},{total}", min_tool_calls=n, min_dependent_rounds=n)


def triage(n, seed, split):
    """Fan-out reads, a condition per item, then writes for the matching ones."""
    rng = random.Random(f"triage:{n}:{seed}")
    tids = _ids(rng, "T", n + 2)
    cust = _ids(rng, "C", 3)
    limit = rng.choice([14, 21, 30])
    tickets = {}
    for t in tids[:n]:
        tickets[t] = {"customer_id": rng.choice(cust), "priority": rng.choice(["low", "normal", "high"]),
                      "age_days": rng.randrange(1, 60), "status": "open"}
    for t in tids[n:]:
        tickets[t] = {"customer_id": cust[0], "priority": "low", "age_days": 90, "status": "closed"}
    if not any(v["age_days"] > limit for v in tickets.values() if v["status"] == "open"):
        tickets[tids[0]]["age_days"] = limit + 5
    stale = [t for t, v in tickets.items() if v["status"] == "open" and v["age_days"] > limit]
    data = {"tickets": tickets, "customers": {c: {"name": rng.choice(NAMES), "tier": "silver", "region": "east"}
                                              for c in cust}}
    return Task(f"triage-n{n}-s{seed}", "triage", split, n, seed,
                f"Close every open ticket that is more than {limit} days old (age_days > {limit}) by setting its status "
                "to 'closed'. Leave every other ticket unchanged. Answer with only the number of tickets you closed.",
                data, str(len(stale)), tuple(sorted(("update_ticket", t, "closed") for t in stale)),
                min_tool_calls=1 + n + len(stale), min_dependent_rounds=3)


def notify_gold(n, seed, split):
    """HELD-OUT family. Fan-out, condition, second dependent lookup, de-duplication, writes."""
    rng = random.Random(f"notify_gold:{n}:{seed}")
    tids = _ids(rng, "T", n)
    cust = _ids(rng, "C", max(3, n // 2))
    customers = {c: {"name": rng.choice(NAMES), "tier": rng.choice(["gold", "silver", "bronze"]),
                     "region": rng.choice(["north", "south"])} for c in cust}
    customers[cust[0]]["tier"] = "gold"
    tickets = {t: {"customer_id": rng.choice(cust), "priority": rng.choice(["high", "normal", "low"]),
                   "age_days": rng.randrange(1, 40), "status": "open"} for t in tids}
    tickets[tids[0]].update(customer_id=cust[0], priority="high")
    targets = sorted({v["customer_id"] for v in tickets.values()
                      if v["priority"] == "high" and customers[v["customer_id"]]["tier"] == "gold"})
    text = "We are on it."
    high_owners = {v["customer_id"] for v in tickets.values() if v["priority"] == "high"}
    return Task(f"notify_gold-n{n}-s{seed}", "notify_gold", split, n, seed,
                "Find every open ticket with priority 'high'. For each distinct gold-tier customer who owns at least one "
                f"of them, send exactly one message with the text '{text}'. Send nothing to anyone else. "
                "Answer with only the number of messages you sent.",
                {"tickets": tickets, "customers": customers}, str(len(targets)),
                tuple(sorted(("send_message", c, text) for c in targets)),
                min_tool_calls=1 + n + len(high_owners) + len(targets), min_dependent_rounds=4)


def top_customer(n, seed, split):
    """HELD-OUT family. Fan-out over a given list, aggregation, then a dependent lookup."""
    rng = random.Random(f"top_customer:{n}:{seed}")
    cust = _ids(rng, "C", n)
    names = rng.sample(NAMES, n) if n <= len(NAMES) else [f"{rng.choice(NAMES)} {i}" for i in range(n)]
    customers = {c: {"name": names[i], "tier": "silver", "region": "west"} for i, c in enumerate(cust)}
    counts = rng.sample(range(1, 3 * n + 2), n)
    orders = {}
    for c, k in zip(cust, counts):
        for oid in _ids(rng, "O" + c[-2:], k, width=5):
            orders[oid] = {"customer_id": c, "sku": "SKU-1000", "qty": 1}
    best = cust[counts.index(max(counts))]
    return Task(f"top_customer-n{n}-s{seed}", "top_customer", split, n, seed,
                f"Among the customers {', '.join(cust)}, which one has placed the most orders? Answer with only that "
                "customer's name.", {"customers": customers, "orders": orders,
                                     "skus": {"SKU-1000": {"on_hand": 1, "price": 100}}},
                customers[best]["name"], min_tool_calls=n + 1, min_dependent_rounds=2)


FAMILIES = {f.__name__: f for f in (fanout_sum, partial_failure, order_value, chain, triage, notify_gold, top_customer)}


def make(family, n, seed, split):
    return FAMILIES[family](n, seed, split)


# ---- frozen suites ---------------------------------------------------------------------------------
# DEV: used while building the harness and while changing Weave's prompt/listing/errors.
DEV = tuple(make(f, n, s, "dev") for f, n, s in [
    ("fanout_sum", 8, 1), ("partial_failure", 8, 1), ("order_value", 5, 1),
    ("chain", 4, 1), ("triage", 8, 1),
])

# HELD-OUT: pre-registered in docs/paper/preregistration.md before any live run on it. New seeds and sizes
# for the dev families plus two families (notify_gold, top_customer) never run during development.
HELD_OUT = tuple(make(f, n, s, "held_out") for f, n, s in [
    ("fanout_sum", 20, 101), ("fanout_sum", 12, 102),
    ("partial_failure", 16, 101),
    ("order_value", 10, 101),
    ("chain", 6, 101),
    ("triage", 15, 101),
    ("notify_gold", 12, 101), ("notify_gold", 20, 102),
    ("top_customer", 8, 101), ("top_customer", 15, 102),
])

# SCALING: fan-out width and chain depth sweeps (seed 7 throughout).
SCALING = tuple([make("fanout_sum", n, 7, "scaling") for n in (5, 10, 20, 40)]
                + [make("chain", n, 7, "scaling") for n in (2, 4, 8)])

SUITES = {"dev": DEV, "held_out": HELD_OUT, "scaling": SCALING}


def manifest(suite):
    tasks = SUITES[suite]
    return {"suite": suite, "tasks": [{"id": t.id, "family": t.family, "size": t.size, "fingerprint": t.fingerprint()}
                                      for t in tasks],
            "hash": hashlib.sha256("".join(t.fingerprint() for t in tasks).encode()).hexdigest()[:16]}
