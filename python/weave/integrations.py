"""Adapters that put a `WeaveTool` into other agent stacks and MCP clients.

- `langchain_tool(tool)`: a LangChain `StructuredTool` (LangChain, LangGraph).
- `WeaveTool.agents_tool()`: an OpenAI Agents SDK `FunctionTool` (defined on the class).
- `mcp_server_entry(...)` / `install_mcp_config(...)`: configure stdio MCP clients to launch `weave-mcp`;
  `weave mcp-config` on the command line.

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

MCP_CLIENTS = ("claude", "cursor", "vscode", "claude-code")


def mcp_config_path(client: str) -> Path:
    """Return the client's profile config, or the project config for Claude Code."""
    home = Path.home()
    if client == "cursor":
        return home / ".cursor" / "mcp.json"
    if client == "claude-code":
        return Path.cwd() / ".mcp.json"
    system = platform.system()
    if client == "vscode":
        if system == "Darwin":
            config_dir = home / "Library" / "Application Support"
        elif system == "Windows":
            config_dir = Path(os.environ.get("APPDATA") or home / "AppData" / "Roaming")
        else:
            config_dir = Path(os.environ.get("XDG_CONFIG_HOME") or home / ".config")
        return config_dir / "Code" / "User" / "mcp.json"
    if client == "claude":
        if system == "Darwin":
            return home / "Library" / "Application Support" / "Claude" / "claude_desktop_config.json"
        if system == "Windows":
            return Path(os.environ.get("APPDATA") or home / "AppData" / "Roaming") / "Claude" / "claude_desktop_config.json"
        return home / ".config" / "Claude" / "claude_desktop_config.json"
    choices = ", ".join(MCP_CLIENTS)
    raise ValueError(f"unknown MCP client `{client}`; choose from {choices}")


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


def mcp_config_fragment(client: str | None, name: str, entry: dict[str, Any]) -> dict[str, Any]:
    """Wrap an entry in the client's configuration schema."""
    if client == "vscode":
        return {"servers": {name: {**entry, "type": "stdio"}}}
    return {"mcpServers": {name: entry}}


def install_mcp_config(client: str, entry: dict[str, Any], name: str = "weave", path: Path | None = None) -> Path:
    """Merge `entry` into the client's config file, keeping every other server and setting.
    A `.bak` copy of the previous file is written if that backup does not already exist."""
    path = path or mcp_config_path(client)
    data: dict[str, Any] = {}
    if path.exists():
        text = path.read_text(encoding="utf-8")
        data = json.loads(text) if text.strip() else {}
        backup = path.with_name(path.name + ".bak")
        if not backup.exists():
            backup.write_text(text, encoding="utf-8")
    for key, servers in mcp_config_fragment(client, name, entry).items():
        existing = data.setdefault(key, {})
        if not isinstance(existing, dict):
            raise ValueError(
                f"{path} has `{key}` as {type(existing).__name__}, expected a JSON object of servers"
            )
        existing.update(servers)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    return path
