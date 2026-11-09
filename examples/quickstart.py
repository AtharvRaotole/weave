"""Zero-to-working Weave in one file. No API key or network needed.

    python examples/quickstart.py

Registers two Python functions as tools, runs a script that fans out reads and
loops over results, then replays the run from its trace without calling tools.
"""

import time

import weave

ORDERS = {"c9": ["o1", "o2", "o3"]}
TOTALS = {"o1": 12, "o2": 30, "o3": 7}


@weave.declare(effect="read", arity=1)
def list_orders(customer_id):
    """List order ids for a customer."""
    time.sleep(0.05)  # stand-in for an API call
    return ORDERS.get(customer_id, [])


@weave.declare(effect="read", arity=1)
def order_total(order_id):
    """Integer total of one order."""
    time.sleep(0.05)
    return TOTALS[order_id]


SCRIPT = """
total = 0
for order in list_orders("c9"):
    total += order_total(order)
print(total)
"""


def main():
    engine = weave.Engine()
    engine.install([list_orders, order_total])
    result = engine.run(SCRIPT)
    print(result.stdout, end="")  # 49
    assert result.stdout == "49\n", result.stdout

    # Replay reproduces the run from its trace; no tool is called again.
    replayed = engine.replay(SCRIPT, result.trace_json())
    assert replayed.stdout == result.stdout

    # The same functions as ONE function-calling tool for a model.
    tool = weave.WeaveTool([list_orders, order_total])
    assert tool.chat_tool()["function"]["name"] == "run_weave"
    print("quickstart ok")


if __name__ == "__main__":
    main()
