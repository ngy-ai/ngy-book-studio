"""Check observable tutorial behavior, independently of printed demo assertions."""

import copy
import importlib
import json
from datetime import date

import pytest

from tutorial_examples import CHAPTER_MODULES
from tutorial_examples.__main__ import compare_chapter
from tutorial_examples.ch02_tools import Host, supported
from tutorial_examples.ch03_retrieval import compose, read_evidence, retrieve, split_documents
from tutorial_examples.ch04_memory import CORRECTION, MemoryStore
from tutorial_examples.ch05_planning import build_plan, ready_steps, validate_plan


@pytest.mark.parametrize("chapter", CHAPTER_MODULES)
def test_paired_examples_are_repeatable_and_json_serializable(chapter):
    first = compare_chapter(chapter)
    second = compare_chapter(chapter)
    assert first["same_output"], first
    assert first == second, "a fresh example must not inherit a previous run's memory or approval"
    assert json.loads(json.dumps(first, ensure_ascii=False)) == first


def test_tool_boundary_rejects_invalid_and_unauthorized_calls_before_reading():
    module = importlib.import_module("tutorial_examples.ch02_tools")
    for run in (module.run_manual, module.run_framework):
        result = run()
        assert [event["code"] for event in result["audit"]] == [
            "UNKNOWN_TOOL",
            "INVALID_ARGUMENTS",
            "INVALID_ARGUMENTS",
            "FORBIDDEN",
            "OK",
            "OK",
        ]
        assert result["successful_reads"] == ["D1", "D2"]
        assert result["answer"]["start_date"] == {"value": "2026-10-12", "source_ids": ["D1"]}
        assert not result["forged_answer_accepted"]
        assert not supported(result["answer"], {}), "source IDs alone do not prove a read occurred"
        host = Host()
        host.read("D1")
        host.read("D2")
        forged = copy.deepcopy(result["answer"])
        forged["audience"]["source_ids"] = ["D1"]
        assert not supported(forged, host.reads), "a read source must support this particular fact"


def test_retrieval_filters_before_ranking_and_rechecks_loaded_evidence():
    chunks = split_documents()
    hits = retrieve("松果项目试运行开始日期、面向对象、允许操作均在此", chunks, "pine", limit=1)
    assert len(hits) == 1
    assert hits[0]["document_id"] != "PRIVATE"
    assert read_evidence([{"chunk_id": "PRIVATE:p1"}], chunks, "pine") == []
    assert compose([])["start_date"]["status"] == "unknown"
    result = importlib.import_module("tutorial_examples.ch03_retrieval").run_manual()
    date_answer = result["answer"]["start_date"]
    assert date_answer["status"] == "conflict"
    assert date_answer["value"] is None and date_answer["source_ids"] == []
    assert {claim["value"] for claim in date_answer["alternatives"]} == {"2026-10-12", "2026-10-15"}
    assert result["answer"]["audience"]["value"] == "内部员工"


def test_memory_checks_scope_expiry_and_correction_without_erasing_history():
    store = MemoryStore()
    before = store.recall("learner", "pine", date(2026, 10, 11))
    assert [record["id"] for record in before["records"]] == ["M1"]
    assert before["rejected_counts"] == {"scope": 2, "expired": 1, "unverified": 1, "superseded": 0}
    fabricated = {**CORRECTION, "value": "2027-01-01"}
    with pytest.raises(ValueError):
        store.apply_verified_correction("learner", "pine", fabricated)
    assert store.apply_verified_correction("learner", "pine", CORRECTION) == "applied"
    count = len(store.records)
    assert store.apply_verified_correction("learner", "pine", CORRECTION) == "already_applied"
    assert len(store.records) == count
    assert next(item for item in store.records if item.id == "M1").status == "superseded"
    assert store.recall("learner", "pine", date(2026, 10, 20))["records"] == []


def test_planning_rejects_cycles_and_keeps_budget_exhaustion_explicit():
    plan = build_plan(["D1", "D2"])
    assert {step["id"] for step in ready_steps(plan, [])} == {"read:D1", "read:D2"}
    plan[0]["after"] = ["answer"]
    with pytest.raises(ValueError, match="循环依赖"):
        validate_plan(plan, frozenset({"D1", "D2"}))
    module = importlib.import_module("tutorial_examples.ch05_planning")
    for budget in (1, 2):
        for run in (module.run_manual, module.run_framework):
            result = run(max_tools=budget)
            assert result["tool_calls"] == budget
            assert result["status"] == "budget_exhausted"
            assert result["answer"] is None
            assert "answer" not in result["completed_steps"]


def test_evaluation_separates_correct_facts_supported_sources_and_repeatability():
    module = importlib.import_module("tutorial_examples.ch06_evaluation")
    summary = module.run_manual()["policies"]
    assert summary["grounded"]["passed"] == 9
    assert summary["wrong_source"]["value_correct"] == summary["wrong_source"]["fields"]
    assert summary["wrong_source"]["passed"] < summary["wrong_source"]["trials"]
    assert summary["guess_missing"]["unknown_correct"] == 0
    assert summary["unstable"]["stable_cases"] == 0
    case = {"id": "normal", "available": ["D1", "D2"]}
    forged = module.grounded_answer(case["available"])
    forged["audience"]["source_ids"] = ["D1"]
    assert not module.score(case, forged)["passed"]


