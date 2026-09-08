"""第 5 章 langgraph 独立练习骨架。"""

from typing import TypedDict

from langgraph.graph import END, START, StateGraph
from langsmith import tracing_context

from moye_lab.chapter_support import finish, load_case


def prepare(data):
    # 验证完整依赖图；返回 {"status": "running" 或终止状态, "order": []}。
    raise NotImplementedError("先实现 prepare：不要把参考输出或旧资料 ID 写成常量")


def perform(data, plan, state, tools):
    # 需要调用工具时：from moye_lab.chapter_support import call_tool
    # call_tool(state, tools, 名称, 参数) 返回 ok/data 或 ok/error。
    # 每次只执行一个就绪步骤，返回更新的 order 与 status；条件边决定是否继续。
    raise NotImplementedError("再实现 perform：保留实际工具响应并形成候选结果")


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
