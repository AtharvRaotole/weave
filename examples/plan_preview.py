"""See what a script will do before any tool runs: `WeaveTool.plan` and `approve=`.

Offline. The two scripts stand in for what a model would send to `run_weave`.
"""

import os
import time

import weave

PAUSE = float(os.environ.get("WEAVE_DEMO_PAUSE", "0"))  # only for the README recording

CALLS = []


@weave.declare(effect="read", arity=1)
def get_ticket(ticket_id: str) -> dict:
    """Load a support ticket."""
    CALLS.append(f"get_ticket({ticket_id})")
    return {"id": ticket_id, "order": "o-" + ticket_id[-1], "stale": ticket_id != "T-2"}


@weave.declare(effect="reversible", arity=1, writes=["ticket:{ticket_id}"])
def close_ticket(ticket_id: str) -> str:
    """Close a ticket."""
    CALLS.append(f"close_ticket({ticket_id})")
    return "closed"


@weave.declare(effect="irreversible", arity=1, writes=["payment:{order_id}"])
def refund(order_id: str) -> str:
    """Refund an order."""
    CALLS.append(f"refund({order_id})")
    return "refunded"


def policy(plan: dict):
    """Auto-approve writes whose targets are known up front; anything else needs a person."""
    print(plan["text"].rstrip())
    for call in plan["calls"]:
        writes = call["writes"]
        if call["effect"] == "irreversible":
            return f"{call['tool']} on line {call['line']} is irreversible and needs a person"
        if writes is not None and writes["kind"] != "names":
            return f"{call['tool']} on line {call['line']} writes targets only known at run time"
    return True


CLOSE_STALE = """\
tickets = [get_ticket(t) for t in ["T-1", "T-2", "T-3"]]
for t in ["T-1", "T-3"]:
    close_ticket(t)
print(len(tickets), "checked")
"""

REFUND_STALE = """\
tickets = [get_ticket(t) for t in ["T-1", "T-2", "T-3"]]
for t in tickets:
    if t["stale"]:
        refund(t["order"])
"""


def main():
    tool = weave.WeaveTool([get_ticket, close_ticket, refund], approve=policy)

    print("--- the model sends:")
    print(CLOSE_STALE, end="")
    time.sleep(PAUSE)
    print("--- Weave plans it, the policy approves, then it runs:")
    out = tool.run(CLOSE_STALE)
    print("result:", out.strip())
    assert out == "3 checked\n", out
    assert CALLS.count("close_ticket(T-1)") == 1 and len(CALLS) == 5, CALLS

    time.sleep(PAUSE)
    print("--- the model sends:")
    print(REFUND_STALE, end="")
    time.sleep(PAUSE)
    print("--- Weave plans it, the policy says no, nothing runs:")
    CALLS.clear()
    out = tool.run(REFUND_STALE)
    print("result:", out.strip())
    assert out.startswith("[not approved] Nothing ran"), out
    assert CALLS == [], CALLS  # not even the reads ran
    print("tool calls made:", len(CALLS))


if __name__ == "__main__":
    main()