def test_approval_is_bound_to_the_shown_draft_and_replay_writes_once():
    module = importlib.import_module("tutorial_examples.ch07_approval")
    for run in (module.run_manual, module.run_framework):
        outcomes = run()
        for result in outcomes.values():
            assert result["writes_before_resume"] == 0
        approved = outcomes["approved"]
        assert approved["status"] == "completed" and approved["writes"] == 1
        assert approved["receipt"] == approved["duplicate_receipt"]
        for scenario, status in {
            "rejected": "rejected",
            "expired": "expired",
            "cancelled": "cancelled",
            "tampered": "invalid_approval",
            "changed": "proposal_changed",
        }.items():
            assert outcomes[scenario]["status"] == status
            assert outcomes[scenario]["writes"] == 0
            assert outcomes[scenario]["drafts"] == []
    store = module.DraftStore()
    value = module.proposal()
    store.put(value)
    with pytest.raises(ValueError, match="idempotency_conflict"):
        store.put({**value, "text": "另一个未批准的草稿"})


def test_multi_agent_conflicts_are_preserved_instead_of_voted_away():
    module = importlib.import_module("tutorial_examples.ch08_multi_agent")
    for run in (module.run_manual, module.run_framework):
        outcomes = run()
        normal = outcomes["normal"]
        assert normal["tool_reads"] == 2 and normal["handoffs"] == 2
        assert normal["single_worker_same_answer"]
        conflicting = outcomes["conflict"]
        assert conflicting["answer"]["start_date"] == {"value": None, "source_ids": []}
        assert len(conflicting["conflicts"]) == 1
        assert conflicting["answer"]["audience"]["value"] == "内部员工"
        assert outcomes["forged_claims_rejected"] == 1
        assert outcomes["fabricated_read_log_rejected"] == 1
        assert outcomes["stolen_role_claims_rejected"] == 2
        assert outcomes["rejected_plan"] == "handoff_budget"


def test_multi_agent_reads_are_observed_by_the_host_and_cannot_be_reassigned():
    module = importlib.import_module("tutorial_examples.ch08_multi_agent")
    ledger = module.EvidenceLedger()
    forged = {
        "role": "timeline",
        "read_ids": ["D1"],
        "claims": [{"field": "start_date", "value": "2026-10-12", "source_id": "D1"}],
    }
    result = module.reconcile({"timeline": forged}, ledger)
    assert len(result["rejected_claims"]) == 1
    assert result["answer"]["start_date"]["value"] is None
    observed = ledger.read("timeline", "D1")
    observed["start_date"] = "2030-01-01"
    assert (
        module.reconcile({"timeline": forged}, ledger)["answer"]["start_date"]["value"]
        == "2026-10-12"
    )
    assert len(module.reconcile({"policy": forged}, ledger)["rejected_claims"]) == 1
    with pytest.raises(PermissionError, match="role_scope_denied"):
        ledger.read("policy", "D1")
    assert ledger.actual_reads == 1


def test_protocol_requires_initialization_and_distinguishes_tool_errors():
    module = importlib.import_module("tutorial_examples.ch09_protocol")
    client = module.LocalClient()
    assert client.exchange("tools/list")["error"]["code"] == -32600
    assert client.initialize() == module.PROTOCOL
    assert client.exchange("unknown/method")["error"]["code"] == -32601
    denied = client.exchange(
        "tools/call", {"name": "read_document", "arguments": {"document_id": "OTHER_PROJECT"}}
    )
    assert "error" not in denied and denied["result"]["isError"] is True
    assert "structuredContent" not in denied["result"]
    with pytest.raises(ValueError, match="unsupported_protocol_version"):
        module.LocalClient(("2099-01-01",)).initialize()
    with pytest.raises(ValueError, match="request_too_large"):
        client.exchange("tools/call", {"padding": "x" * 5000})


@pytest.mark.parametrize("invalid_id", [True, [], {}, None])
def test_protocol_does_not_echo_invalid_request_ids(invalid_id):
    module = importlib.import_module("tutorial_examples.ch09_protocol")
    peer = module.LocalPeer({"D1"})
    response = peer.receive({"jsonrpc": "2.0", "id": invalid_id, "method": "tools/list"})
    assert response["id"] is None
    assert response["error"]["code"] == -32600


def test_capstone_stops_before_writing_on_failure_and_shows_verification_ablation():
    module = importlib.import_module("tutorial_examples.ch10_capstone")
    for run in (module.run_manual, module.run_framework):
        result = run()
        cases = result["cases"]
        assert cases["normal"]["status"] == "completed"
        assert cases["normal"]["draft_writes"] == 1
        assert cases["transient"]["actual_document_reads"] == 3
        assert cases["missing"]["answer"]["audience"] == {"value": None, "source_ids": []}
        for scenario, status in {
            "forged_citation": "review_failed",
            "rejected": "rejected",
            "cancelled": "cancelled",
            "tool_budget": "tool_budget",
            "deadline": "deadline",
        }.items():
            assert cases[scenario]["status"] == status
            assert cases[scenario]["draft_writes"] == 0
        assert cases["cancelled"]["actual_document_reads"] == 0
        assert cases["tool_budget"]["actual_document_reads"] == 1
        assert result["ablation"]["with_source_verification"] == "review_failed"
        assert result["ablation"]["without_source_verification"] == "completed"
        assert result["ablation"]["unsafe_draft_writes_without_check"] == 1
