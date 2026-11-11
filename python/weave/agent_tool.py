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
import threading
from typing import Any, Callable, Iterable

from weave._native import Engine

RULES = (
    "Do the whole task in as few scripts as possible: one script can call many functions, loop over results, "
    "branch on them and aggregate, and each call of this tool costs a model round trip. "
    "Call this tool once per turn: put the reads, the decisions and the writes in that one script instead of "
    "splitting them across parallel calls (scripts do not share variables). "
    "Print only the values you need (the final answer, or what you must see to decide the next step), "
    "not whole intermediate results. "
    "Write a Weave script: a Python subset. No imports, class, while, with, or recursion "
    "(for a loop of unknown length use `for _ in range(100):` with break). "
    "Call functions by their bare names, e.g. `get(x)`, not `functions.get(x)`. Use assignment, calls, if/else, for (with break/continue; over a dict it yields keys), "
    "try/except (Exception, ToolError, KeyError, ...), raise, assert, isinstance, lists, dicts, "
    "list/dict/set comprehensions, generator expressions, `lambda` as `key=` or in map()/filter(), "
    "top-level `def` helpers (they may not change their arguments in place), "
    "f-strings (with format specs), str.format, string/list/dict methods, and builtins (str, int, round, len, range, "
    "sum, min, max, sorted, enumerate, zip, any, all, list, dict, set, map, filter, json.dumps, parse_json). Independent calls on separate lines run "
    "in parallel. `for x in text` loops over characters; use text.splitlines() for lines. "
    "A loop's tool calls run in parallel when their arguments use only the loop variable and values "
    "from before the loop, even if the loop accumulates results, so loop over items freely. "
    "A function's return type follows `->`. "
    "A failing tool raises ToolError. print() the values you need; the tool returns "
    "what the script printed, or the error with a hint so you can fix the script."
)

# For hosts where a person replies between turns (support agents, assistants): every script is a model
# round trip the person waits for, so a script should fetch everything the next reply needs. Writes still
# wait for the person's explicit go-ahead, which is the host policy's job to require; these rules only
# keep the script from jumping ahead of it.
CONVERSATION_RULES = (
    "A person is waiting for your reply, so make each script fetch everything that reply needs: chain dependent "
    "lookups in one script (find an id, fetch its record, then fetch the related records you may discuss, e.g. "
    "every order a user has) and print a compact summary of the relevant fields, not whole records. "
    "Never call a function that changes state before the person has explicitly confirmed that exact change; "
    "once they confirm, make the change and print its result in one script."
)


def shape_of(value: Any, depth: int = 0) -> str:
    """A short Python-like type for a sample result, for `__weave_returns__`.

    `shape_of({"id": "W1", "items": [{"price": 9.5}]})` is `{"id": str, "items": list[{"price": float}]}`.
    A dict whose values all share one dict shape (records keyed by id) renders as `dict[str, {...}]`."""
    if depth > 5:
        return "any"
    if value is None:
        return "None"
    if isinstance(value, bool):
        return "bool"
    if isinstance(value, int):
        return "int"
    if isinstance(value, float):
        return "float"
    if isinstance(value, str):
        return "str"
    if isinstance(value, (list, tuple)):
        if not value:
            return "list"
        if all(isinstance(v, dict) and v for v in value):
            return f"list[{_merged_shape(list(value[:20]), depth + 1)}]"
        shapes = list(dict.fromkeys(shape_of(v, depth + 1) for v in value[:20]))
        return f"list[{' | '.join(shapes[:3])}]"
    if isinstance(value, dict):
        if not value:
            return "dict"
        vals = list(value.values())
        id_keys = all(isinstance(k, str) and any(c.isdigit() for c in k) for k in value)
        if all(isinstance(v, dict) and v for v in vals) and id_keys:
            return f"dict[str, {_merged_shape(vals[:20], depth + 1)}]"
        if all(isinstance(v, dict) for v in vals) and len(value) >= 2:
            shapes = list(dict.fromkeys(shape_of(v, depth + 1) for v in vals[:20]))
            if len(shapes) == 1:
                return f"dict[str, {shapes[0]}]"
        if len(value) >= 2 and all(isinstance(v, str) for v in vals) and all(isinstance(k, str) for k in value) \
                and len(value) > 6:
            return "dict[str, str]"
        return "{" + ", ".join(f"{json.dumps(str(k))}: {shape_of(v, depth + 1)}" for k, v in value.items()) + "}"
    return type(value).__name__


