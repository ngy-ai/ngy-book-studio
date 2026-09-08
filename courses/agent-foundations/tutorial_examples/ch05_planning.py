"""第五章：动态生成读取计划、检查依赖、限制实际动作；示例顺序执行。"""

import json
from typing import TypedDict

from langgraph.graph import END, START, StateGraph
from langsmith import tracing_context

DOCUMENTS = {
    "D1": {"start_date": "2026-10-12"},
    "D2": {"audience": "内部员工", "allowed_operations": "资料检索与阅读"},
}
MAX_TOOLS = 3


def search() -> list[str]:
    return sorted(DOCUMENTS)


def build_plan(document_ids: list[str]) -> list[dict]:
    if len(document_ids) != len(set(document_ids)):
        raise ValueError("搜索结果包含重复 ID")
    reads = [
        {"id": f"read:{document_id}", "kind": "read", "document_id": document_id, "after": []}
        for document_id in document_ids
    ]
    return reads + [{"id": "answer", "kind": "answer", "after": [item["id"] for item in reads]}]


def validate_plan(plan: list[dict], allowed_ids: frozenset[str]) -> None:
    ids = [item["id"] for item in plan]
    if len(ids) != len(set(ids)):
        raise ValueError("步骤 ID 不得重复")
    if sum(item["kind"] == "answer" for item in plan) != 1:
        raise ValueError("必须恰好有一个回答步骤")
    for item in plan:
        if item["kind"] not in {"read", "answer"}:
            raise ValueError("计划包含未授权动作")
        if item["kind"] == "read" and item.get("document_id") not in allowed_ids:
            raise ValueError("计划包含未授权资料")
        if not set(item["after"]) <= set(ids):
            raise ValueError("依赖了不存在的步骤")
    reads = {item["id"] for item in plan if item["kind"] == "read"}
    answer = next(item for item in plan if item["kind"] == "answer")
    if set(answer["after"]) != reads:
        raise ValueError("回答必须等待全部读取步骤")
    completed = set()
    while len(completed) != len(ids):
        ready = {
            item["id"]
            for item in plan
            if item["id"] not in completed and set(item["after"]) <= completed
        }
        if not ready:
            raise ValueError("计划存在循环依赖")
        completed.update(ready)


def initial(max_tools: int) -> dict:
    if type(max_tools) is not int or not 1 <= max_tools <= MAX_TOOLS:
        raise ValueError("预算必须为 1..3")
    return {"calls": 0, "max_tools": max_tools, "done": [], "evidence": {}, "audit": []}


def search_step(state: dict) -> dict:
    state = {**state, "calls": state["calls"] + 1, "audit": ["search"]}
    state["document_ids"] = search()
    return state


def plan_step(state: dict) -> dict:
    plan = build_plan(state["document_ids"])
    validate_plan(plan, frozenset(state["document_ids"]))
    return {**state, "plan": plan}


def ready_steps(plan: list[dict], done: list[str]) -> list[dict]:
    return sorted(
        [item for item in plan if item["id"] not in done and set(item["after"]) <= set(done)],
        key=lambda item: item["id"],
    )


def next_step(state: dict) -> dict:
    ready = ready_steps(state["plan"], state["done"])
    if not ready:
        raise ValueError("尚未完成，但没有可执行步骤")
    task = ready[0]  # 独立读取具备并行条件；本实现明确按稳定顺序执行。
    if task["kind"] == "read":
        if state["calls"] >= state["max_tools"]:
            return {**state, "status": "budget_exhausted"}
        source = task["document_id"]
        return {
            **state,
            "calls": state["calls"] + 1,
            "done": [*state["done"], task["id"]],
            "evidence": {**state["evidence"], source: dict(DOCUMENTS[source])},
            "audit": [*state["audit"], task["id"]],
        }
    answer = {
        field: {"value": value, "source_ids": [source]}
        for source, facts in sorted(state["evidence"].items())
        for field, value in facts.items()
    }
    return {
        **state,
        "answer": answer,
        "status": "completed",
        "done": [*state["done"], "answer"],
        "audit": [*state["audit"], "answer"],
    }


def outcome(state: dict) -> dict:
    return {
        "status": state["status"],
        "tool_calls": state["calls"],
        "plan": state["plan"],
        "completed_steps": state["done"],
        "evidence": state["evidence"],
        "answer": state.get("answer"),
        "audit": state["audit"],
    }


def run_manual(max_tools: int = MAX_TOOLS) -> dict:
    state = plan_step(search_step(initial(max_tools)))
    while "status" not in state:
        state = next_step(state)
    return outcome(state)


class WorkflowState(TypedDict, total=False):
    calls: int
    max_tools: int
    done: list[str]
    evidence: dict
    audit: list[str]
    document_ids: list[str]
    plan: list[dict]
    status: str
    answer: dict


def run_framework(max_tools: int = MAX_TOOLS) -> dict:
    graph = StateGraph(WorkflowState)
    graph.add_node("search", search_step)
    graph.add_node("plan", plan_step)
    graph.add_node("step", next_step)
    graph.add_edge(START, "search")
    graph.add_edge("search", "plan")
    graph.add_edge("plan", "step")
    graph.add_conditional_edges(
        "step",
        lambda state: "stop" if "status" in state else "continue",
        {"stop": END, "continue": "step"},
    )
    with tracing_context(enabled=False):
        return outcome(graph.compile().invoke(initial(max_tools), {"recursion_limit": 16}))


def main() -> None:
    manual, framework = run_manual(), run_framework()
    exhausted = run_manual(max_tools=2)
    assert manual == framework
    assert exhausted == run_framework(max_tools=2)
    assert exhausted["status"] == "budget_exhausted" and exhausted["answer"] is None
    print(
        json.dumps(
            {"manual": manual, "framework": framework, "limited": exhausted},
            ensure_ascii=False,
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
