"""Declared error convention: `error_key=` on `weave.declare`, `Engine.tool` and `Engine.declare`. No network."""

from __future__ import annotations

import pytest

import weave

CALLS: list = []


@weave.declare(effect="read", error_key="error")
def find_car(plate: str) -> dict:
    """Look up a car; reports failure as {"error": "..."}."""
    CALLS.append(("find_car", plate))
    return {"error": "No car found"} if plate == "missing" else {"id": plate, "error": None}


@weave.declare(effect="irreversible")
def book(car_id: str) -> dict:
    """Book a car."""
    CALLS.append(("book", car_id))
    return {"booked": car_id}


@pytest.fixture(autouse=True)
def _reset():
    CALLS.clear()


def test_error_value_raises_at_the_call_and_later_writes_do_not_run():
    out = weave.WeaveTool([find_car, book]).run('c = find_car("missing")\nbook(c.get("id", "X"))\n')
    assert CALLS == [("find_car", "missing")]
    assert "find_car" in out and "No car found" in out


def test_error_value_is_catchable():
    src = ('try:\n    c = find_car("missing")\nexcept ToolError as e:\n    print("handled:", e)\n'
           'c = find_car("AB1")\nprint(book(c["id"]))\n')
    out = weave.WeaveTool([find_car, book]).run(src)
    assert "handled:" in out and "No car found" in out
    assert "{'booked': 'AB1'}" in out
    # The two reads are independent and may run in either order; the write comes last.
    assert sorted(CALLS[:2]) == [("find_car", "AB1"), ("find_car", "missing")] and CALLS[2] == ("book", "AB1")


def test_none_under_the_key_is_a_normal_result():
    out = weave.WeaveTool([find_car]).run('print(find_car("AB1"))\n')
    assert out == "{'id': 'AB1', 'error': None}\n"


def test_engine_tool_and_engine_declare():
    def lookup(x):
        return {"err": f"bad {x}"} if x == "bb" else {"ok": x}

    for register in ("tool", "declare"):
        e = weave.Engine()
        if register == "tool":
            e.tool("lookup", lookup, effect="read", error_key="err")
        else:
            e.declare(effect="read", error_key="err")(lookup)
        assert e.run('print(lookup("a"))\n').stdout == "{'ok': 'a'}\n"
        with pytest.raises(ValueError, match="tool `lookup` failed: bad bb"):
            e.run('lookup("bb")\n')


def test_undeclared_tools_keep_error_values():
    e = weave.Engine()
    e.tool("lookup", lambda: {"error": "x"}, effect="read")
    assert e.run('print(lookup())\n').stdout == "{'error': 'x'}\n"


def test_empty_key_is_rejected():
    with pytest.raises(ValueError):
        weave.declare(effect="read", error_key="")
    with pytest.raises(ValueError):
        weave.Engine().tool("f", lambda: 1, effect="read", error_key="")


def test_write_gate_still_pauses_a_write_after_a_caught_error_value():
    tool = weave.WeaveTool([find_car, book], write_gate="errors")
    out = tool.run('try:\n    find_car("missing")\nexcept ToolError:\n    pass\nbook("AB1")\n')
    assert CALLS == [("find_car", "missing")]
    assert out.startswith("[paused before book('AB1')")
