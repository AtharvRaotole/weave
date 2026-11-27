"""Declared return types: `weave.declare(returns=...)`, `Engine.tool(returns=...)`. No network."""

from __future__ import annotations

import pytest

import weave
from weave.agent_tool import shape_of_samples, tool_line

CALLS: list = []


@weave.declare(effect="read", returns='{"id": str, "status": str, "items": list[{"sku": str, "qty": int}], "note": str?}')
def get_order(order_id: str) -> dict:
    """Load an order."""
    CALLS.append(("get_order", order_id))
    return {"id": order_id, "status": "open", "items": [{"sku": "A1", "qty": 2}]}


@weave.declare(effect="irreversible", returns={"type": "object", "properties": {"refund_id": {"type": "string"}},
                                                "additionalProperties": False})
def refund(order_id: str) -> dict:
    """Refund an order."""
    CALLS.append(("refund", order_id))
    return {"refund_id": "R-" + order_id}


@pytest.fixture(autouse=True)
def _reset():
    CALLS.clear()


def test_bad_key_is_rejected_before_any_tool_runs():
    tool = weave.WeaveTool([get_order, refund])
    out = tool.run('refund("W1")\no = get_order("W1")\nprint(o["state"])\n')
    assert CALLS == []
    assert "get_order() has no key 'state'" in out and "did you mean 'status'?" in out


def test_loop_items_and_json_schema_results_are_checked():
    tool = weave.WeaveTool([get_order, refund])
    out = tool.run('for it in get_order("W1")["items"]:\n    print(it["quantity"])\nprint(refund("W1")["id"])\n')
    assert CALLS == []
    assert "an item of get_order()['items'] has no key 'quantity'" in out
    assert "refund() has no key 'id'; its declared keys are 'refund_id'" in out


def test_correct_scripts_run_unchanged():
    tool = weave.WeaveTool([get_order, refund])
    out = tool.run('o = get_order("W1")\nprint(o["status"], [i["qty"] for i in o["items"]], o.get("note"))\n')
    assert out == "open [2] None\n"
    assert CALLS == [("get_order", "W1")]


def test_listing_shows_the_declared_shape():
    assert '-> {"id": str, "status": str' in tool_line(get_order)
    assert "-> {\"refund_id\": str?}" in tool_line(refund)


def test_display_only_returns_are_not_checked():
    # `__weave_returns__` alone stays a hint for the model, as before.
    def get_user(user_id: str) -> dict:
        CALLS.append(("get_user", user_id))
        return {"id": user_id, "tier": "gold"}

    get_user.__weave_returns__ = shape_of_samples([{"id": "u1"}])
    tool = weave.WeaveTool([weave.declare(effect="read")(get_user)])
    assert tool.run('print(get_user("u1")["tier"])\n') == "gold\n"


def test_explicit_returns_keep_a_custom_display():
    def f(x: str) -> dict:
        return {"a": 1}

    f.__weave_returns__ = "an A record"
    weave.declare(effect="read", returns='{"a": int}')(f)
    assert f.__weave_returns__ == "an A record"


def test_open_shapes_allow_other_keys():
    @weave.declare(effect="read", returns='{"id": str, ...}')
    def get_doc(doc_id: str) -> dict:
        return {"id": doc_id, "title": "t"}

    assert weave.WeaveTool([get_doc]).run('print(get_doc("d")["title"])\n') == "t\n"


def test_engine_tool_and_engine_declare_accept_returns():
    engine = weave.Engine()
    engine.tool("lookup", lambda k: [k], effect="read", returns="list[str]")

    @engine.declare(effect="read", returns='{"n": int}')
    def count(x: str) -> dict:
        return {"n": 1}

    diags = engine.check('print(lookup("a")["x"], count("b")["m"])\n')
    assert len(diags) == 2
    assert "can't look up 'x' in lookup(): it is declared as a list" in diags[0]
    assert "count() has no key 'm'" in diags[1]
    assert count.__weave_returns__ == '{"n": int}'


@pytest.mark.parametrize("bad", ["{'id': str}", "Order", 42, ["list"]])
def test_bad_returns_fail_at_declaration(bad):
    with pytest.raises(ValueError, match="returns="):
        weave.declare(effect="read", returns=bad)(lambda x: x)
    with pytest.raises(ValueError, match="returns="):
        weave.Engine().tool("t", lambda x: x, effect="read", returns=bad)
