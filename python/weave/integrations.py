"""Adapters that put a `WeaveTool` into other agent stacks and MCP clients.

- `langchain_tool(tool)`: a LangChain `StructuredTool` (LangChain, LangGraph).
- `WeaveTool.agents_tool()`: an OpenAI Agents SDK `FunctionTool` (defined on the class).
- `mcp_server_entry(...)` / `install_mcp_config(...)`: the `mcpServers` entry that makes Claude Desktop, Cursor or any
  stdio MCP client launch `weave-mcp`; `weave mcp-config` on the command line.

Every optional dependency is imported lazily, so `import weave` never needs them.
"""

from __future__ import annotations

import json
import os
import platform
import shutil
from pathlib import Path
from typing import Any


def langchain_tool(tool):
    """Wrap a `weave.WeaveTool` as a LangChain `StructuredTool` with one `source` argument."""
    from langchain_core.tools import StructuredTool
    from pydantic import BaseModel, Field

    class WeaveScript(BaseModel):
        source: str = Field(description="The Weave script to run.")

    def run(source: str) -> str:
        return tool.run(source)

    async def arun(source: str) -> str:
        return await tool.run_async(source)

    return StructuredTool.from_function(func=run, coroutine=arun, name=tool.name, description=tool.description,
                                        args_schema=WeaveScript)


# ---- MCP clients ---------------------------------------------------------------------------------------------------

def mcp_config_path(client: str) -> Path:
    """Where `client` ("claude" for Claude Desktop, "cursor") reads its MCP servers on this OS."""
    home = Path.home()
    if client == "cursor":
        return home / ".cursor" / "mcp.json"
    if client == "claude":
        system = platform.system()
        if system == "Darwin":
            return home / "Library" / "Application Support" / "Claude" / "claude_desktop_config.json"
        if system == "Windows":
            return Path(os.environ.get("APPDATA", home / "AppData" / "Roaming")) / "Claude" / "claude_desktop_config.json"
        return home / ".config" / "Claude" / "claude_desktop_config.json"
    raise ValueError(f"unknown MCP client `{client}`; use `claude` or `cursor`")


def mcp_server_entry(config: str | os.PathLike | None = None, binary: str | None = None) -> dict[str, Any]:
    """The `mcpServers` entry that launches weave-mcp, with absolute paths (MCP clients do not use your shell's cwd).
    `config` is a weave-mcp config (tools and/or `upstreams`); `binary` defaults to `weave-mcp` on PATH."""
    found = binary or shutil.which("weave-mcp")
    if not found:
        raise FileNotFoundError("weave-mcp not found on PATH; install it with "
                                "`cargo install --locked --git https://github.com/AtharvRaotole/weave weave-mcp` "
                                "or pass --binary /path/to/weave-mcp")
    entry: dict[str, Any] = {"command": str(Path(found).expanduser().resolve())}
    if config is not None:
        path = Path(config).expanduser().resolve()
        if not path.exists():
            raise FileNotFoundError(f"config file {path} does not exist")
        entry["args"] = ["--config", str(path)]
    return entry


def install_mcp_config(client: str, entry: dict[str, Any], name: str = "weave", path: Path | None = None) -> Path:
    """Merge `entry` into the client's config file as `mcpServers[name]`, keeping every other server and setting.
    The previous file is kept next to it as `<file>.bak`."""
    path = path or mcp_config_path(client)
    data: dict[str, Any] = {}
    if path.exists():
        text = path.read_text(encoding="utf-8")
        data = json.loads(text) if text.strip() else {}
        path.with_name(path.name + ".bak").write_text(text, encoding="utf-8")
    data.setdefault("mcpServers", {})[name] = entry
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    return path
