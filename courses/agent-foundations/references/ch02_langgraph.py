"""第 2 章 langgraph 参考示范；阅读或运行示范不代表独立完成。"""

from typing import TypedDict

from langgraph.graph import END, START, StateGraph
from langsmith import tracing_context
from pydantic import BaseModel, ConfigDict, Field, ValidationError

from ngy_lab.chapter_support import call_tool, finish, load_case


class ReadArguments(BaseModel):
    model_config = ConfigDict(strict=True, extra="forbid")
    document_id: str = Field(min_length=1)


def prepare(data):
    decisions = []
    for request in data["requests"]:
        args = request["arguments"]
        error = None
        if request["name"] != "read_document":
            error = "unknown_tool"
        else:
            try:
                validated = ReadArguments.model_validate(args)
            except ValidationError:
                error = "invalid_arguments"
            else:
                # A valid shape is not permission to read that document.
                if validated.document_id not in data["allowed_ids"]:
                    error = "outside_scope"
        decisions.append(
            {
                "request_id": request["id"],
                "accepted": error is None,
                "error_code": error,
                "data": None,
            }
        )
    return decisions


def perform(data, plan, state, tools):
    for request, decision in zip(data["requests"], plan, strict=True):
        if decision["accepted"]:
            result = call_tool(state, tools, "read_document", request["arguments"])
            decision["data"] = result["data"]
    return {"decisions": plan}


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
