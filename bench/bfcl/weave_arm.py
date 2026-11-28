"""A Weave arm for BFCL multi-turn, next to BFCL's own OpenAI function-calling handler (the native arm, unchanged).

The tasks, function docs, backend classes (file system, Twitter, trading, travel, ...) and the grader are BFCL's. The
only difference is how the model reaches the functions. The native arm is `OpenAICompletionsHandler`'s own
`inference_multi_turn_FC`: one JSON tool per function, all calls of a step executed. The Weave arm gives the model one
`run_weave` tool whose script calls the same functions, each executed through BFCL's `execute_multi_turn_func_call` on
the same kind of backend instances. Every call a script makes is recorded in the exact form the native handler
records a tool call (`{name: json_args}`), so BFCL's `_evaluate_single_multi_turn_entry` grades both arms the same way:
it re-executes the recorded calls on fresh instances and checks state and responses against the ground truth.
"""

from __future__ import annotations

import ast
import inspect
import json
import os
import time
from typing import Any, Dict, List

from bfcl_eval.constants.default_prompts import MAXIMUM_STEP_LIMIT
from bfcl_eval.eval_checker.multi_turn_eval.multi_turn_utils import execute_multi_turn_func_call

GORILLA_TYPES = {"string": "str", "integer": "int", "float": "float", "number": "float", "boolean": "bool",
                 "array": "list", "dict": "dict", "object": "dict", "tuple": "list", "any": "any"}


def _type(schema: Dict[str, Any]) -> str:
    t = schema.get("type", "any")
    if t == "array" and isinstance(schema.get("items"), dict):
        return f"list[{_type(schema['items'])}]"
    if t == "dict" and schema.get("properties"):
        fields = ", ".join(f'"{k}": {_type(v)}' for k, v in schema["properties"].items())
        return "{" + fields + "}"
    return GORILLA_TYPES.get(t, "any")


_PY = {"str": str, "int": int, "float": (int, float), "bool": bool, "list": list, "dict": dict}


def type_problem(name: str, schema: dict, value) -> str | None:
    """A mismatch between an argument and its declared simple type, e.g. `order_id` (int) given the str
    'order_type'. Only top-level str/int/float/bool/list/dict are checked; anything else passes."""
    want = _type(schema).split("[", 1)[0]
    kind = _PY.get(want)
    if kind is None or value is None:
        return None
    if isinstance(value, bool) and want in ("int", "float"):
        return f"{name} expects {want}, got bool {value!r}"
    if not isinstance(value, kind):
        return f"{name} expects {want}, got {type(value).__name__} {value!r:.60}"
    return None


def parse_result(out: str):
    """A BFCL backend result as a value. `execute_multi_turn_func_call` returns `repr`-style text for dicts and lists
    (single quotes), which `json.loads` rejects; round 8 handed those to scripts as strings (9 + 4 script errors of
    the form "cannot look up 'airports' in a str"). Try JSON, then a Python literal, else keep the text."""
    try:
        return json.loads(out)
    except (ValueError, TypeError):
        pass
    try:
        return ast.literal_eval(out)
    except (ValueError, SyntaxError, TypeError, MemoryError, RecursionError):
        return out


_SHAPES = None


def learned_shapes() -> dict:
    """Return shapes learned from train-split ground-truth executions (`learn_shapes.py`)."""
    global _SHAPES
    if _SHAPES is None:
        path = os.path.join(os.path.dirname(__file__), "shapes.json")
        _SHAPES = json.load(open(path))["shapes"] if os.path.exists(path) else {}
    return _SHAPES


_READS = None


def _long_context(entry: dict) -> bool:
    """BFCL's flag for the long-context and composite categories (bigger backend states), as its loop sets it."""
    return "long_context" in entry["id"] or "composite" in entry["id"]


def learned_reads() -> list:
    """Functions that never changed backend state on train ground-truth executions (`learn_effects.py`)."""
    global _READS
    if _READS is None:
        path = os.path.join(os.path.dirname(__file__), "effects.json")
        _READS = json.load(open(path))["reads"] if os.path.exists(path) else []
    return _READS


