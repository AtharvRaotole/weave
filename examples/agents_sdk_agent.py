"""Weave inside the OpenAI Agents SDK: `WeaveTool(...).agents_tool()` is a regular `FunctionTool`.

    python examples/agents_sdk_agent.py          # offline: a scripted Model, no key, no network
    python examples/agents_sdk_agent.py --live   # real model (OPENAI_API_KEY; OPENAI_MODEL, default gpt-5.4-mini)

Needs `pip install openai-agents`.
"""

from __future__ import annotations

import asyncio
import json
import os
import sys
import time

from agents import Agent, Runner, set_tracing_disabled

import weave

ORDERS = {"c9": ["o1", "o2", "o3"]}
TOTALS = {"o1": 12, "o2": 30, "o3": 7}


@weave.declare(effect="read", arity=1)
def list_orders(customer_id):
    """List order ids for a customer."""
    time.sleep(0.05)
    return ORDERS.get(customer_id, [])


@weave.declare(effect="read", arity=1)
def order_total(order_id):
    """Integer total of one order."""
    time.sleep(0.05)
    return TOTALS[order_id]


SCRIPT = "print(sum(order_total(o) for o in list_orders('c9')))\n"


def scripted_model():
    """A `Model` that calls run_weave once, then answers from the tool output (offline stand-in)."""
    from agents.items import ModelResponse
    from agents.models.interface import Model
    from agents.usage import Usage
    from openai.types.responses import ResponseFunctionToolCall, ResponseOutputMessage, ResponseOutputText

    class Scripted(Model):
        async def get_response(self, system_instructions, input, model_settings, tools, output_schema, handoffs,
                               tracing, **kw):
            done = [i for i in input if isinstance(i, dict) and i.get("type") == "function_call_output"]
            if done:
                item = ResponseOutputMessage(id="m1", type="message", role="assistant", status="completed", content=[
                    ResponseOutputText(type="output_text", text=f"The total is {done[-1]['output'].strip()}.",
                                       annotations=[])])
            else:
                item = ResponseFunctionToolCall(id="f1", call_id="c1", type="function_call", name="run_weave",
                                                arguments=json.dumps({"source": SCRIPT}))
            return ModelResponse(output=[item], usage=Usage(), response_id=None)

        def stream_response(self, *a, **k):
            raise NotImplementedError

    return Scripted()


def main():
    live = "--live" in sys.argv
    if not live:
        set_tracing_disabled(True)  # offline: nothing to export traces to
    agent = Agent(
        name="orders",
        instructions="Answer with one run_weave script that does every lookup, then reply in one sentence.",
        tools=[weave.WeaveTool([list_orders, order_total]).agents_tool()],
        model=os.environ.get("OPENAI_MODEL", "gpt-5.4-mini") if live else scripted_model(),
    )
    result = asyncio.run(Runner.run(agent, "What is the total of all orders of customer c9?"))
    print(result.final_output)
    if not live:
        assert result.final_output == "The total is 49.", result.final_output
        print("agents sdk agent ok")


if __name__ == "__main__":
    main()
