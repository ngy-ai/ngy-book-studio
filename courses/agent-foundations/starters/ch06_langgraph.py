"""第 6 章 langgraph 独立练习骨架。"""

from typing import TypedDict

from langgraph.graph import END, START, StateGraph
from langsmith import tracing_context

from moye_lab.chapter_support import finish, load_case


def prepare(data):
    # 在这里实现本章的验证、筛选或计划；输入形状见本章桌面练习说明。
    raise NotImplementedError("先实现 prepare：不要把参考输出或旧资料 ID 写成常量")


def perform(data, plan, state, tools):
    # 需要调用工具时：from moye_lab.chapter_support import call_tool
    # call_tool(state, tools, 名称, 参数) 返回 ok/data 或 ok/error。
    # 返回本章 result 字典；宿主会独立核验真实动作与本次数据。
    raise NotImplementedError("再实现 perform：保留实际工具响应并形成候选结果")


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
