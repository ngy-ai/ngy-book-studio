"""第 7 章 langgraph 参考示范；阅读或运行示范不代表独立完成。"""

import hashlib
import json
from typing import TypedDict

from langgraph.graph import END, START, StateGraph
from langsmith import tracing_context

from ngy_lab.chapter_support import call_tool, finish, load_case


def proposal_digest(proposal):
    encoded = json.dumps(proposal, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def prepare(data):
    approval = data["approval"]
    if data["cancelled"]:
        return "cancelled"
    if approval["decision"] != "approve":
        return "rejected"
    if approval["expires_at"] <= data["now"]:
        return "expired"
    if approval["digest"] != proposal_digest(data["proposal"]):
        return "proposal_changed"
    return "completed"


def perform(data, plan, state, tools):
    receipt = None
    # Repeated delivery of this one logical proposal must not repeat its effect.
    if plan == "completed":
        response = call_tool(
            state,
            tools,
            "save_draft",
            {"proposal": data["proposal"], "digest": proposal_digest(data["proposal"])},
        )
        receipt = response["data"]["receipt"]
    return {"status": plan, "writes": int(receipt is not None), "receipt": receipt}


class GraphState(TypedDict):
    plan: object
    result: dict


def run(task, model, tools, limits, emit):
    state = load_case(task, model, tools)
    data = state["case"]["input"]

    def plan_node(graph_state):
        emit({"phase": "graph_node", "node": "prepare"})
        return {"plan": prepare(data)}

    def execute_node(graph_state):
        emit({"phase": "graph_node", "node": "execute"})
        return {"result": perform(data, graph_state["plan"], state, tools)}

    graph = StateGraph(GraphState)
    graph.add_node("prepare", plan_node)
    graph.add_node("execute", execute_node)
    graph.add_edge(START, "prepare")
    graph.add_edge("prepare", "execute")
    graph.add_edge("execute", END)
    # Offline exercises must not inherit a caller's remote tracing setting.
    with tracing_context(enabled=False):
        output = graph.compile().invoke({"plan": None, "result": {}}, {"recursion_limit": 8})
    return finish(state, output["result"])
