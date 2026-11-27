"""Static plans (`Engine.plan`, `WeaveTool.plan`) and `WeaveTool(approve=...)`. No network."""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

import weave

CALLS: list = []


@weave.declare(effect="read", arity=1)
def get_ticket(ticket_id: str) -> dict:
    """Load a ticket."""
    CALLS.append(("get_ticket", ticket_id))
    return {"id": ticket_id, "owner": "ana" if ticket_id != "T-3" else "bo"}


@weave.declare(effect="reversible", writes=["ticket:{0}"])
def close_ticket(ticket_id: str, note: str = "") -> str:
    """Close a ticket."""
    CALLS.append(("close_ticket", ticket_id))
    return "closed"


@weave.declare(effect="irreversible")
def send_email(to: str, body: str) -> str:
    """Send an email."""
    CALLS.append(("send_email", to))
    return "sent"


SCRIPT = (
    'tickets = [get_ticket(t) for t in ["T-1", "T-2", "T-3"]]\n'
    "for t in tickets:\n"
    '    if t["owner"] == "bo":\n'
    '        close_ticket(t["id"], note="stale")\n'
    'send_email("ops@example.com", f"closed {len(tickets)}")\n'
)


@pytest.fixture(autouse=True)
def _reset():
    CALLS.clear()


def test_engine_plan_lists_sites_without_running_anything():
    engine = weave.Engine()
    engine.install([get_ticket, close_ticket, send_email])
    plan = engine.plan(SCRIPT)
    assert CALLS == []
    assert [(c["tool"], c["min_count"], c["max_count"]) for c in plan["calls"]] == [
        ("get_ticket", 3, 3),
        ("close_ticket", 0, 3),
        ("send_email", 1, 1),
    ]
    get, close, send = plan["calls"]
    assert get["args"][0]["source"] == {"kind": "literal", "values": ["'T-1'", "'T-2'", "'T-3'"]}
    assert close["args"][0] == {"name": "ticket_id", "position": 0, "source": {"kind": "tools", "calls": [0]}}
    assert close["args"][1]["name"] == "note" and close["args"][1]["source"]["values"] == ["'stale'"]
    assert close["writes"] == {"kind": "templates", "templates": ["ticket:{0}"]}
    assert close["control_from"] == [0] and close["stage"] == 2
    assert send["args"][0]["source"] == {"kind": "literal", "values": ["'ops@example.com'"]}
    assert send["writes"] == {"kind": "all"} and get["writes"] is None
    assert (plan["min_calls"], plan["max_calls"], plan["stages"]) == (4, 7, 2)
    assert plan["max_effect"] == "irreversible" and plan["may_write"] is True
    assert "close_ticket(<#1>, note='stale') reversible x0-3 stage 2" in plan["text"]


def test_engine_plan_raises_like_run_on_a_bad_script():
    engine = weave.Engine()
    engine.install([get_ticket])
    with pytest.raises(Exception, match="get_tiket"):
        engine.plan('get_tiket("T-1")\n')
    assert CALLS == []


def test_approve_true_runs_and_sees_the_plan():
    seen = []
    tool = weave.WeaveTool([get_ticket, close_ticket, send_email], approve=lambda p: seen.append(p) or True)
    out = tool.run(SCRIPT + "print('done')\n")
    assert out == "done\n"
    assert len(seen) == 1 and seen[0]["may_write"]
    assert ("close_ticket", "T-3") in CALLS and ("send_email", "ops@example.com") in CALLS


@pytest.mark.parametrize("verdict, reason", [(False, "the plan was declined"), (None, "the plan was declined"),
                                             ("no emails today.", "no emails today")])
def test_declined_plan_calls_no_tool(verdict, reason):
    tool = weave.WeaveTool([get_ticket, close_ticket, send_email], approve=lambda p: verdict)
    out = tool.run(SCRIPT)
    assert CALLS == []  # not even the reads
    assert out.startswith("[not approved] Nothing ran")
    assert f"Reason: {reason}." in out and "close_ticket, send_email" in out


def test_approval_that_raises_fails_closed():
    def broken(plan):
        raise RuntimeError("ui offline")

    out = weave.WeaveTool([get_ticket, send_email], approve=broken).run('send_email("a", "b")\n')
    assert CALLS == [] and "ui offline" in out and out.startswith("[not approved]")


def test_truthy_but_not_true_does_not_approve():
    out = weave.WeaveTool([send_email], approve=lambda p: "yes").run('send_email("a", "b")\n')
    assert CALLS == [] and "Reason: yes" in out


def test_read_only_scripts_skip_approval():
    asked = []
    tool = weave.WeaveTool([get_ticket, send_email], approve=lambda p: asked.append(p) or False)
    assert tool.run('print(get_ticket("T-1")["owner"])\n') == "ana\n"
    assert asked == []


def test_bad_script_reports_the_check_error_not_an_approval():
    asked = []
    tool = weave.WeaveTool([send_email], approve=lambda p: asked.append(p) or True)
    out = tool.run('send_emial("a", "b")\n')
    assert out.startswith("error:") and "send_emial" in out and asked == []


def test_plan_lines_match_the_models_script_with_persisted_variables():
    tool = weave.WeaveTool([get_ticket, send_email], persist_variables=True, approve=lambda p: True)
    tool.run('who = "T-2"\n')
    plan = tool.plan('x = 1\nsend_email(who, "hi")\n')
    assert plan["calls"][0]["line"] == 2
    assert "line 2:" in plan["text"]
    assert plan["calls"][0]["args"][0]["source"] == {"kind": "literal", "values": ["'T-2'"]}


def test_approve_works_with_the_write_gate():
    tool = weave.WeaveTool([get_ticket, close_ticket], write_gate="choice", approve=lambda p: False)
    out = tool.run('close_ticket("T-1")\n')
    assert CALLS == [] and out.startswith("[not approved]")


def test_approve_needs_an_engine_that_can_plan():
    class NoPlan:
        def check(self, source):
            return []

        def run(self, source):
            raise AssertionError

    with pytest.raises(ValueError, match="plan"):
        weave.WeaveTool([send_email], engine=NoPlan(), approve=lambda p: True)


def test_cli_plan_text_and_json(tmp_path: Path):
    script = tmp_path / "s.py"
    script.write_text('for t in ["T-1", "T-2"]:\n    close(t)\n')
    tool = "name=close,effect=reversible,writes=ticket:{0}"
    env = {"PYTHONPATH": str(Path(weave.__file__).parent.parent)}
    text = subprocess.run([sys.executable, "-m", "weave", "plan", str(script), "--tool", tool],
                          capture_output=True, text=True, env=env, check=True).stdout
    assert "close('T-1' | 'T-2') reversible x2 stage 1, writes ticket:T-1, ticket:T-2" in text
    data = json.loads(subprocess.run([sys.executable, "-m", "weave", "plan", str(script), "--tool", tool, "--json"],
                                     capture_output=True, text=True, env=env, check=True).stdout)
    assert data["calls"][0]["writes"] == {"kind": "names", "names": ["ticket:T-1", "ticket:T-2"]}
    assert "text" not in data
