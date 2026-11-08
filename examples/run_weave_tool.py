"""Drop Weave into a plain OpenAI function-calling loop as one `run_weave` tool.

With OPENAI_API_KEY set, this asks gpt-5.4-mini (or OPENAI_MODEL) a question
and lets it answer with one Weave script. Without a key it plays a canned tool
call through the same handler, so CI can run it offline.
"""

from __future__ import annotations

import json
import os
import time
import urllib.request

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


def chat(messages, tools):
    request = urllib.request.Request(
        "https://api.openai.com/v1/chat/completions",
        data=json.dumps({"model": os.environ.get("OPENAI_MODEL", "gpt-5.4-mini"), "messages": messages, "tools": tools}).encode(),
        headers={"Authorization": f"Bearer {os.environ['OPENAI_API_KEY']}", "Content-Type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=60) as response:
        return json.loads(response.read())["choices"][0]["message"]


def main() -> None:
    tool = weave.WeaveTool([list_orders, order_total])
    question = "What is the sum of the order totals for customer c9? Answer with just the number."
    if not os.environ.get("OPENAI_API_KEY"):
        source = 'total = 0\nfor o in list_orders("c9"):\n    total += order_total(o)\nprint(total)\n'
        call = {"id": "offline", "type": "function", "function": {"name": "run_weave", "arguments": json.dumps({"source": source})}}
        result = tool.handle(call)
        assert result["content"].strip() == "49", result
        print("run_weave_tool_ok offline answer=49")
        return
    messages = [{"role": "user", "content": question}]
    for _ in range(4):
        message = chat(messages, [tool.chat_tool()])
        messages.append(message)
        if not message.get("tool_calls"):
            print(f"answer={message['content'].strip()} model_turns={len(messages) // 2}")
            return
        for call in message["tool_calls"]:
            messages.append(tool.handle(call))
    raise SystemExit("no answer")


if __name__ == "__main__":
    main()
