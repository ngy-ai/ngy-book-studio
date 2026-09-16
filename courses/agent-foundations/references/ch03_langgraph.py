"""第 3 章 langgraph 参考示范；阅读或运行示范不代表独立完成。"""

import json
from typing import TypedDict

from langgraph.graph import END, START, StateGraph
from langsmith import tracing_context

from ngy_lab.chapter_support import call_tool, finish, load_case


def prepare(data):
    # Only metadata participates in selection; it is not body evidence.
    return sorted(
        document["id"]
        for document in data["documents"]
        if document["id"] in data["allowed_ids"]
        and set(document["terms"]) & set(data["query_terms"])
    )


def perform(data, plan, state, tools):
    evidence = {}
    for sid in plan:
        reply = call_tool(state, tools, "read_document", {"document_id": sid})
        if reply["ok"]:
            evidence[sid] = reply["data"]["facts"]
    fields = {}
    for field in data["fields"]:
        entries = [(sid, facts[field]) for sid, facts in evidence.items() if field in facts]
        unique = {json.dumps(value, sort_keys=True) for _, value in entries}
        status = "unknown" if not entries else "supported" if len(unique) == 1 else "conflict"
        fields[field] = {
            "status": status,
            "value": entries[0][1] if status == "supported" else None,
            "source_ids": sorted(sid for sid, _ in entries),
        }
    return {"fields": fields}


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
