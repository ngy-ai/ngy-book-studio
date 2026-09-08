"""Offline evaluation of deliberately simple, deterministic answer policies."""

from __future__ import annotations

import json
from copy import deepcopy
from typing import Any, TypedDict

from langgraph.graph import END, START, StateGraph
from langsmith import tracing_context

FIELDS = ("start_date", "audience", "allowed_operations")
FACTS = {
    "D1": {"start_date": "2026-10-12"},
    "D2": {"audience": "内部员工", "allowed_operations": "资料检索与阅读"},
    "D3": {"start_date": "2026-11-01"},
}
CASES = [
    {"id": "normal", "available": ["D1", "D2"]},
    {"id": "missing", "available": ["D1"]},
    {"id": "conflict", "available": ["D1", "D2", "D3"]},
]
GOLD = {
    "normal": ("2026-10-12", "内部员工", "资料检索与阅读"),
    "missing": ("2026-10-12", None, None),
    "conflict": (None, "内部员工", "资料检索与阅读"),
}


def grounded_answer(available: list[str]) -> dict[str, Any]:
    answer = {}
    for field in FIELDS:
        evidence = [(sid, FACTS[sid][field]) for sid in available if field in FACTS[sid]]
        values = {value for _, value in evidence}
        answer[field] = {
            "value": next(iter(values)) if len(values) == 1 else None,
            "source_ids": sorted(sid for sid, _ in evidence) if len(values) == 1 else [],
        }
    return answer


def candidate(case: dict, policy: str, repeat: int) -> dict:
    answer = grounded_answer(case["available"])
    if policy == "guess_missing":
        for field, fallback in zip(FIELDS, GOLD["normal"], strict=True):
            if answer[field]["value"] is None:
                answer[field] = {"value": fallback, "source_ids": ["D1"]}
    elif policy == "wrong_source":
        for item in answer.values():
            if item["value"] is not None:
                item["source_ids"] = ["D1"]
    elif policy == "unstable" and repeat == 1:
        answer["start_date"] = {"value": "2030-01-01", "source_ids": ["D1"]}
    return answer


def score(case: dict, answer: dict) -> dict:
    """A deliberately narrow oracle for the fixture's three structured fields."""
    value_correct = 0
    supported_claims = 0
    claims = 0
    unknown_correct = 0
    unknown_targets = 0
    for field, expected in zip(FIELDS, GOLD[case["id"]], strict=True):
        item = answer[field]
        value_correct += item["value"] == expected
        if expected is None:
            unknown_targets += 1
            unknown_correct += item["value"] is None and item["source_ids"] == []
        if item["value"] is not None:
            claims += 1
            sources = item["source_ids"]
            supported_claims += bool(sources) and all(
                sid in case["available"] and FACTS[sid].get(field) == item["value"]
                for sid in sources
            )
    return {
        "value_correct": value_correct,
        "fields": len(FIELDS),
        "supported_claims": supported_claims,
        "claims": claims,
        "unknown_correct": unknown_correct,
        "unknown_targets": unknown_targets,
        "passed": value_correct == len(FIELDS)
        and supported_claims == claims
        and unknown_correct == unknown_targets,
    }


class TrialState(TypedDict):
    case: dict
    policy: str
    repeat: int
    answer: dict
    score: dict


def build_evaluator():
    graph = StateGraph(TrialState)
    graph.add_node(
        "candidate", lambda s: {"answer": candidate(s["case"], s["policy"], s["repeat"])}
    )
    graph.add_node("score", lambda s: {"score": score(s["case"], s["answer"])})
    graph.add_edge(START, "candidate")
    graph.add_edge("candidate", "score")
    graph.add_edge("score", END)
    return graph.compile()


def evaluate(use_framework: bool) -> dict:
    graph = build_evaluator() if use_framework else None
    summaries = {}
    for policy in ("grounded", "guess_missing", "wrong_source", "unstable"):
        rows = []
        for case in CASES:
            for repeat in range(3):
                if graph is None:
                    answer = candidate(case, policy, repeat)
                    result = score(case, answer)
                else:
                    with tracing_context(enabled=False):
                        state = graph.invoke(
                            {
                                "case": deepcopy(case),
                                "policy": policy,
                                "repeat": repeat,
                                "answer": {},
                                "score": {},
                            }
                        )
                    answer, result = state["answer"], state["score"]
                rows.append({"case_id": case["id"], "answer": answer, **result})
        counts = {
            name: sum(row[name] for row in rows)
            for name in (
                "value_correct",
                "fields",
                "supported_claims",
                "claims",
                "unknown_correct",
                "unknown_targets",
            )
        }
        stable_cases = sum(
            len(
                {
                    json.dumps(row["answer"], sort_keys=True, ensure_ascii=False)
                    for row in rows
                    if row["case_id"] == case["id"]
                }
            )
            == 1
            for case in CASES
        )
        summaries[policy] = {
            "trials": len(rows),
            "passed": sum(row["passed"] for row in rows),
            **counts,
            "stable_cases": stable_cases,
            "case_count": len(CASES),
            "failed_cases": sorted({row["case_id"] for row in rows if not row["passed"]}),
        }
    return {
        "dataset_version": "songguo-eval-v1",
        "repeats": 3,
        "policies": summaries,
        "model_calls": 0,
        "measurement_note": "固定响应回归，不测真实模型质量、费用或端到端延迟",
    }


def run_manual() -> dict:
    return evaluate(False)


def run_framework() -> dict:
    return evaluate(True)


if __name__ == "__main__":
    manual, framework = run_manual(), run_framework()
    print(
        json.dumps(
            {"manual": manual, "framework": framework, "equivalent": manual == framework},
            ensure_ascii=False,
            indent=2,
        )
    )
