"""Bounded specialist handoffs with shared evidence and deterministic merging."""

from __future__ import annotations

import json
from copy import deepcopy
from threading import Lock
from typing import TypedDict

from langgraph.graph import END, START, StateGraph
from langsmith import tracing_context

DOCS = {
    "D1": {"start_date": "2026-10-12"},
    "D2": {"audience": "内部员工", "allowed_operations": "资料检索与阅读"},
    "D3": {"start_date": "2026-11-01"},
}
FIELDS = ("start_date", "audience", "allowed_operations")
ROLE_FIELDS = {"timeline": {"start_date"}, "policy": {"audience", "allowed_operations"}}
ROLE_SOURCES = {"timeline": {"D1", "D3"}, "policy": {"D2"}}


class EvidenceLedger:
    """Host-owned observations; worker self-reports are never evidence of a read."""

    def __init__(self, *, single_worker: bool = False) -> None:
        self.allowed_sources = {"single": set(DOCS)} if single_worker else ROLE_SOURCES
        self.allowed_fields = {"single": set(FIELDS)} if single_worker else ROLE_FIELDS
        self._observations: dict[tuple[str, str], dict] = {}
        self._lock = Lock()
        self.actual_reads = 0

    def read(self, role: str, sid: str) -> dict:
        with self._lock:
            if sid not in self.allowed_sources.get(role, set()):
                raise PermissionError("role_scope_denied")
            if self.actual_reads >= 3:
                raise ValueError("team_read_budget")
            self.actual_reads += 1
            facts = deepcopy(DOCS[sid])
            self._observations[role, sid] = deepcopy(facts)
            return facts

    def supports(self, role: str, claim: dict) -> bool:
        sid, field = claim["source_id"], claim["field"]
        with self._lock:
            return (
                sid in self.allowed_sources.get(role, set())
                and field in self.allowed_fields.get(role, set())
                and (role, sid) in self._observations
                and field in self._observations[role, sid]
                and self._observations[role, sid][field] == claim["value"]
            )


def specialist(role: str, source_ids: list[str], ledger: EvidenceLedger) -> dict:
    if len(source_ids) > 2:
        raise ValueError("worker_read_budget")
    claims = [
        {"field": field, "value": value, "source_id": sid}
        for sid in sorted(source_ids)
        for field, value in ledger.read(role, sid).items()
        if field in ROLE_FIELDS[role]
    ]
    return {
        "role": role,
        "read_ids": sorted(source_ids),
        "claims": claims,
        "tool_reads": len(source_ids),
    }


def reconcile(reports: dict[str, dict], ledger: EvidenceLedger) -> dict:
    claims = []
    rejected = []
    # Mapping keys come from scheduler assignments, not the untrusted report.
    for assigned_role, report in sorted(reports.items()):
        for claim in report["claims"]:
            if ledger.supports(assigned_role, claim):
                claims.append(claim)
            else:
                rejected.append(claim)
    answer, conflicts = {}, []
    for field in FIELDS:
        relevant = [c for c in claims if c["field"] == field]
        values = {c["value"] for c in relevant}
        if len(values) == 1:
            answer[field] = {
                "value": next(iter(values)),
                "source_ids": sorted({c["source_id"] for c in relevant}),
            }
        else:
            answer[field] = {"value": None, "source_ids": []}
            if len(values) > 1:
                conflicts.append(
                    {
                        "field": field,
                        "candidates": sorted(relevant, key=lambda item: item["source_id"]),
                    }
                )
    return {"answer": answer, "conflicts": conflicts, "rejected_claims": rejected}


class WorkerState(TypedDict):
    source_ids: list[str]
    report: dict


class TeamState(TypedDict):
    timeline_ids: list[str]
    timeline_report: dict
    policy_report: dict
    result: dict


def worker_graph(role: str, ledger: EvidenceLedger):
    graph = StateGraph(WorkerState)
    graph.add_node(
        "read_and_extract", lambda s: {"report": specialist(role, s["source_ids"], ledger)}
    )
    graph.add_edge(START, "read_and_extract")
    graph.add_edge("read_and_extract", END)
    return graph.compile()


