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

import inspect
import json
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


def make_functions(entry: dict, instance_name: str, step_calls: List[dict], stats: Dict[str, int]):
    """One Python function per BFCL function doc. Each call runs `execute_multi_turn_func_call` on this arm's
    instances and is appended to `step_calls` as `{name: json_args}`, exactly what the native handler records."""
    import weave

    functions = []
    for doc in entry["function"]:
        name = doc["name"]
        props = doc.get("parameters", {}).get("properties", {}) or {}
        order = list(props)
        required = set(doc.get("parameters", {}).get("required", []) or [])

        def make(name=name, order=tuple(order)):
            def fn(*args, **kwargs):
                if len(args) > len(order):
                    raise weave.ToolError(f"{name} takes at most {len(order)} arguments")
                kwargs = {**dict(zip(order, args)), **kwargs}
                step_calls.append({name: json.dumps(kwargs)})
                stats["tool_calls"] += 1
                call = f"{name}({','.join(f'{k}={repr(v)}' for k, v in kwargs.items())})"
                [out], _ = execute_multi_turn_func_call([call], entry["initial_config"], entry["involved_classes"],
                                                        instance_name, entry["id"], long_context=False,
                                                        is_evaL_run=False)
                if out.startswith("Error during execution:"):
                    raise weave.ToolError(out)
                try:
                    return json.loads(out)
                except ValueError:
                    return out
            return fn

        fn = make()
        fn.__name__ = name
        args_doc = "; ".join(f"{p} ({_type(s)}{'' if p in required else ', optional'}): {s.get('description', '')}"
                             for p, s in props.items())
        fn.__doc__ = " ".join((doc.get("description", "") + (f" Args: {args_doc}" if args_doc else "")).split())
        fn.__signature__ = inspect.Signature([
            inspect.Parameter(p, inspect.Parameter.POSITIONAL_OR_KEYWORD, annotation=_type(s),
                              **({} if p in required else {"default": s.get("default")})) for p, s in props.items()])
        if doc.get("response"):
            fn.__weave_returns__ = _type(doc["response"])
        # BFCL's backends are stateful in-memory objects (a file system with a current directory, an account, ...):
        # every call may read or change that state, so every call is declared a (reversible) write and Weave keeps
        # them in program order.
        functions.append(weave.declare(effect="reversible")(fn))
    return functions


def inference_multi_turn_weave(handler, entry: dict, instance_name: str, create) -> tuple[list, dict]:
    """BFCL's `inference_multi_turn_FC` loop (same messages, same step limit, same force quit), with one `run_weave`
    tool. Returns (per-turn list of per-step model responses in the native handler's format, metadata)."""
    import weave

    stats = {"tool_calls": 0, "scripts": 0, "script_errors": 0, "agent_calls": 0}
    step_calls: List[dict] = []
    # No persisted variables: in dev task 135 a persisted `order` let the model answer the last turn without the
    # lookup BFCL's grader expects in every turn ("empty turn"); each script starts fresh, as each native step does.
    tool = weave.WeaveTool(make_functions(entry, instance_name, step_calls, stats), max_wall_time_ms=60_000)
    tools = [tool.chat_tool()]
    # Instantiate the backends now, as the native loop does before its first query.
    execute_multi_turn_func_call([], entry["initial_config"], entry["involved_classes"], instance_name, entry["id"],
                                 long_context=False, is_evaL_run=False)
    messages: List[Any] = []
    all_turns: list = []
    force_quit = False
    for turn in entry["question"]:
        messages.extend(turn)
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
            for call in calls:
                if call.function.name != tool.name:
                    out = f"error: the only tool is {tool.name}"
                else:
                    try:
                        source = json.loads(call.function.arguments or "{}").get("source", "")
                    except ValueError:
                        source = None
                    out = tool.run(source) if isinstance(source, str) else "error: arguments must be JSON with a 'source' string"
                    stats["scripts"] += 1
                stats["script_errors"] += out.startswith("error:")
                messages.append({"role": "tool", "content": out, "tool_call_id": call.id})
            turn_responses.append(list(step_calls))
            count += 1
            if count > MAXIMUM_STEP_LIMIT:
                force_quit = True
                break
        all_turns.append(turn_responses)
        if force_quit:
            break
    return all_turns, {"weave": stats, "force_quit": force_quit, "messages": messages}
