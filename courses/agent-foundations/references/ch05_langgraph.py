"""第 5 章库版：依赖验证与 StateGraph 条件边逐步推进。"""

from graphlib import CycleError, TopologicalSorter
from typing import TypedDict

from langgraph.graph import END, START, StateGraph
from langsmith import tracing_context

from moye_lab.chapter_support import call_tool, finish, load_case


def prepare(data):
    steps = {step["id"]: step for step in data["steps"]}
    if len(steps) != len(data["steps"]) or any(
        dep not in steps for step in steps.values() for dep in step["depends_on"]
    ):
        return {"status": "invalid_plan", "order": []}
    # Validate the whole dependency graph before any effects, including late cycles.
    try:
        TopologicalSorter({sid: step["depends_on"] for sid, step in steps.items()}).prepare()
    except CycleError:
        return {"status": "invalid_plan", "order": []}
    status = "completed" if not steps else "budget_exhausted" if not data["budget"] else "running"
    return {"status": status, "order": []}


def perform(data, plan, state, tools):
    # Exactly one effect per node. The graph, not this function, repeats execution.
    order = list(plan["order"])
    ready = sorted(
        step["id"]
        for step in data["steps"]
        if step["id"] not in order and all(dep in order for dep in step["depends_on"])
    )
    sid = ready[0]
    call_tool(state, tools, "execute_step", {"step_id": sid})
    order.append(sid)
    status = (
        "completed"
        if len(order) == len(data["steps"])
        else "budget_exhausted"
        if len(order) >= data["budget"]
        else "running"
    )
    return {"status": status, "order": order}


class GraphState(TypedDict):
    result: dict


def run(task, model, tools, limits, emit):
    state = load_case(task, model, tools)
    data = state["case"]["input"]

    def prepare_node(graph_state):
        emit({"phase": "graph_node", "node": "prepare"})
        return {"result": prepare(data)}

    def execute_node(graph_state):
        emit({"phase": "graph_node", "node": "execute_step"})
        return {"result": perform(data, graph_state["result"], state, tools)}

    def route(graph_state):
        return "execute_step" if graph_state["result"]["status"] == "running" else END

    graph = StateGraph(GraphState)
    graph.add_node("prepare", prepare_node)
    graph.add_node("execute_step", execute_node)
    graph.add_edge(START, "prepare")
    graph.add_conditional_edges("prepare", route, ["execute_step", END])
    graph.add_conditional_edges("execute_step", route, ["execute_step", END])
    # Every node updates state before the conditional edge chooses the next step.
    # This graph limit is separate from the host's model/tool budgets.
    with tracing_context(enabled=False):
        output = graph.compile().invoke({"result": {}}, {"recursion_limit": 10})
    return finish(state, output["result"])
