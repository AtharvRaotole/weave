"""Offline tests for the agent-stack integrations: LangChain/LangGraph, the OpenAI Agents SDK (with a scripted
model, no network), and the `weave mcp-config` helper. Each section skips if its optional package is missing."""

import asyncio
import json
import os
import sys
import tempfile
from pathlib import Path
from unittest.mock import patch

import weave
from weave.cli import dispatch
from weave.integrations import install_mcp_config, mcp_config_path, mcp_server_entry


@weave.declare(effect="read", arity=1)
def stars(repo):
    """Star count of a repository."""
    return {"weave": 7, "rust": 99}[repo]


SCRIPT = "total = sum(stars(r) for r in ['weave', 'rust'])\nprint(total)\n"


def test_langchain_tool_and_langgraph():
    try:
        from langchain_core.messages import AIMessage, HumanMessage, ToolMessage
    except ImportError:
        print("skip: langchain-core not installed")
        return
    lc = weave.WeaveTool([stars]).langchain_tool()
    assert lc.name == "run_weave" and "stars(repo" in lc.description
    assert lc.invoke({"source": SCRIPT}).strip() == "106"
    assert asyncio.run(lc.ainvoke({"source": SCRIPT})).strip() == "106"
    try:
        from langgraph.graph import END, START, MessagesState, StateGraph
        from langgraph.prebuilt import ToolNode
    except ImportError:
        print("skip: langgraph not installed")
        return
    # A scripted model: first turn sends one Weave script, second turn answers from its output.
    def model(state):
        last = state["messages"][-1]
        if isinstance(last, ToolMessage):
            return {"messages": [AIMessage(content=f"total stars: {last.content.strip()}")]}
        return {"messages": [AIMessage(content="", tool_calls=[{"name": "run_weave", "args": {"source": SCRIPT},
                                                                   "id": "c1", "type": "tool_call"}])]}

    g = StateGraph(MessagesState)
    g.add_node("model", model)
    g.add_node("tools", ToolNode([lc]))
    g.add_edge(START, "model")
    g.add_conditional_edges("model", lambda s: "tools" if s["messages"][-1].tool_calls else END)
    g.add_edge("tools", "model")
    out = g.compile().invoke({"messages": [HumanMessage(content="How many stars in total?")]})
    assert out["messages"][-1].content == "total stars: 106", out["messages"][-1]


def test_agents_sdk_runner_with_scripted_model():
    try:
        from agents import Agent, Runner, set_tracing_disabled
        from agents.items import ModelResponse
        from agents.models.interface import Model
        from agents.usage import Usage
        from openai.types.responses import ResponseFunctionToolCall, ResponseOutputMessage, ResponseOutputText
    except ImportError:
        print("skip: openai-agents not installed")
        return

    class Scripted(Model):
        async def get_response(self, system_instructions, input, model_settings, tools, output_schema, handoffs,
                               tracing, **kw):
            outputs = [i for i in input if isinstance(i, dict) and i.get("type") == "function_call_output"]
            if outputs:
                item = ResponseOutputMessage(id="m1", type="message", role="assistant", status="completed", content=[
                    ResponseOutputText(type="output_text", text=f"total stars: {outputs[-1]['output'].strip()}",
                                       annotations=[])])
            else:
                assert [t.name for t in tools] == ["run_weave"]
                item = ResponseFunctionToolCall(id="f1", call_id="c1", type="function_call", name="run_weave",
                                                arguments=json.dumps({"source": SCRIPT}))
            return ModelResponse(output=[item], usage=Usage(), response_id=None)

        def stream_response(self, *a, **k):
            raise NotImplementedError

    set_tracing_disabled(True)
    agent = Agent(name="stars", instructions="Use run_weave.", tools=[weave.WeaveTool([stars]).agents_tool()],
                  model=Scripted())
    result = asyncio.run(Runner.run(agent, "How many stars in total?"))
    assert result.final_output == "total stars: 106", result.final_output


