"""The same protocol expressed as model/tool nodes and conditional graph edges."""

from typing import Any, TypedDict

from langgraph.graph import END, START, StateGraph
from langsmith import tracing_context

from moye_lab.contracts import AgentOutcome, BudgetExceeded, Emit, Message, Model, RunLimits, Tools
from moye_lab.implementations.common import Session, parse_final


class AgentState(TypedDict):
    messages: list[Message]
    pending_tools: bool
    answer: dict[str, Any] | None


def run(task: str, model: Model, tools: Tools, limits: RunLimits, emit: Emit) -> AgentOutcome:
    session = Session(task, model, tools, limits, emit)

    def model_node(state: AgentState) -> AgentState:
        emit({"phase": "graph_node", "node": "model"})
        session.messages = list(state["messages"])
        assistant = session.model_step()
        pending = bool(assistant.get("tool_calls"))
        return {
            "messages": list(session.messages),
            "pending_tools": pending,
            "answer": None if pending else parse_final(assistant),
        }

    def tool_node(state: AgentState) -> AgentState:
        emit({"phase": "graph_node", "node": "tools"})
        session.messages = list(state["messages"])
        session.tool_step()
        return {"messages": list(session.messages), "pending_tools": False, "answer": None}

    def next_node(state: AgentState) -> str:
        return "tools" if state["pending_tools"] else "finished"

    graph = StateGraph(AgentState)
    graph.add_node("model", model_node)
    graph.add_node("tools", tool_node)
    graph.add_edge(START, "model")
    graph.add_conditional_edges("model", next_node, {"tools": "tools", "finished": END})
    graph.add_edge("tools", "model")

    try:
        # Explicitly override inherited LangSmith tracing configuration. Model
        # access, if enabled by the runner, still uses the supplied host broker.
        with tracing_context(enabled=False):
            compiled = graph.compile()
            result = compiled.invoke(
                {"messages": list(session.messages), "pending_tools": False, "answer": None},
                config={"recursion_limit": 2 * limits.max_model_decisions + 3},
            )
        return session.completed(result["answer"])
    except BudgetExceeded as error:
        return session.exhausted(error)
