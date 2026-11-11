"""A Weave arm for tau-bench, next to tau-bench's own ToolCallingAgent (the native arm, used unchanged).

The environment, tasks, user simulator, tools and grader are tau-bench's. The only difference is how the agent
reaches the tools: instead of one JSON tool call per model step, the model gets one `run_weave` tool whose
script calls the same tau-bench tools; every call still goes through `env.step(Action(...))`, so the
environment records it exactly as it would a native call, and tau-bench's `calculate_reward` grades the result.
Replying to the user is a plain assistant message in both arms (tau-bench's `respond` action).
"""

from __future__ import annotations

import inspect
import json
import threading
from typing import Any, Dict, List, Optional

from litellm import completion

from tau_bench.agents.base import Agent
from tau_bench.envs.base import Env
from tau_bench.types import RESPOND_ACTION_NAME, Action, SolveResult

# Host declarations (Weave's effect system). Everything not listed is treated as irreversible.
READ = {"find_user_id_by_email", "find_user_id_by_name_zip", "get_user_details", "get_order_details",
        "get_product_details", "list_all_product_types", "get_reservation_details", "search_direct_flight",
        "search_onestop_flight", "list_all_airports"}
PURE = {"calculate", "think"}


def _param_type(schema: Dict[str, Any]) -> str:
    from weave.agent_tool import schema_type
    return schema_type(schema)


def make_functions(env: Env, state: Dict[str, Any], shapes: Optional[Dict[str, str]] = None):
    """One Python function per tau-bench tool; each call is an `env.step(Action(name, kwargs))`."""
    import weave

    shapes = shapes or {}
    lock = threading.Lock()
    functions = []
    for info in env.tools_info:
        f = info["function"]
        name, props = f["name"], f["parameters"].get("properties", {})
        order = list(props)
        required = set(f["parameters"].get("required", order))

        def make(name=name, order=tuple(order)):
            def fn(*args, **kwargs):
                if len(args) > len(order):
                    raise weave.ToolError(f"{name} takes {len(order)} arguments")
                kwargs.update(zip(order, args))
                with lock:  # tau-bench's env is not written for concurrent steps
                    res = env.step(Action(name=name, kwargs=kwargs))
                state["tool_calls"] += 1
                if res.done:
                    state["done"], state["reward"], state["info"] = True, res.reward, res.info.model_dump()
                obs = res.observation
                if isinstance(obs, str) and obs.startswith("Error"):
                    raise weave.ToolError(obs)
                try:
                    return json.loads(obs)
                except (TypeError, ValueError):
                    return obs
            return fn

        fn = make()
        fn.__name__ = name
        args_doc = "; ".join(f"{p} ({_param_type(s)}{'' if p in required else ', optional'}): {s.get('description', '')}".strip()
                             for p, s in props.items())
        fn.__doc__ = " ".join((f.get("description", "") + (f" Args: {args_doc}" if args_doc else "")).split())
        fn.__signature__ = inspect.Signature([
            inspect.Parameter(p, inspect.Parameter.POSITIONAL_OR_KEYWORD, annotation=_param_type(s),
                              **({} if p in required else {"default": None})) for p, s in props.items()])
        fn.__weave_returns__ = shapes.get(name, "dict | list | str")
        effect = "pure" if name in PURE else "read" if name in READ else "irreversible"
        functions.append(weave.declare(effect=effect)(fn))
    return functions