def test_mcp_config_entry_and_merge():
    with tempfile.TemporaryDirectory() as d:
        d = Path(d)
        binary = d / ("weave-mcp.exe" if os.name == "nt" else "weave-mcp")
        binary.write_text("")
        cfg = d / "tools.json"
        cfg.write_text('{"tools": []}')
        entry = mcp_server_entry(cfg, str(binary))
        assert entry == {"command": str(binary.resolve()), "args": ["--config", str(cfg.resolve())]}
        printed = json.loads(dispatch(["mcp-config", "--config", str(cfg), "--binary", str(binary)]))
        assert printed == {"mcpServers": {"weave": entry}}
        target = d / "claude_desktop_config.json"
        target.write_text(json.dumps({"mcpServers": {"other": {"command": "x"}}, "theme": "dark"}))
        install_mcp_config("claude", entry, path=target)
        merged = json.loads(target.read_text())
        assert merged["mcpServers"]["other"] == {"command": "x"} and merged["theme"] == "dark"
        assert merged["mcpServers"]["weave"] == entry
        assert json.loads((d / "claude_desktop_config.json.bak").read_text())["theme"] == "dark"
        try:
            mcp_server_entry(d / "missing.json", str(binary))
        except FileNotFoundError:
            pass
        else:
            raise AssertionError("missing config accepted")


def test_new_mcp_client_config_paths():
    with tempfile.TemporaryDirectory() as d:
        root = Path(d)
        data = root / "appdata"
        xdg = root / "xdg"
        for system, config_dir in (
            ("Linux", xdg),
            ("Darwin", root / "Library" / "Application Support"),
            ("Windows", data),
        ):
            with patch("weave.integrations.Path.home", return_value=root), \
                 patch("weave.integrations.Path.cwd", return_value=root / "project"), \
                 patch("weave.integrations.platform.system", return_value=system), \
                 patch.dict(os.environ, {"XDG_CONFIG_HOME": str(xdg), "APPDATA": str(data)}):
                assert mcp_config_path("vscode") == config_dir / "Code" / "User" / "mcp.json"
                assert mcp_config_path("claude-code") == root / "project" / ".mcp.json"
        with patch("weave.integrations.Path.home", return_value=root), \
             patch("weave.integrations.platform.system", return_value="Linux"), \
             patch.dict(os.environ, clear=True):
            assert mcp_config_path("vscode") == root / ".config" / "Code" / "User" / "mcp.json"


def test_new_mcp_clients_print_and_merge():
    with tempfile.TemporaryDirectory() as d:
        root = Path(d)
        binary = root / "weave-mcp"
        binary.write_text("")
        command = str(binary.resolve())
        for client, key, path, entry in (
            ("vscode", "servers", root / "mcp.json", {"command": command, "type": "stdio"}),
            ("claude-code", "mcpServers", root / ".mcp.json", {"command": command}),
        ):
            original = {key: {"other": {"command": "x"}}, "extra": {"keep": True}}
            path.write_text(json.dumps(original), encoding="utf-8")
            args = ["mcp-config", "--client", client, "--binary", str(binary), "--name", "my-weave"]
            with patch("weave.integrations.mcp_config_path", return_value=path):
                printed = dispatch(args).split("\n# paste", 1)[0]
                assert json.loads(printed) == {key: {"my-weave": entry}}
                assert json.loads(path.read_text(encoding="utf-8")) == original
                assert str(path) in dispatch(args + ["--write"])
            expected = {**original, key: {**original[key], "my-weave": entry}}
            assert json.loads(path.read_text(encoding="utf-8")) == expected
            backup = path.with_name(path.name + ".bak")
            assert json.loads(backup.read_text(encoding="utf-8")) == original


if __name__ == "__main__":
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            fn()
            print("ok", name)
    sys.exit(0)
