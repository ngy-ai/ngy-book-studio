"""第 10 章 langgraph 参考示范；阅读或运行示范不代表独立完成。"""

from typing import TypedDict

from langgraph.graph import END, START, StateGraph
from langsmith import tracing_context

from ngy_lab.chapter_support import call_tool, finish, load_case


def prepare(data):
    return list(data["document_ids"])


def perform(data, plan, state, tools):
    observed = {}
    for sid in plan:
        response = call_tool(state, tools, "read_document", {"document_id": sid})
        if not response["ok"] and response["error"]["retryable"]:
            response = call_tool(state, tools, "read_document", {"document_id": sid})
        if response["ok"]:
            observed[sid] = response["data"]["facts"]
    candidate = call_tool(state, tools, "draft_candidate", {})["data"]
    answer = candidate["answer"]
    valid = True
    for field in data["fields"]:
        value, sources = answer[field]["value"], answer[field]["source_ids"]
        if value is None:
            valid = valid and not sources and not any(field in facts for facts in observed.values())
        else:
            valid = (
                valid
                and bool(sources)
                and len(set(sources)) == len(sources)
                and all(
                    sid in observed and field in observed[sid] and observed[sid][field] == value
                    for sid in sources
                )
            )
    status = (
        "review_failed" if not valid else "rejected" if not candidate["approved"] else "completed"
    )
    receipt = None
    if status == "completed":
        receipt = call_tool(state, tools, "save_draft", {"answer": answer})["data"]["receipt"]
    return {"status": status, "answer": answer, "receipt": receipt}


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