def _merged_shape(dicts: list, depth: int) -> str:
    """One record shape for sibling dicts: a key missing from some gets `?`, and a field holding str-valued dicts
    whose keys differ between siblings (free-form options) is `dict[str, str]`."""
    keys = list(dict.fromkeys(k for d in dicts for k in d))
    fields = []
    for k in keys:
        vals = [d[k] for d in dicts if k in d]
        if all(isinstance(v, dict) and all(isinstance(x, str) for x in v.values()) for v in vals) \
                and len({tuple(v) for v in vals}) > 1:
            shape = "dict[str, str]"
        elif all(isinstance(v, dict) and v for v in vals) and len(vals) > 1:
            shape = _merged_shape(vals, depth + 1)
        else:
            shape = " | ".join(list(dict.fromkeys(shape_of(v, depth + 1) for v in vals))[:3])
        fields.append(f"{json.dumps(str(k))}: {shape}" + ("" if len(vals) == len(dicts) else "?"))
    return "{" + ", ".join(fields) + "}"


def echo_last_expression(source: str) -> str:
    """Print the value of a script's final bare expression, like a REPL.

    Models often end a script with `get_customer(cid)` and expect to see the result; without this
    the value is discarded and the model guesses (seen live with GPT-4.1). Returns the source
    unchanged when the last statement is not a lone top-level expression."""
    import ast

    try:
        tree = ast.parse(source)
    except SyntaxError:
        return source
    if not tree.body or not isinstance(tree.body[-1], ast.Expr):
        return source
    node = tree.body[-1].value
    if isinstance(node, ast.Constant) or (
        isinstance(node, ast.Call) and isinstance(node.func, ast.Name) and node.func.id == "print"
    ):
        return source
    lines = source.splitlines(keepends=True)
    first, last = tree.body[-1].lineno, tree.body[-1].end_lineno
    segment = ast.get_source_segment(source, tree.body[-1])
    if segment is None or "".join(lines[first - 1:last]).strip() != segment.strip():
        return source  # shares a line with another statement
    return "".join(lines[: first - 1]) + f"print({segment})\n" + "".join(lines[last:])


def _annotation(annotation: Any) -> str | None:
    if annotation is inspect.Parameter.empty or annotation is inspect.Signature.empty:
        return None
    if isinstance(annotation, str):
        return annotation
    if annotation is type(None):
        return "None"
    if isinstance(annotation, type) and not getattr(annotation, "__args__", None):
        return annotation.__name__
    return str(annotation).replace("typing.", "")


def schema_type(schema: Any, depth: int = 0) -> str:
    """Render a JSON schema as a short Python-like type, e.g. `{"value": str}`."""
    if not isinstance(schema, dict) or depth > 4:
        return "any"
    if "enum" in schema and isinstance(schema["enum"], list):
        return " | ".join(json.dumps(v) for v in schema["enum"][:8])
    kind = schema.get("type")
    if isinstance(kind, list):
        return " | ".join(schema_type({**schema, "type": k}, depth) for k in kind)
    if kind == "object" or "properties" in schema:
        props = schema.get("properties") or {}
        if not props:
            return "dict"
        required = set(schema.get("required") or ())
        fields = ", ".join(
            f'"{k}": {schema_type(v, depth + 1)}' + ("" if k in required else "?") for k, v in props.items()
        )
        return "{" + fields + "}"
    if kind == "array":
        return f"list[{schema_type(schema.get('items'), depth + 1)}]"
    return {"string": "str", "integer": "int", "number": "float", "boolean": "bool", "null": "None"}.get(kind, "any")


