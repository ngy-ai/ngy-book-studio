"""第 6 章 langgraph 参考示范；阅读或运行示范不代表独立完成。"""

from typing import TypedDict

from langgraph.graph import END, START, StateGraph
from langsmith import tracing_context

from moye_lab.chapter_support import finish, load_case


def prepare(data):
    rows = []
    for sample in data["samples"]:
        checks = []
        for field in sorted(sample["gold"]):
            gold = sample["gold"][field]
            answer = sample["answer"][field]
            value, sources = answer["value"], answer["source_ids"]
            if value is None:
                evidence_ok = not sources
            else:
                evidence_ok = (
                    bool(sources)
                    and len(set(sources)) == len(sources)
                    and all(
                        sid in sample["read_facts"]
                        and field in sample["read_facts"][sid]
                        and sample["read_facts"][sid][field] == value
                        for sid in sources
                    )
                )
            checks.append(
                {
                    "field": field,
                    "value_ok": value == gold,
                    "evidence_ok": evidence_ok,
                    "unknown_ok": value is None and not sources if gold is None else True,
                }
            )
        rows.append({"id": sample["id"], "checks": checks})
    return rows


def perform(data, plan, state, tools):
    # Scoring one field and deciding whether the complete sample passes are separate steps.
    for row in plan:
        row["passed"] = all(
            all(check[name] for name in ("value_ok", "evidence_ok", "unknown_ok"))
            for check in row["checks"]
        )
    return {"rows": plan}


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