class WeaveAgent(Agent):
    def __init__(self, tools_info: List[Dict[str, Any]], wiki: str, model: str, provider: str,
                 temperature: float = 0.0):
        self.tools_info, self.wiki, self.model, self.provider, self.temperature = tools_info, wiki, model, provider, temperature

    def solve(self, env: Env, task_index: Optional[int] = None, max_num_steps: int = 30) -> SolveResult:
        import weave

        reset = env.reset(task_index=task_index)
        info = reset.info.model_dump()
        state = {"done": False, "reward": 0.0, "info": None, "tool_calls": 0, "scripts": 0, "script_errors": 0}
        tool = weave.WeaveTool(make_functions(env, state, sample_return_shapes(env)), max_wall_time_ms=60_000,
                               conversational=True, persist_variables=True)
        tools = [tool.chat_tool()]
        messages: List[Dict[str, Any]] = [{"role": "system", "content": self.wiki},
                                          {"role": "user", "content": reset.observation}]
        reward = 0.0
        for _ in range(max_num_steps):
            res = completion(messages=messages, model=self.model, custom_llm_provider=self.provider, tools=tools,
                             temperature=self.temperature, **tool.request_options)
            msg = res.choices[0].message.model_dump()
            calls = msg.get("tool_calls") or []
            if calls:
                messages.append(msg)
                for call in calls:
                    fn = call.get("function") or {}
                    if fn.get("name") != tool.name:
                        out = f"error: the only tool is {tool.name}"
                    else:
                        try:
                            source = json.loads(fn.get("arguments") or "{}").get("source", "")
                        except ValueError:
                            source = None
                        out = tool.run(source) if isinstance(source, str) else "error: arguments must be JSON with a 'source' string"
                        state["scripts"] += 1
                    state["script_errors"] += out.startswith("error:")
                    messages.append({"role": "tool", "tool_call_id": call["id"], "name": fn.get("name"), "content": out})
                if state["done"]:  # a terminating tool (transfer_to_human_agents) ran inside the script
                    reward, info = state["reward"], {**info, **state["info"]}
                    break
                continue
            r = env.step(Action(name=RESPOND_ACTION_NAME, kwargs={"content": msg.get("content") or ""}))
            reward, info = r.reward, {**info, **r.info.model_dump()}
            messages.extend([msg, {"role": "user", "content": r.observation}])
            if r.done:
                break
        info["weave"] = {k: state[k] for k in ("tool_calls", "scripts", "script_errors")}
        return SolveResult(reward=reward, info=info, messages=messages, total_cost=0.0)


def sample_return_shapes(env: Env, samples: int = 12) -> Dict[str, str]:
    """`name -> shape` for each read tool, from calls on a copy of the environment's database.

    tau-bench tools have no output schemas; a native agent sees each JSON result, a script must know the shape
    before it runs. The sample calls go to `Tool.invoke` on a deep copy, never through `env.step`, so they are
    not actions and cannot change the graded state. Arguments come from the first `samples` records of each
    table, and the shapes of all results are merged, so per-record keys (product options) stay generic."""
    import copy

    from weave.agent_tool import shape_of_samples

    data = copy.deepcopy(env.data)
    firsts = {table: list(rows)[:samples] for table, rows in data.items() if isinstance(rows, dict) and rows}
    shapes = {}
    for info in env.tools_info:
        f = info["function"]
        name = f["name"]
        if name not in READ:
            continue
        params = list(f["parameters"].get("properties", {}))
        results = []
        for i in range(samples):
            pool = {}
            if "users" in firsts and i < len(firsts["users"]):
                uid = firsts["users"][i]
                user = data["users"][uid]
                pool.update(user_id=uid, email=user.get("email"), first_name=(user.get("name") or {}).get("first_name"),
                            last_name=(user.get("name") or {}).get("last_name"), zip=(user.get("address") or {}).get("zip"))
            for table, key in (("orders", "order_id"), ("products", "product_id"), ("reservations", "reservation_id")):
                if table in firsts and i < len(firsts[table]):
                    pool[key] = firsts[table][i]
            if any(pool.get(p) is None for p in params):
                continue
            try:
                out = env.tools_map[name].invoke(data=copy.deepcopy(data), **{p: pool[p] for p in params})
            except Exception:  # noqa: BLE001 - no sample, no shape
                continue
            if not isinstance(out, str) or out.startswith("Error"):
                continue
            try:
                results.append(json.loads(out))
            except ValueError:
                results.append(out)
            if not params:
                break
        if results:
            shapes[name] = shape_of_samples(results)
    return shapes