def make_functions(entry: dict, instance_name: str, step_calls: List[dict], stats: Dict[str, int],
                   shapes: str = "learned", typed_args_doc: bool = True, trace: list | None = None,
                   check_types: bool = False, error_key: str | None = None):
    """One Python function per BFCL function doc. Each call runs `execute_multi_turn_func_call` on this arm's
    instances and is appended to `step_calls` as `{name: json_args}`, exactly what the native handler records."""
    import weave

    functions = []
    refused: set = set()
    for doc in entry["function"]:
        name = doc["name"]
        props = doc.get("parameters", {}).get("properties", {}) or {}
        order = list(props)
        required = set(doc.get("parameters", {}).get("required", []) or [])

        def make(name=name, order=tuple(order), props=props):
            def fn(*args, **kwargs):
                if len(args) > len(order):
                    raise weave.ToolError(f"{name} takes at most {len(order)} arguments")
                kwargs = {**dict(zip(order, args)), **kwargs}
                if check_types:
                    # Checked execution: a script chains results without the model looking at them, so a value of
                    # the wrong kind (an id field that is really a label) would reach the backend unnoticed.
                    # Refuse it before the call; the model sees the error and repairs the script.
                    # Refused once per (function, argument, value): if the model sends it again after seeing the
                    # error, it means it (BFCL's own ground truth passes 'ticket_001' as an int id once in 1,323
                    # train arguments).
                    for k, v in kwargs.items():
                        problem = type_problem(k, props.get(k, {}), v)
                        key = (name, k, repr(v))
                        if problem and key not in refused:
                            refused.add(key)
                            raise weave.ToolError(f"{name}: {problem} (send the same call again if that is intended)")
                step_calls.append({name: json.dumps(kwargs)})
                stats["tool_calls"] += 1
                call = f"{name}({','.join(f'{k}={repr(v)}' for k, v in kwargs.items())})"
                [out], _ = execute_multi_turn_func_call([call], entry["initial_config"], entry["involved_classes"],
                                                        instance_name, entry["id"], long_context=_long_context(entry),
                                                        is_evaL_run=False)
                if trace is not None:
                    trace.append((call, out))
                if out.startswith("Error during execution:"):
                    raise weave.ToolError(out)
                value = parse_result(out)
                if isinstance(value, dict) and "error" in value:
                    stats["_soft_error"] = 1  # the backend reported a failure as a value, e.g. {"error": "..."}
                return value
            return fn

        fn = make()
        fn.__name__ = name
        if typed_args_doc:
            args_doc = "; ".join(f"{p} ({_type(s)}{'' if p in required else ', optional'}): {s.get('description', '')}"
                                 for p, s in props.items())
        else:  # types and defaults are already in the signature
            args_doc = "; ".join(f"{p}: {s.get('description', '')}" for p, s in props.items() if s.get("description"))
        fn.__doc__ = " ".join((doc.get("description", "") + (f" Args: {args_doc}" if args_doc else "")).split())
        fn.__signature__ = inspect.Signature([
            inspect.Parameter(p, inspect.Parameter.POSITIONAL_OR_KEYWORD, annotation=_type(s),
                              **({} if p in required else {"default": s.get("default")})) for p, s in props.items()])
        learned = learned_shapes().get(name) if shapes == "learned" else None
        if learned:
            fn.__weave_returns__ = learned
        elif shapes in ("learned", "doc") and doc.get("response"):
            fn.__weave_returns__ = _type(doc["response"])
        # BFCL's backends are stateful in-memory objects (a file system with a current directory, an account, ...):
        # every call may read or change that state, so every call is declared a (reversible) write and Weave keeps
        # them in program order.
        # `errkey` (round 15): the backends report failures as {"error": str} (train executions), so declare that
        # convention and a failed call raises at the call, as a raising tool does.
        functions.append(weave.declare(effect="reversible", error_key=error_key)(fn))
    return functions


RECOVER = ("If a call fails, fix the cause (look up what is missing or meet the precondition) and run the script again "
           "before you answer. Pass only values the user gave or a call returned. Never make up credentials or "
           "placeholders.")

DEFAULT_CONFIG = "doc"  # round 8: full rules, doc response schemas, no reply()