def team_graph(ledger: EvidenceLedger):
    timeline, policy = worker_graph("timeline", ledger), worker_graph("policy", ledger)
    graph = StateGraph(TeamState)
    graph.add_node(
        "timeline",
        lambda s: {
            "timeline_report": timeline.invoke({"source_ids": s["timeline_ids"], "report": {}})[
                "report"
            ]
        },
    )
    graph.add_node(
        "policy",
        lambda _: {"policy_report": policy.invoke({"source_ids": ["D2"], "report": {}})["report"]},
    )
    graph.add_node(
        "review",
        lambda s: {
            "result": reconcile(
                {"timeline": s["timeline_report"], "policy": s["policy_report"]}, ledger
            )
        },
    )
    graph.add_edge(START, "timeline")
    graph.add_edge(START, "policy")
    # This list-form edge is a join: review waits for both predecessor nodes.
    graph.add_edge(["timeline", "policy"], "review")
    graph.add_edge("review", END)
    return graph.compile()


def check_handoffs(roles: list[str], limit: int = 2) -> None:
    if len(roles) > limit or len(set(roles)) != len(roles):
        raise ValueError("handoff_budget")
    if set(roles) != set(ROLE_FIELDS):
        raise ValueError("invalid_team_plan")


def exercise(conflict: bool, use_framework: bool) -> dict:
    check_handoffs(["timeline", "policy"])
    ledger = EvidenceLedger()
    timeline_ids = ["D1", "D3"] if conflict else ["D1"]
    if use_framework:
        with tracing_context(enabled=False):
            state = team_graph(ledger).invoke(
                {
                    "timeline_ids": timeline_ids,
                    "timeline_report": {},
                    "policy_report": {},
                    "result": {},
                }
            )
        reports = [state["timeline_report"], state["policy_report"]]
        result = state["result"]
    else:
        # A bounded serial scheduler. It is intentionally not described as parallel.
        reports = [
            specialist("timeline", timeline_ids, ledger),
            specialist("policy", ["D2"], ledger),
        ]
        result = reconcile({"timeline": reports[0], "policy": reports[1]}, ledger)
    # One worker sees the same authorized documents; it is the comparison baseline.
    ids = sorted(timeline_ids + ["D2"])
    single_ledger = EvidenceLedger(single_worker=True)
    single = {
        "role": "single",
        "read_ids": ids,
        "claims": [
            {"field": field, "value": value, "source_id": sid}
            for sid in ids
            for field, value in single_ledger.read("single", sid).items()
        ],
    }
    return {
        **result,
        "reports": sorted(reports, key=lambda item: item["role"]),
        "handoffs": 2,
        "tool_reads": ledger.actual_reads,
        "single_worker_same_answer": reconcile({"single": single}, single_ledger)["answer"]
        == result["answer"],
        "single_worker_handoffs": 0,
        "model_calls": 0,
    }


def run(use_framework: bool) -> dict:
    try:
        check_handoffs(["timeline", "policy", "timeline"])
    except ValueError as error:
        rejected_plan = str(error)
    else:
        raise AssertionError("the cyclic handoff plan must be rejected")
    ledger = EvidenceLedger()
    forged = specialist("timeline", ["D1"], ledger)
    forged["claims"][0]["value"] = "2030-01-01"
    fabricated_read = {
        "role": "timeline",
        "read_ids": ["D3"],
        "claims": [{"field": "start_date", "value": "2026-11-01", "source_id": "D3"}],
    }
    stolen_role = specialist("policy", ["D2"], ledger)
    return {
        "normal": exercise(False, use_framework),
        "conflict": exercise(True, use_framework),
        "rejected_plan": rejected_plan,
        "forged_claims_rejected": len(reconcile({"timeline": forged}, ledger)["rejected_claims"]),
        "fabricated_read_log_rejected": len(
            reconcile({"timeline": fabricated_read}, ledger)["rejected_claims"]
        ),
        "stolen_role_claims_rejected": len(
            reconcile({"timeline": stolen_role}, ledger)["rejected_claims"]
        ),
        "note": "角色逻辑是固定抽取函数，未调用语言模型，也未测并发提速",
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
