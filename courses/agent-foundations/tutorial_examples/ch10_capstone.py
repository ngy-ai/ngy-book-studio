"""A reproducible offline research-to-approved-draft pipeline, not deployment."""

from __future__ import annotations

import json
from copy import deepcopy
from typing import TypedDict

from langgraph.graph import END, START, StateGraph
from langsmith import tracing_context

from tutorial_examples.ch07_approval import DraftStore, proposal_digest

DOCUMENTS = {
    "D1": {"start_date": "2026-10-12"},
    "D2": {"audience": "内部员工", "allowed_operations": "资料检索与阅读"},
}
FIELDS = ("start_date", "audience", "allowed_operations")


class ResearchState(TypedDict):
    scenario: str
    status: str
    evidence: dict
    missing: list[str]
    answer: dict
    errors: list[str]
    trace: list[str]
    actual_document_reads: int
    virtual_ticks: int
    receipt: str | None


def initial_state(scenario: str) -> ResearchState:
    return {
        "scenario": scenario,
        "status": "running",
        "evidence": {},
        "missing": [],
        "answer": {},
        "errors": [],
        "trace": ["scope:D1,D2"],
        "actual_document_reads": 0,
        "virtual_ticks": 0,
        "receipt": None,
    }


def retrieve(state: ResearchState) -> dict:
    scenario = state["scenario"]
    evidence, missing, trace = {}, [], list(state["trace"])
    reads, ticks = 0, 0
    tool_limit = 1 if scenario == "tool_budget" else 3
    deadline = 15 if scenario == "deadline" else 100
    status = "running"
    # Host-owned scope and fixed plan. Source text cannot append more document IDs.
    allowed_ids = frozenset({"D1", "D2"})
    for sid in ("D1", "D2"):
        if scenario == "cancelled":
            status = "cancelled"
            break
        if sid not in allowed_ids:
            status = "scope_denied"
            break
        for attempt in range(2):
            if reads >= tool_limit:
                status = "tool_budget"
                break
            if ticks >= deadline:
                status = "deadline"
                break
            reads += 1
            ticks += 10  # Synthetic operation clock; no sleeping or timing claim.
            if scenario == "transient" and sid == "D2" and attempt == 0:
                trace.append("read:D2:temporary_error")
                continue
            if scenario == "missing" and sid == "D2":
                missing.append(sid)
                trace.append("read:D2:not_found")
            else:
                evidence[sid] = deepcopy(DOCUMENTS[sid])
                trace.append(f"read:{sid}:ok")
            break
        if ticks >= deadline:
            status = "deadline"
        if status != "running":
            break
    return {
        "evidence": evidence,
        "missing": missing,
        "trace": trace,
        "actual_document_reads": reads,
        "virtual_ticks": ticks,
        "status": status,
    }


def draft(state: ResearchState) -> dict:
    if state["status"] != "running":
        return {}
    answer = {}
    for field in FIELDS:
        sources = [sid for sid, facts in state["evidence"].items() if field in facts]
        values = {state["evidence"][sid][field] for sid in sources}
        answer[field] = {
            "value": next(iter(values)) if len(values) == 1 else None,
            "source_ids": sorted(sources) if len(values) == 1 else [],
        }
    if state["scenario"] == "forged_citation":
        answer["start_date"]["source_ids"] = ["D404"]
    return {"answer": answer, "status": "drafted", "trace": state["trace"] + ["draft"]}


def verify_answer(state: ResearchState) -> list[str]:
    errors = []
    for field in FIELDS:
        item = state["answer"][field]
        if item["value"] is None:
            if item["source_ids"]:
                errors.append(f"{field}:unknown_with_claimed_sources")
            continue
        if not item["source_ids"] or any(
            state["evidence"].get(sid, {}).get(field) != item["value"] for sid in item["source_ids"]
        ):
            errors.append(f"{field}:unsupported_source")
    return errors


def exercise(scenario: str, use_framework: bool, *, verify: bool = True) -> dict:
    store = DraftStore()

    def review(s: ResearchState) -> dict:
        if s["status"] != "drafted":
            return {}
        errors = verify_answer(s) if verify else []
        return {
            "errors": errors,
            "status": "review_failed" if errors else "awaiting_approval",
            "trace": s["trace"] + ["verify" if verify else "verify:removed_for_ablation"],
        }

    def approve_and_save(s: ResearchState) -> dict:
        if s["status"] != "awaiting_approval":
            return {}
        value = {
            "operation": "save_memory_draft",
            "action_id": f"capstone-{scenario}",
            "revision": 1,
            "text": json.dumps(s["answer"], ensure_ascii=False, sort_keys=True),
            "source_ids": sorted(
                {sid for field in s["answer"].values() for sid in field["source_ids"]}
            ),
        }
        # The fixture supplies the human's reply explicitly; no model can approve itself.
        request_digest = proposal_digest(value)
        reply = {
            "decision": "reject" if scenario == "rejected" else "approve",
            "digest": request_digest,
        }
        if reply["decision"] != "approve":
            return {"status": "rejected", "trace": s["trace"] + ["approval:rejected"]}
        if reply["digest"] != proposal_digest(value):
            return {"status": "approval_changed"}
        receipt = store.put(value)
        assert store.put(value) == receipt  # Delivery replay must not write twice.
        return {
            "status": "completed",
            "receipt": receipt,
            "trace": s["trace"] + ["approval:approved", "memory_draft:saved"],
        }

    nodes = (
        ("retrieve", retrieve),
        ("draft", draft),
        ("review", review),
        ("approval", approve_and_save),
    )
    state = initial_state(scenario)
    if use_framework:
        graph = StateGraph(ResearchState)
        for name, node in nodes:
            graph.add_node(name, node)
        graph.add_edge(START, "retrieve")
        graph.add_edge("retrieve", "draft")
        graph.add_edge("draft", "review")
        graph.add_edge("review", "approval")
        graph.add_edge("approval", END)
        with tracing_context(enabled=False):
            state = graph.compile().invoke(state)
    else:
        for _, node in nodes:
            state.update(node(state))
    return {
        **state,
        "draft_writes": store.writes,
        "data_version": "songguo-capstone-v1",
        "pipeline_version": "tutorial-v1",
        "model_calls": 0,
    }


def run(use_framework: bool) -> dict:
    cases = {
        scenario: exercise(scenario, use_framework)
        for scenario in (
            "normal",
            "transient",
            "missing",
            "forged_citation",
            "rejected",
            "cancelled",
            "tool_budget",
            "deadline",
        )
    }
    ablated = exercise("forged_citation", use_framework, verify=False)
    return {
        "cases": cases,
        "ablation": {
            "fault": "forged_citation",
            "with_source_verification": cases["forged_citation"]["status"],
            "without_source_verification": ablated["status"],
            "unsafe_draft_writes_without_check": ablated["draft_writes"],
        },
        "boundary": "全程内存虚构资料；virtual_ticks 不是毫秒；未部署或评测真实模型",
    }


def run_manual() -> dict:
    return run(False)


def run_framework() -> dict:
    return run(True)


if __name__ == "__main__":
    manual, framework = run_manual(), run_framework()
    print(
        json.dumps(
            {"manual": manual, "framework": framework, "equivalent": manual == framework},
            ensure_ascii=False,
            indent=2,
        )
    )
