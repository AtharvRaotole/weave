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

# `compact=True`: the same rules in fewer tokens. The description is sent with every request, so on short tasks it is
# most of what Weave adds to the bill (round 8, BFCL: about 850 more billed input tokens per request than the native
# tool schemas).
COMPACT_RULES = (
    "Run a Weave script (Python subset) that calls the functions below by bare name. Put a turn's dependent calls in "
    "ONE script; independent calls and loop iterations run in parallel. No imports, classes, while or with; use "
    "`for _ in range(N):` + break. Allowed: if/for/try/except, def, comprehensions, lambda, f-strings, str/list/dict "
    "methods, common builtins, parse_json. A failing call raises ToolError. print() only what you need; you get the "
    "printed text or an error with a hint."
)

REPLY_RULE = (
    " If the printed results fully answer the user, end the script with reply(text), e.g. "
    "reply(f\"Moved {n} files to {dest}.\"): the turn then ends with that text and you will not see the output. "
    "Call reply() only when this script has already done everything the user asked in this turn (every requested "
    "change made, every requested value fetched), never to report partial progress; do not call it when you need to "
    "look at results first."
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
    Pass several samples with `shape_of_samples` so that fields whose keys vary between records (free-form
    options) are not mistaken for fixed fields."""
    return shape_of_samples([value], depth)


def _id_like(key: Any) -> bool:
    return isinstance(key, str) and any(c.isdigit() for c in key)


def shape_of_samples(values: list, depth: int = 0) -> str:
    """One shape covering every sample. Dicts keyed by ids (`{"item_123": {...}}`) become `dict[str, V]`;
    str-valued dicts whose keys differ between samples become `dict[str, str]`; a record field missing from
    some samples is marked `?`."""
    if depth > 5 or not values:
        return "any"
    if all(isinstance(v, dict) for v in values):
        nonempty = [v for v in values if v]
        if not nonempty:
            return "dict"
        if all(_id_like(k) for v in nonempty for k in v) and all(isinstance(x, dict) for v in nonempty for x in v.values()):
            return f"dict[str, {shape_of_samples([x for v in nonempty for x in v.values()][:200], depth + 1)}]"
        keysets = {tuple(v) for v in nonempty}
        if len(keysets) > 1 and all(isinstance(x, str) for v in nonempty for x in v.values()):
            return "dict[str, str]"
        if len(nonempty) == 1 and len(nonempty[0]) > 6 and all(isinstance(x, str) for x in nonempty[0].values()):
            return "dict[str, str]"
        keys = list(dict.fromkeys(k for v in nonempty for k in v))
        fields = []
        for k in keys:
            vals = [v[k] for v in nonempty if k in v]
            fields.append(f"{json.dumps(str(k))}: {shape_of_samples(vals, depth + 1)}" + ("" if len(vals) == len(nonempty) else "?"))
        return "{" + ", ".join(fields) + "}"
    if all(isinstance(v, (list, tuple)) for v in values):
        items = [x for v in values for x in v][:200]
        return f"list[{shape_of_samples(items, depth + 1)}]" if items else "list"
    kinds: list = []
    for v in values:
        if v is None:
            k = "None"
        elif isinstance(v, bool):
            k = "bool"
        elif isinstance(v, int):
            k = "int"
        elif isinstance(v, float):
            k = "float"
        elif isinstance(v, str):
            k = "str"
        elif isinstance(v, (dict, list, tuple)):
            k = shape_of_samples([v], depth)
        else:
            k = type(v).__name__
        if k not in kinds:
            kinds.append(k)
    if kinds == ["int", "float"] or kinds == ["float", "int"]:
        return "float"
    return " | ".join(kinds[:3])


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
        isinstance(node, ast.Call) and isinstance(node.func, ast.Name) and node.func.id in ("print", "reply")
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


def _sentences(text: str) -> list:
    import re

    return [x.strip() for x in re.split(r"(?<=[.!?])\s+", text.strip()) if x.strip()]


def compact_listing(functions: list) -> str:
    """Function lines with shared text factored out, for `compact=True`.

    Tool docs generated from one API often repeat the same sentences in every entry ("This tool belongs to the
    vehicle control system, ...", "Note that the provided function is in Python 3 syntax."). Any sentence of 40+
    characters that appears in 3 or more docs is written once as a note `[A] ...` and each line cites its notes.
    The effect is printed once when every function has the same one."""
    metas = [(getattr(f, "__weave__", None) or {}).get("effect") or "irreversible" for f in functions]
    same_effect = len(set(metas)) == 1
    docs = [_sentences(" ".join((inspect.getdoc(f) or "").split())) for f in functions]
    counts: dict = {}
    for d in docs:
        for sent in dict.fromkeys(d):
            counts[sent] = counts.get(sent, 0) + 1
    shared = [x for x, c in counts.items() if c >= 3 and len(x) >= 40]
    labels = {x: chr(65 + i) if i < 26 else f"N{i}" for i, x in enumerate(shared)}
    lines = []
    if same_effect:
        lines.append(f"Every function has effect {metas[0]}.")
    lines += [f"[{labels[x]}] {x}" for x in shared]
    owns = [" ".join(x for x in d if x not in labels) for d in docs]
    # A label every remaining doc starts with ("Tool description: ...") says nothing; drop it.
    import os as _os

    prefix = _os.path.commonprefix(owns) if len(owns) >= 3 else ""
    prefix = prefix[: prefix.find(":") + 1] if ":" in prefix[:40] else ""
    for f, effect, d, own in zip(functions, metas, docs, owns):
        returns = _returns(f)
        head = f"{f.__name__}({_signature(f)})" + (f" -> {returns}" if returns else "")
        if not same_effect:
            head += f", effect {effect}"
        own = own[len(prefix):].strip() if prefix else own
        refs = "".join(f"[{labels[x]}]" for x in dict.fromkeys(d) if x in labels)
        lines.append(f"{head}. {own} {refs}".rstrip())
    return "\n".join(lines)


def _literal(value: Any, depth: int = 0) -> str | None:
    """Weave source for a plain value, or None when it has no literal form."""
    if depth > 40:
        return None
    if value is None or isinstance(value, bool):
        return repr(value)
    if isinstance(value, (int, str)):
        return repr(value)
    if isinstance(value, float):
        return repr(value) if value == value and value not in (float("inf"), float("-inf")) else None
    if isinstance(value, (list, tuple)):
        items = [_literal(v, depth + 1) for v in value]
        return None if None in items else "[" + ", ".join(items) + "]"
    if isinstance(value, dict):
        pairs = []
        for k, v in value.items():
            kt, vt = _literal(k, depth + 1), _literal(v, depth + 1)
            if kt is None or vt is None:
                return None
            pairs.append(f"{kt}: {vt}")
        return "{" + ", ".join(pairs) + "}"
    return None


def _shift_lines(text: str, prelude: str) -> str:
    """Report positions in the model's own script, not counting the one-line prelude."""
    import re

    if not prelude:
        return text
    return re.sub(r"\bL(\d+):", lambda m: f"L{max(1, int(m.group(1)) - 1)}:", text)



GATE_RULE = (
    " A write whose arguments were computed from results you have not seen (or that follows a call that failed) is "
    "paused before it runs: the tool returns the results so far, and you send the writes again in the next script."
)

_WRITE_EFFECTS = ("reversible", "irreversible")


def _leaves(value, out: list, depth: int = 0) -> list:
    """Scalar leaves of a value (strings, numbers), for value provenance."""
    if depth > 6 or len(out) > 2000:
        return out
    if isinstance(value, dict):
        for k, v in value.items():
            _leaves(k, out, depth + 1)
            _leaves(v, out, depth + 1)
    elif isinstance(value, (list, tuple, set)):
        for v in value:
            _leaves(v, out, depth + 1)
    elif isinstance(value, (str, int, float)) and not isinstance(value, bool):
        out.append(value)
    return out


def _choice_leaves(value, out: set, depth: int = 0, in_list: bool = False) -> set:
    """Scalar leaves that sit inside a list of two or more items: values a script can *pick* among."""
    if depth > 6 or len(out) > 2000:
        return out
    if isinstance(value, dict):
        for v in value.values():
            _choice_leaves(v, out, depth + 1, in_list)
    elif isinstance(value, (list, tuple, set)):
        many = in_list or len(value) >= 2
        for v in value:
            _choice_leaves(v, out, depth + 1, many)
    elif in_list and isinstance(value, (str, int, float)) and not isinstance(value, bool):
        out.add(repr(value))
    return out


def default_is_error(value) -> bool:
    """A result that reports a failure as a value, e.g. `{"error": "..."}`."""
    return isinstance(value, dict) and "error" in value


class WriteGate:
    """Pauses a script before a write the model has not effectively seen the inputs of.

    A script batches calls the model never looks at one by one. That saves round trips, but a write can then act on
    a value the model never saw: an id picked out of a list, a directory name from a listing, or a step that follows
    a call that failed. Native function calling never does this, because the model sees every result before its next
    call. The gate restores that guarantee only where it matters, at writes:

    * **Value provenance (`flow`).** Every argument of a `reversible` or `irreversible` call is split into scalar
      leaves. A leaf is *known* when it appears literally in text the model has seen: its own scripts, what the tool
      returned to it, and anything the host adds with `note_shown` (the user's messages). A write with an unknown
      leaf, made after this script received any result, was computed from data the model has not seen.
    * **Choices (`choice`, the default).** As `flow`, but only for a leaf that the script picked out of a list of
      two or more items it has not seen (which order, which file, which flight). A value read from a single field,
      such as a price or an id looked up by name, passes: the model asked for exactly that value.
    * **Failures (all modes; `errors` is this rule alone).** A write after a call of this script that raised a ToolError the script caught, or
      returned an error value (`is_error`), is control-dependent on an outcome the model has not seen.

    The paused write is refused before it runs (its function is never called), every later write of the script is
    refused too, and reads may continue. The tool then returns the printed output plus every call and result of the
    script, which become known, so the same writes go through in the next script. Nothing is undone and nothing runs
    twice: refused writes never started. The refusal is an ordinary tool error, so traces and replay record it like
    any other. Writes wait for all earlier observable calls (effect ordering), so by the time a write reaches the
    gate every result it could depend on has been seen by the gate.

    Matching is by value, not by tracking the interpreter: a computed value that happens to equal known text passes
    (a miss), and short numbers usually appear somewhere, so they pass. It never pauses a write whose arguments
    are all literals in the script.
    """

    def __init__(self, mode: str = "choice", is_error: Callable[[Any], bool] | None = None,
                 read_names: Iterable[str] = ()):
        if mode not in ("flow", "choice", "errors"):
            raise ValueError("write_gate must be 'choice', 'flow', 'errors' or None")
        self.mode = mode
        self.is_error = is_error or default_is_error
        self.read_names = set(read_names)
        self._shown: list[str] = []
        self._lock = threading.Lock()
        self.pauses = 0
        self.begin("")

    def note_shown(self, text: str) -> None:
        if text:
            with self._lock:
                self._shown.append(str(text))

    def begin(self, source: str) -> None:
        with self._lock:
            self._calls: list[tuple[str, str]] = []
            self._results = 0
            self._choices: set = set()
            self._failed: str | None = None
            self._held: str | None = None
            self._reason = ""
        self.note_shown(source)

    def _known(self, leaf) -> bool:
        text = leaf if isinstance(leaf, str) else repr(leaf)
        if isinstance(leaf, str) and len(leaf.strip()) < 2:
            return True
        return any(text in chunk for chunk in self._shown) or (
            isinstance(leaf, str) and any(json.dumps(leaf)[1:-1] in chunk for chunk in self._shown))

    def wrap(self, func: Callable) -> Callable:
        import functools

        from weave.errors import ToolError

        meta = getattr(func, "__weave__", None) or {}
        name = getattr(func, "__name__", "tool")
        is_write = (meta.get("effect") or "irreversible") in _WRITE_EFFECTS and name not in self.read_names
        gate = self

        @functools.wraps(func)
        def gated(*args, **kwargs):
            shown_call = f"{name}({', '.join([repr(a) for a in args] + [f'{k}={v!r}' for k, v in kwargs.items()])})"
            if is_write:
                with gate._lock:
                    if gate._held is None:
                        if gate._failed is not None:
                            gate._held, gate._reason = shown_call, f"it follows {gate._failed}, which failed"
                        elif gate._results and gate.mode != "errors":
                            unknown = [x for x in _leaves([list(args), kwargs], []) if not gate._known(x)]
                            if gate.mode == "choice":
                                unknown = [x for x in unknown if repr(x) in gate._choices]
                            if unknown:
                                gate._held = shown_call
                                gate._reason = (f"{unknown[0]!r} was picked from a list you have not seen"
                                                if gate.mode == "choice" else
                                                f"{unknown[0]!r} came from results you have not seen")
                    held = gate._held
                if held is not None:
                    raise ToolError(f"paused before {shown_call}: check the results first")
            try:
                value = func(*args, **kwargs)
            except ToolError as exc:
                with gate._lock:
                    gate._calls.append((shown_call, f"error: {exc}"))
                    gate._failed = gate._failed or shown_call
                raise
            with gate._lock:
                gate._results += 1
                try:
                    text = json.dumps(value, default=str)
                except (TypeError, ValueError):
                    text = repr(value)
                gate._calls.append((shown_call, text))
                if gate.mode == "choice":
                    _choice_leaves(value, gate._choices)
                if gate.is_error(value):
                    gate._failed = gate._failed or shown_call
            return value

        return gated

    def finish(self, out: str) -> str:
        """The text to return for this run: `out`, or the pause report when a write was held."""
        with self._lock:
            held, reason, calls = self._held, self._reason, list(self._calls)
        if held is not None:
            self.pauses += 1
            lines = [f"{c} -> {r if len(r) <= 400 else r[:400] + '...'}" for c, r in calls[:40]]
            printed = out if out and not out.startswith("error:") else ""
            out = (f"[paused before {held}: {reason}. It and any later writes did not run; the calls before it did.]\n"
                   + (printed.rstrip("\n") + "\n" if printed.strip() else "")
                   + "[results so far]\n" + "\n".join(lines) +
                   "\nThe request is not finished. Check these results, then call this tool again now with the "
                   "remaining steps (fixed if needed); do not repeat calls above that already ran.")
        self.note_shown(out)
        return out

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
        persist_variables: bool = False,
        compact: bool = False,
        reply: bool = False,
        listing: bool = True,
        write_gate: str | None = None,
        gate_is_error: Callable[[Any], bool] | None = None,
        gate_read_names: Iterable[str] = (),
    ):
        """`worker_pool=N` runs every function in killable worker processes
        (`Engine(worker_pool=N, isolate_all=True)`): a tool timeout or the run
        deadline kills the worker. Functions must then be module-level and
        pickleable; opt one out with `weave.declare(hard_deadline=False)`.
        `max_wall_time_ms` bounds each script run (default 30s).
        `conversational=True` adds `CONVERSATION_RULES` to the description, for hosts where a person
        replies between turns.
        `persist_variables=True` keeps the variables of earlier scripts, like a notebook: a later script
        can use `order` from the script before instead of fetching it again. Values are snapshots taken
        when the earlier script ended; they reach the engine as literal assignments, so effect analysis
        and scheduling see plain constants.
        `compact=True` sends shorter rules and a listing with repeated doc sentences factored out
        (`compact_listing`); the description is part of every request.
        `listing=False` (with `compact=True`) leaves the function list out of the description, for hosts that
        also send every function as its own native tool (a hybrid: one call directly, several in a script).
        `reply=True` adds a `reply(text)` function: a script that succeeds and calls it ends the turn with that
        text, saving the model call that would only restate the output. After `run`, `take_reply()` returns the
        text (or None); the host decides whether to end the turn with it.
        `write_gate="choice"` pauses a script before a write whose arguments were picked from a list the model has not
        seen, or that follows a failed call (`"flow"`: any argument computed from unseen results; `"errors"`: failures only), and returns those results instead (see `WriteGate`). `gate_is_error` decides
        which results count as failures (default: a dict with an "error" key); `gate_read_names` lists functions the
        gate treats as reads although they are declared writes (e.g. for scheduling). Call `note_shown(text)` with
        other text the model sees, such as the user's messages. Not available with `worker_pool` or an `engine`."""
        self.functions = list(functions)
        self.gate = WriteGate(write_gate, gate_is_error, gate_read_names) if write_gate else None
        if self.gate is not None and (engine is not None or worker_pool is not None):
            raise ValueError("write_gate wraps the functions in-process; it cannot be used with engine= or worker_pool=")
        self.compact = compact
        self.reply = reply
        self.listing = listing
        self._reply_text = None
        if reply:
            box = self

            def reply_fn(text):
                """End the turn with this text for the user."""
                box._pending_reply = str(text)
                return None

            reply_fn.__name__ = "reply"
            from weave import declare as _declare

            self._reply_fn = _declare(effect="pure", arity=1)(reply_fn)
            self._pending_reply = None
        self.conversational = conversational
        self.persist_variables = persist_variables
        self.variables: dict = {}
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
            installed = [self.gate.wrap(f) for f in self.functions] if self.gate is not None else self.functions
            engine.install(installed + ([self._reply_fn] if reply else []))
        elif reply:
            engine.install([self._reply_fn])
        self.engine = engine
        if max_wall_time_ms is not None:
            self.engine.set_limits(max_wall_time_ms=max_wall_time_ms)

    # ---- tool definitions -------------------------------------------------
    @property
    def description(self) -> str:
        if self.compact:
            rules = COMPACT_RULES + (REPLY_RULE if self.reply else "") + (GATE_RULE if self.gate else "")
            if self.persist_variables:
                rules += " Variables persist between scripts."
            if self.conversational:
                rules = f"{rules} {CONVERSATION_RULES}"
            if not self.listing:
                # Hybrid hosts also send each function as its own tool; the script calls the same functions by name.
                return (rules.replace("the functions below", "the other tools in this request") +
                        " Call a single tool directly; use this tool for two or more calls, loops or dependent calls.")
            return f"{rules}\nFunctions:\n{compact_listing(self.functions)}"
        lines = "\n".join(tool_line(f) for f in self.functions)
        rules = RULES
        if self.reply:
            rules += REPLY_RULE
        if self.gate is not None:
            rules += GATE_RULE
        if self.persist_variables:
            rules = rules.replace("(scripts do not share variables)", "(each script sees the variables earlier scripts set)")
            rules += (" Variables persist between scripts, like a notebook: reuse values you already fetched instead "
                      "of calling again; call again when you need fresh data after a change.")
        if self.conversational:
            rules = f"{rules} {CONVERSATION_RULES}"
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
    def note_shown(self, text: str) -> None:
        """Tell the write gate about text the model has seen outside this tool (e.g. the user's message)."""
        if self.gate is not None:
            self.gate.note_shown(text)

    def run(self, source: str, _cancel: dict | None = None) -> str:
        """Check and run a script. Returns text for the model: printed output
        on success, or `error: ...` lines it can repair from."""
        if self.gate is None:
            return self._run(source, _cancel)
        self.gate.begin(source if isinstance(source, str) else "")
        out = self.gate.finish(self._run(source, _cancel))
        if out.startswith("[paused"):
            self._reply_text = None
        return out

    def _run(self, source: str, _cancel: dict | None = None) -> str:
        self._reply_text = None
        if self.reply:
            self._pending_reply = None
        if not isinstance(source, str):
            return 'error: source must be a string'
        if not source.endswith("\n"):
            source += "\n"
        source = echo_last_expression(source)
        prelude = self._prelude(source) if self.persist_variables else ""
        problems = self.engine.check(prelude + source)
        if problems:
            return "error: the script did not pass the static check\n" + "\n".join(_shift_lines(p, prelude) for p in problems)
        source = prelude + source
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
                result = self.engine.run(source)
                out = result.stdout
                if self.persist_variables:
                    self._keep(result.bindings)
        except Exception as exc:  # noqa: BLE001 - every Weave failure goes back to the model
            return f"error: {_shift_lines(str(exc), prelude)}"
        if len(out) > self.max_output_chars:
            out = out[: self.max_output_chars] + f"\n[truncated to {self.max_output_chars} characters]"
        if self.reply and self._pending_reply is not None:
            self._reply_text = self._pending_reply
            return out if out.strip() else "(replied)"
        return out if out.strip() else "(the script printed nothing; print the values you need)"

    def take_reply(self) -> str | None:
        """The text the last successful script passed to `reply()`, once; None if it did not reply or failed."""
        text, self._reply_text = self._reply_text, None
        return text

    def _prelude(self, source: str) -> str:
        """Literal assignments for the earlier variables this script reads, on one line."""
        import ast

        if not self.variables:
            return ""
        try:
            tree = ast.parse(source)
        except SyntaxError:
            return ""
        used = {n.id for n in ast.walk(tree) if isinstance(n, ast.Name)}
        parts = [f"{k} = {v}" for k, v in self.variables.items() if k in used]
        return ("; ".join(parts) + "\n") if parts else ""

    def _keep(self, bindings: dict) -> None:
        for name, value in bindings.items():
            if name.startswith("_") or name in ("ToolError",):
                continue
            text = _literal(value)
            if text is not None and len(text) <= 200_000:
                self.variables[name] = text
            else:
                self.variables.pop(name, None)

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

    def langchain_tool(self):
        """A LangChain `StructuredTool` (`pip install langchain-core`). Works anywhere LangChain tools do:
        `model.bind_tools([...])`, LangGraph's `ToolNode` and `create_react_agent`. Async invocation
        (`ainvoke`) cancels the script session when the awaiting task is cancelled, like `agents_tool`."""
        from weave.integrations import langchain_tool  # imported lazily; LangChain is optional

        return langchain_tool(self)