def _returns(func: Callable) -> str | None:
    declared = getattr(func, "__weave_returns__", None)
    if isinstance(declared, dict):
        return schema_type(declared)
    if isinstance(declared, str):
        return declared
    try:
        return _annotation(inspect.signature(func).return_annotation)
    except (TypeError, ValueError):
        return None


def _signature(func: Callable) -> str:
    try:
        params = inspect.signature(func).parameters.values()
    except (TypeError, ValueError):
        return "..."
    out = []
    for p in params:
        if p.kind in (p.VAR_POSITIONAL, p.VAR_KEYWORD) or p.name == "cancel":
            continue
        text = p.name
        hint = _annotation(p.annotation)
        if hint:
            text += f": {hint}"
        if p.default is not inspect.Parameter.empty:
            text += f"={p.default!r}"
        out.append(text)
    return ", ".join(out)


def tool_line(func: Callable) -> str:
    """One listing line: `name(params) -> returns, effect E. First doc line.`

    The return type comes from `func.__weave_returns__` (a JSON schema or a
    string) or the return annotation. A script cannot see a result's shape
    the way a native tool call shows the model each JSON result, so listing
    it avoids a wasted round trip (for example treating a `{"value": str}`
    envelope as a plain string)."""
    meta = getattr(func, "__weave__", None) or {}
    effect = meta.get("effect") or "irreversible"
    doc = (inspect.getdoc(func) or "").split("\n", 1)[0]
    returns = _returns(func)
    arrow = f" -> {returns}" if returns else ""
    return f"{func.__name__}({_signature(func)}){arrow}, effect {effect}. {doc}".rstrip()


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
        conversational: bool = False,
    ):
        """`worker_pool=N` runs every function in killable worker processes
        (`Engine(worker_pool=N, isolate_all=True)`): a tool timeout or the run
        deadline kills the worker. Functions must then be module-level and
        pickleable; opt one out with `weave.declare(hard_deadline=False)`.
        `max_wall_time_ms` bounds each script run (default 30s).
        `conversational=True` adds `CONVERSATION_RULES` to the description, for hosts where a person
        replies between turns."""
        self.functions = list(functions)
        self.conversational = conversational
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
        rules = f"{RULES} {CONVERSATION_RULES}" if self.conversational else RULES
        return f"{rules}\nFunctions callable from the script:\n{lines}"

    @property
    def request_options(self) -> dict:
        """Request settings to send with this tool, e.g. ``client.responses.create(...,
        **tool.request_options)``.

        Weave already runs independent calls in parallel inside one script, so the model gains
        nothing from several parallel calls of this tool, and splitting the work loses it: scripts
        do not share variables. Measured on the round-4 dev suite (GPT-4.1, temperature 0, first
        turn of a 14-file git task): with parallel tool calls on, 6 of 64 responses sent two
        scripts and then ran away to max_output_tokens with nothing more in the output; with them
        off, 0 of 28 did, and each sent a single script."""
        return {"parallel_tool_calls": False}

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
    def run(self, source: str, _cancel: dict | None = None) -> str:
        """Check and run a script. Returns text for the model: printed output
        on success, or `error: ...` lines it can repair from."""
        if not isinstance(source, str):
            return 'error: source must be a string'
        if not source.endswith("\n"):
            source += "\n"
        problems = self.engine.check(source)
        if problems:
            return "error: the script did not pass the static check\n" + "\n".join(problems)
        source = echo_last_expression(source)
        try:
            if _cancel is not None and hasattr(self.engine, "session"):
                session = self.engine.session()
                handle = session.cancel_handle()
                with _cancel["lock"]:
                    _cancel["handle"] = handle
                    if _cancel["cancelled"]:
                        handle.cancel()
                session.push(source)
                out = session.finish().stdout
            else:
                out = self.engine.run(source).stdout
        except Exception as exc:  # noqa: BLE001 - every Weave failure goes back to the model
            return f"error: {exc}"
        if len(out) > self.max_output_chars:
            out = out[: self.max_output_chars] + f"\n[truncated to {self.max_output_chars} characters]"
        return out if out.strip() else "(the script printed nothing; print the values you need)"

    def run_many(self, sources: list) -> list:
        """Run the scripts of one model turn (parallel tool calls) and return
        their outputs in order. Consecutive scripts that only read run
        concurrently; a script that may write waits for the scripts before
        it and blocks the ones after it, like native parallel tool calls."""
        from concurrent.futures import ThreadPoolExecutor

        def effect(source):
            try:
                return self.engine.script_effect(source if isinstance(source, str) else "")
            except Exception:  # noqa: BLE001 - a script that does not parse just reports its error
                return "pure"

        outputs: list = [None] * len(sources)
        with ThreadPoolExecutor(max_workers=max(1, min(8, len(sources)))) as pool:
            group: list = []
            for i, source in enumerate(sources):
                if effect(source) in ("pure", "read"):
                    group.append((i, pool.submit(self.run, source)))
                    continue
                for j, future in group:
                    outputs[j] = future.result()
                group = []
                outputs[i] = self.run(source)
            for j, future in group:
                outputs[j] = future.result()
        return outputs

    def handle_many(self, calls: list) -> list:
        """`handle` for every tool call of one model turn, using `run_many`."""
        get = lambda obj, key: obj.get(key) if isinstance(obj, dict) else getattr(obj, key, None)  # noqa: E731
        sources, shapes = [], []
        for call in calls:
            function = get(call, "function")
            chat = function is not None
            args = (get(function, "arguments") if chat else get(call, "arguments")) or "{}"
            try:
                source = json.loads(args)["source"]
            except (json.JSONDecodeError, KeyError, TypeError):
                source = None
            sources.append(source)
            shapes.append((chat, get(call, "id") if chat else get(call, "call_id")))
        valid = [i for i, src in enumerate(sources) if isinstance(src, str)]
        ran = dict(zip(valid, self.run_many([sources[i] for i in valid])))
        out = []
        for i, (chat, call_id) in enumerate(shapes):
            text = ran.get(i, 'error: call run_weave with JSON arguments {"source": "<script>"}')
            out.append({"role": "tool", "tool_call_id": call_id, "content": text} if chat
                       else {"type": "function_call_output", "call_id": call_id, "output": text})
        return out

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

    def _run_arguments(self, arguments: str, _cancel: dict | None = None) -> str:
        try:
            source = json.loads(arguments)["source"]
        except (json.JSONDecodeError, KeyError, TypeError):
            return 'error: call run_weave with JSON arguments {"source": "<script>"}'
        return self.run(source, _cancel)

    async def _cancellable(self, arguments: str) -> str:
        state = {"lock": threading.Lock(), "handle": None, "cancelled": False}
        try:
            return await asyncio.to_thread(self._run_arguments, arguments, state)
        except asyncio.CancelledError:
            # Cancel the session: isolated workers are killed, in-process tools
            # stop being waited on (they may still finish in their thread).
            with state["lock"]:
                state["cancelled"] = True
                if state["handle"] is not None:
                    state["handle"].cancel()
            raise

    async def run_async(self, source: str) -> str:
        """`run` for asyncio hosts. Cancelling the awaiting task cancels the
        script session, which kills isolated worker tools (`worker_pool=N`)."""
        return await self._cancellable(json.dumps({"source": source}))

    def agents_tool(self):
        """A FunctionTool for the OpenAI Agents SDK (`pip install openai-agents`)."""
        from agents import FunctionTool  # imported lazily; the SDK is optional

        async def invoke(_ctx, arguments: str) -> str:
            # Do not block the SDK event loop. Cancelling this coroutine cancels
            # the script session: isolated workers are killed, in-process host
            # work is soft-cancelled and may finish in its thread.
            return await self._cancellable(arguments)

        return FunctionTool(
            name=self.name,
            description=self.description,
            params_json_schema=self.parameters,
            on_invoke_tool=invoke,
            strict_json_schema=True,
        )