def inference_multi_turn_weave(handler, entry: dict, instance_name: str, create, config: str = DEFAULT_CONFIG
                               ) -> tuple[list, dict]:
    """BFCL's `inference_multi_turn_FC` loop (same messages, same step limit, same force quit), with one `run_weave`
    tool. Returns (per-turn list of per-step model responses in the native handler's format, metadata)."""
    import weave

    opts = set(filter(None, config.split(",")))
    shapes = "learned" if "learned" in opts else ("none" if "noshapes" in opts else "doc")
    stats = {"tool_calls": 0, "scripts": 0, "script_errors": 0, "agent_calls": 0, "replies": 0}
    step_calls: List[dict] = []
    # No persisted variables: in dev task 135 a persisted `order` let the model answer the last turn without the
    # lookup BFCL's grader expects in every turn ("empty turn"); each script starts fresh, as each native step does.
    trace: list = []
    def build_tool(shown=None):
        t = weave.WeaveTool(make_functions(entry, instance_name, step_calls, stats, shapes, "compact" not in opts,
                                           trace if "trace" in opts else None, "typecheck" in opts,
                                           "error" if "errkey" in opts else None),
                            max_wall_time_ms=60_000,
                            compact="compact" in opts or "hybrid" in opts, reply="reply" in opts,
                            listing="hybrid" not in opts,
                            write_gate=next((m for m in ("flow", "choice", "errors") if "gate" + ("" if m == "flow" else m) in opts),
                                            None), gate_read_names=learned_reads())
        if shown and t.gate is not None:
            t.gate._shown = shown
        return t

    def chat_tools(t):
        spec = t.chat_tool()
        if "recover" in opts:  # round 15, pre-registered wording
            spec["function"]["description"] = spec["function"]["description"].rstrip() + " " + RECOVER
        return [spec]

    tool = build_tool()
    tools = chat_tools(tool)
    if "hybrid" in opts:
        # Adaptive routing: BFCL's own native schemas (as the native arm sends them) plus run_weave for chains.
        from bfcl_eval.constants.enums import ModelStyle
        from bfcl_eval.constants.type_mappings import GORILLA_TO_OPENAPI
        from bfcl_eval.model_handler.utils import convert_to_tool
        tools = convert_to_tool(entry["function"], GORILLA_TO_OPENAPI, ModelStyle.OPENAI_COMPLETIONS) + tools
        direct = {f.__name__: f for f in tool.functions}
    # Instantiate the backends now, as the native loop does before its first query.
    execute_multi_turn_func_call([], entry["initial_config"], entry["involved_classes"], instance_name, entry["id"],
                                 long_context=_long_context(entry), is_evaL_run=False)
    messages: List[Any] = []
    all_turns: list = []
    force_quit = False
    # miss_func: BFCL withholds some functions until a later turn (`missed_function`), as the native loop does.
    holdout = entry.get("missed_function", {}) or {}
    for turn_idx, turn in enumerate(entry["question"]):
        if str(turn_idx) in holdout:
            entry["function"].extend(holdout[str(turn_idx)])
            tool = build_tool(tool.gate._shown if tool.gate is not None else None)
            tools = chat_tools(tool)
        messages.extend(turn)
        for m in turn:  # the write gate counts what the user said as seen
            tool.note_shown(m.get("content", "") if isinstance(m, dict) else "")
        turn_responses: list = []
        count = 0
        while True:
            res = create(messages=messages, model=handler.model_name, temperature=handler.temperature, store=False,
                         tools=tools)
            stats["agent_calls"] += 1
            msg = res.choices[0].message
            calls = msg.tool_calls or []
            if not calls:
                turn_responses.append(msg.content)  # what the native handler records for a text reply
                break
            messages.append(msg)
            step_calls.clear()
            trace.clear()
            stats["_soft_error"] = 0
            replied = None
            for call in calls:
                if "hybrid" in opts and call.function.name in direct:
                    try:
                        value = direct[call.function.name](**json.loads(call.function.arguments or "{}"))
                        out = value if isinstance(value, str) else json.dumps(value)
                    except Exception as exc:  # noqa: BLE001 - shown to the model like the native loop shows it
                        out = str(exc)
                    stats["direct_calls"] = stats.get("direct_calls", 0) + 1
                    trace.clear()
                elif call.function.name != tool.name:
                    out = f"error: the only tool is {tool.name}"
                else:
                    try:
                        source = json.loads(call.function.arguments or "{}").get("source", "")
                    except ValueError:
                        source = None
                    out = tool.run(source) if isinstance(source, str) else "error: arguments must be JSON with a 'source' string"
                    stats["scripts"] += 1
                    text = tool.take_reply()
                    paused = out.startswith("[paused")
                    if paused:
                        stats["gate_pauses"] = stats.get("gate_pauses", 0) + 1
                    if "trace" in opts and trace and not paused:
                        # Show every call and its result, as the native loop shows each tool result: the model keeps
                        # the same observations while the calls stay batched in one script.
                        lines = [f"{c} -> {o if len(o) <= 300 else o[:300] + '...'}" for c, o in trace[:30]]
                        out = out.rstrip("\n") + "\n[calls]\n" + "\n".join(lines)
                        trace.clear()
                        tool.note_shown(out)
                    if text is not None and "guard" in opts and stats["_soft_error"]:
                        out += "\n[reply withheld: a call returned an error; handle it, then answer]"
                        stats["replies_withheld"] = stats.get("replies_withheld", 0) + 1
                        text = None
                    if text is not None and len(calls) == 1:
                        replied = text
                stats["script_errors"] += out.startswith("error:")
                messages.append({"role": "tool", "content": out, "tool_call_id": call.id})
            turn_responses.append(list(step_calls))
            if replied is not None:
                # reply(): the script succeeded and its text ends the turn, recorded as the native handler records a
                # final text reply; the history gets it as the assistant's message.
                stats["replies"] += 1
                messages.append({"role": "assistant", "content": replied})
                turn_responses.append(replied)
                break
            count += 1
            if count > MAXIMUM_STEP_LIMIT:
                force_quit = True
                break
        all_turns.append(turn_responses)
        if force_quit:
            break
    stats.pop("_soft_error", None)
    return all_turns, {"weave": stats, "force_quit": force_quit, "messages": messages}
