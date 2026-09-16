"""第 4 章 langgraph 参考示范；阅读或运行示范不代表独立完成。"""

from typing import TypedDict

from langgraph.graph import END, START, StateGraph
from langsmith import tracing_context

from ngy_lab.chapter_support import call_tool, finish, load_case


def prepare(data):
    eligible = []
    for record in data["records"]:
        if (
            record["user_id"] == data["user_id"]
            and record["project_id"] == data["project_id"]
            and record["expires_at"] > data["now"]
            and record["verified"] is True
            and record["status"] == "active"
        ):
            eligible.append(record)
    return sorted(eligible, key=lambda record: record["id"])


def perform(data, plan, state, tools):
    selected = []
    for record in plan:
        reply = call_tool(state, tools, "read_memory", {"memory_id": record["id"]})
        selected.append(
            {"id": record["id"], "field": record["field"], "value": reply["data"]["value"]}
        )
    return {"selected": selected}


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
