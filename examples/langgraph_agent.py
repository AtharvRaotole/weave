"""Weave inside a LangGraph agent: the model gets ONE tool, `run_weave`, and answers with a script.

    python examples/langgraph_agent.py          # offline: a scripted chat model, no key, no network
    python examples/langgraph_agent.py --live   # real model (pip install langchain-openai; OPENAI_API_KEY)

Needs `pip install langchain-core langgraph`. The graph is the standard model -> ToolNode loop; only the tool differs.
"""

from __future__ import annotations

import sys
import time

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


QUESTION = "What is the total of all orders of customer c9?"
SCRIPT = "print(sum(order_total(o) for o in list_orders('c9')))\n"


def scripted_model():
    """Stands in for a chat model offline: first a `run_weave` call, then an answer from its output."""
    from langchain_core.messages import AIMessage, ToolMessage

    def model(state):
        last = state["messages"][-1]
        if isinstance(last, ToolMessage):
            return {"messages": [AIMessage(content=f"The total is {last.content.strip()}.")]}
        return {"messages": [AIMessage(content="", tool_calls=[
            {"name": "run_weave", "args": {"source": SCRIPT}, "id": "call_1", "type": "tool_call"}])]}

    return model


def live_model(tools):
    from langchain_openai import ChatOpenAI

    import os
    llm = ChatOpenAI(model=os.environ.get("OPENAI_MODEL", "gpt-5.4-mini")).bind_tools(tools)
    return lambda state: {"messages": [llm.invoke(state["messages"])]}


def build(model_fn, tools):
    from langgraph.graph import END, START, MessagesState, StateGraph
    from langgraph.prebuilt import ToolNode

    g = StateGraph(MessagesState)
    g.add_node("model", model_fn)
    g.add_node("tools", ToolNode(tools))
    g.add_edge(START, "model")
    g.add_conditional_edges("model", lambda s: "tools" if getattr(s["messages"][-1], "tool_calls", None) else END)
    g.add_edge("tools", "model")
    return g.compile()


def main():
    from langchain_core.messages import HumanMessage

    tool = weave.WeaveTool([list_orders, order_total]).langchain_tool()
    live = "--live" in sys.argv
    graph = build(live_model([tool]) if live else scripted_model(), [tool])
    out = graph.invoke({"messages": [HumanMessage(content=QUESTION)]})
    answer = out["messages"][-1].content
    print(answer)
    if not live:
        assert answer == "The total is 49.", answer
        print("langgraph agent ok")


if __name__ == "__main__":
    main()
