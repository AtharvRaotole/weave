"""Use Weave as one `run_weave` tool inside an existing agent stack.

Instead of exposing N tools and paying one model round trip per call, expose
one tool that takes a Weave script. Independent calls in the script run in
parallel, control flow runs locally, and the model sees one result.

    tool = WeaveTool([get_user, rename])          # @weave.declare'd functions
    client.chat.completions.create(..., tools=[tool.chat_tool()])
    messages.append(tool.handle(tool_call))       # chat tool_call -> tool message

    client.responses.create(..., tools=[tool.responses_tool()])
    items.append(tool.handle(function_call_item)) # -> function_call_output

    agent = Agent(..., tools=[tool.agents_tool()])  # OpenAI Agents SDK, optional
"""

from __future__ import annotations

import asyncio
import inspect
import json
from typing import Any, Callable, Iterable

from weave._native import Engine

RULES = (
    "Write a Weave script: a Python subset. No imports, def, class, while, with, lambda, "
    "or comprehensions. Use assignment, calls, if/else, for (with break/continue), try/except ToolError, lists, dicts, "
    "f-strings, string/list/dict methods, and builtins (str, int, len, range, sum, min, max, "
    "sorted, enumerate, zip, any, all, list, parse_json). Independent calls on separate lines run "
    "in parallel. `for x in text` loops over characters; use text.splitlines() for lines. "
    "A failing tool raises ToolError. print() the values you need; the tool returns "
    "what the script printed, or the error with a hint so you can fix the script."
)


def _signature(func: Callable) -> str:
    try:
        params = inspect.signature(func).parameters.values()
    except (TypeError, ValueError):
        return "..."
    return ", ".join(p.name if p.default is inspect.Parameter.empty else f"{p.name}={p.default!r}" for p in params)


def tool_line(func: Callable) -> str:
    meta = getattr(func, "__weave__", None) or {}
    effect = meta.get("effect") or "irreversible"
    doc = (inspect.getdoc(func) or "").split("\n", 1)[0]
    return f"{func.__name__}({_signature(func)}), effect {effect}. {doc}".rstrip()


class WeaveTool:
    """A single function-calling tool that runs Weave scripts over `functions`."""

    def __init__(
        self,
        functions: Iterable[Callable],
        *,
        name: str = "run_weave",
        engine: Engine | None = None,
        max_output_chars: int = 20_000,
        worker_pool: int | None = None,
        worker_limits: Any = None,
        max_wall_time_ms: int | None = None,
        debug: bool = False,
    ):
        """`worker_pool=N` runs every function in killable worker processes
        (`Engine(worker_pool=N, isolate_all=True)`): a tool timeout or the run
        deadline kills the worker. Functions must then be module-level and
        pickleable; opt one out with `weave.declare(hard_deadline=False)`.
        `max_wall_time_ms` bounds each script run (default 30s)."""
        self.functions = list(functions)
        self.name = name
        self.max_output_chars = max_output_chars
        if engine is not None and (worker_pool is not None or worker_limits is not None or debug):
            raise ValueError("pass worker_pool/worker_limits/debug to the Engine you supply, not to WeaveTool")
        if engine is None:
            if worker_pool is not None:
                engine = Engine(worker_pool, isolate_all=True, worker_limits=worker_limits, debug=debug)
            else:
                if worker_limits is not None:
                    raise ValueError("worker_limits needs worker_pool=N")
                engine = Engine(debug=debug)
            engine.install(self.functions)
        self.engine = engine
        if max_wall_time_ms is not None:
            self.engine.set_limits(max_wall_time_ms=max_wall_time_ms)

    # ---- tool definitions -------------------------------------------------
    @property
    def description(self) -> str:
        lines = "\n".join(tool_line(f) for f in self.functions)
        return f"{RULES}\nFunctions callable from the script:\n{lines}"

    @property
    def parameters(self) -> dict:
        return {
            "type": "object",
            "properties": {"source": {"type": "string", "description": "The Weave script to run."}},
            "required": ["source"],
            "additionalProperties": False,
        }

    def chat_tool(self) -> dict:
        """Tool definition for Chat Completions."""
        return {"type": "function", "function": {"name": self.name, "description": self.description, "parameters": self.parameters}}

    def responses_tool(self) -> dict:
        """Tool definition for the Responses API."""
        return {"type": "function", "name": self.name, "description": self.description, "parameters": self.parameters, "strict": True}

    # ---- running ----------------------------------------------------------
    def run(self, source: str) -> str:
        """Check and run a script. Returns text for the model: printed output
        on success, or `error: ...` lines it can repair from."""
        if not isinstance(source, str):
            return 'error: source must be a string'
        if not source.endswith("\n"):
            source += "\n"
        problems = self.engine.check(source)
        if problems:
            return "error: the script did not pass the static check\n" + "\n".join(problems)
        try:
            out = self.engine.run(source).stdout
        except Exception as exc:  # noqa: BLE001 - every Weave failure goes back to the model
            return f"error: {exc}"
        if len(out) > self.max_output_chars:
            out = out[: self.max_output_chars] + f"\n[truncated to {self.max_output_chars} characters]"
        return out if out.strip() else "(the script printed nothing; print the values you need)"

    def handle(self, call: Any) -> dict:
        """Run one tool call and return the message to append.

        Accepts a Chat Completions tool call (dict or SDK object with
        `.function.arguments`) or a Responses `function_call` item."""
        get = (lambda obj, key: obj.get(key)) if isinstance(call, dict) else getattr
        function = get(call, "function") if (isinstance(call, dict) and "function" in call) or hasattr(call, "function") else None
        if function is not None:
            args = (function.get("arguments") if isinstance(function, dict) else function.arguments) or "{}"
            output = self._run_arguments(args)
            return {"role": "tool", "tool_call_id": get(call, "id"), "content": output}
        output = self._run_arguments(get(call, "arguments") or "{}")
        return {"type": "function_call_output", "call_id": get(call, "call_id"), "output": output}

    def _run_arguments(self, arguments: str) -> str:
        try:
            source = json.loads(arguments)["source"]
        except (json.JSONDecodeError, KeyError, TypeError):
            return 'error: call run_weave with JSON arguments {"source": "<script>"}'
        return self.run(source)

    def agents_tool(self):
        """A FunctionTool for the OpenAI Agents SDK (`pip install openai-agents`)."""
        from agents import FunctionTool  # imported lazily; the SDK is optional

        async def invoke(_ctx, arguments: str) -> str:
            # Do not block the SDK event loop. Cancellation abandons the await;
            # sync host work is soft-cancelled and may continue in this thread.
            return await asyncio.to_thread(self._run_arguments, arguments)

        return FunctionTool(
            name=self.name,
            description=self.description,
            params_json_schema=self.parameters,
            on_invoke_tool=invoke,
            strict_json_schema=True,
        )
