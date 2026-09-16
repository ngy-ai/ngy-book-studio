"""Host policy unit checks; these are not evidence of OS isolation."""

import copy
import json
import runpy
import time
from pathlib import Path

import pytest

from ngy_lab.chapter_runtime import (
    ChapterModel,
    ChapterTools,
    _score,
    evaluate_chapter,
    make_case,
)
from ngy_lab.chapter_support import call_tool, finish, load_case
from ngy_lab.contracts import BudgetExceeded, ProtocolError, RunLimits
from ngy_lab.sandbox import execute_desktop_run

ROOT = Path(__file__).resolve().parents[1]


def environment(chapter, scenario="normal", *, seed=7, limits=None):
    case = make_case(chapter, scenario, seed=seed)
    limits = limits or RunLimits()
    tools = ChapterTools(case, limits)
    model = ChapterModel(case, tools, limits)
    return case, model, tools, limits


def matching_environment(chapter, scenario, predicate):
    for seed in range(80):
        values = environment(chapter, scenario, seed=seed)
        if predicate(values[0]):
            return values
    raise AssertionError("Expected variant was not generated")


@pytest.mark.parametrize("chapter", range(2, 11))
@pytest.mark.parametrize("scenario", ["normal", "fault", "transfer"])
@pytest.mark.parametrize("implementation", ["manual", "langgraph"])
def test_reference_with_different_runtime_inputs(chapter, scenario, implementation):
    run = runpy.run_path(str(ROOT / "references" / f"ch{chapter:02d}_{implementation}.py"))["run"]
    for seed in [7, 29, 91]:
        case, model, tools, limits = environment(chapter, scenario, seed=seed)
        events = []
        outcome = run(case.task, model, tools, limits, events.append)
        checks = evaluate_chapter(case, outcome, model, tools, limits)
        assert all(item["passed"] for item in checks), checks
        assert len(model.records) == 1
        assert tools.executions <= 5
        assert tools.writes <= 1
        if implementation == "langgraph":
            expected_nodes = (
                ["prepare"] + ["execute_step"] * tools.executions
                if chapter == 5
                else ["prepare", "execute"]
            )
            assert [event["node"] for event in events] == expected_nodes


@pytest.mark.parametrize("chapter", range(2, 11))
@pytest.mark.parametrize("implementation", ["manual", "langgraph"])
def test_starters_require_real_implementation(chapter, implementation):
    run = runpy.run_path(str(ROOT / "starters" / f"ch{chapter:02d}_{implementation}.py"))["run"]
    case, model, tools, limits = environment(chapter)
    with pytest.raises(NotImplementedError, match="prepare"):
        run(case.task, model, tools, limits, lambda _: None)
    assert tools.executions == 0


@pytest.mark.parametrize("chapter", range(2, 11))
def test_old_run_answer_is_not_new_run_evidence(chapter):
    case, model, tools, limits = environment(chapter)
    run = runpy.run_path(str(ROOT / "references" / f"ch{chapter:02d}_manual.py"))["run"]
    old = run(case.task, model, tools, limits, lambda _: None)
    fresh, model, tools, limits = environment(chapter, "transfer", seed=43)
    state = load_case(fresh.task, model, tools)
    forged = finish(state, old.answer["result"])
    assert not all(
        check["passed"] for check in evaluate_chapter(fresh, forged, model, tools, limits)
    )
    assert case.case_id != fresh.case_id
    assert case.input != fresh.input


@pytest.mark.parametrize("chapter", [2, 3, 4, 5, 7, 8, 9, 10])
def test_correct_candidate_cannot_replace_required_host_actions(chapter):
    case, model, tools, limits = matching_environment(chapter, "normal", lambda case: case.required)
    state = load_case(case.task, model, tools)
    forged = finish(state, copy.deepcopy(case.expected))
    checks = {
        item["id"]: item["passed"] for item in evaluate_chapter(case, forged, model, tools, limits)
    }
    assert checks["chapter_result"]
    assert not checks["observed_actions"]


@pytest.mark.parametrize("chapter", [True, False, 0, 11, "2", 2.0, None])
def test_desktop_chapter_identity_is_strict(tmp_path, chapter):
    with pytest.raises(ValueError, match="chapter"):
        execute_desktop_run({"chapter": chapter, "run_dir": str(tmp_path)})


@pytest.mark.parametrize(
    "key,value",
    [
        ("course_id", "agent-foundations.chapter-01"),
        ("course_version", "2.0.0"),
        ("rules_version", "2.0.0"),
        ("task_version", "2.0.0"),
    ],
)
def test_desktop_rejects_cross_chapter_or_version_identity(tmp_path, key, value):
    with pytest.raises(ValueError, match=key):
        execute_desktop_run({"chapter": 2, "run_dir": str(tmp_path), key: value})


def test_model_input_is_once_and_observations_are_not_mutable_by_student():
    case, model, tools, _ = environment(3)
    state = load_case(case.task, model, tools)
    state["case"]["input"]["allowed_ids"].append("forged")
    assert "forged" not in case.input["allowed_ids"]
    with pytest.raises(ProtocolError):
        call_tool(state, tools, "read_document", {"document_id": "forged"})
    assert tools.executions == 0


@pytest.mark.parametrize(
    "mutation", ["unknown", "extra", "number", "nonce", "duplicate_json", "nonfinite"]
)
def test_bad_calls_are_rejected_before_execution(mutation):
    case, model, tools, _ = environment(2)
    load_case(case.task, model, tools)
    call = {
        "id": case.case_id + ":1",
        "type": "function",
        "function": {
            "name": "read_document",
            "arguments": json.dumps({"document_id": case.input["allowed_ids"][0]}),
        },
    }
    if mutation == "unknown":
        call["function"]["name"] = "erase_document"
    elif mutation == "extra":
        call["function"]["arguments"] = '{"document_id":"x","force":true}'
    elif mutation == "number":
        call["function"]["arguments"] = '{"document_id":123}'
    elif mutation == "nonce":
        call["id"] = "old-case:1"
    elif mutation == "duplicate_json":
        call["function"]["arguments"] = '{"document_id":"x","document_id":"y"}'
    else:
        call["function"]["arguments"] = '{"document_id":NaN}'
    with pytest.raises(ProtocolError):
        tools.execute(call)
    assert tools.executions == 0
    assert tools.violations


def test_temporary_error_allows_only_one_retry_and_missing_never_retries():
    case, model, tools, _ = environment(10, "fault")
    state = load_case(case.task, model, tools)
    args = {"document_id": case.input["document_ids"][1]}
    assert call_tool(state, tools, "read_document", args)["error"]["retryable"]
    assert call_tool(state, tools, "read_document", args)["ok"]
    with pytest.raises(ProtocolError, match="重试"):
        call_tool(state, tools, "read_document", args)
    assert tools.executions == 2
    case, model, tools, _ = environment(10, "transfer")
    state = load_case(case.task, model, tools)
    args = {"document_id": case.input["document_ids"][1]}
    assert not call_tool(state, tools, "read_document", args)["error"]["retryable"]
    with pytest.raises(ProtocolError, match="重试"):
        call_tool(state, tools, "read_document", args)
    assert tools.executions == 1


def test_tools_enforce_budget_and_deadline():
    case, model, tools, _ = environment(3, limits=RunLimits(max_tool_executions=1))
    state = load_case(case.task, model, tools)
    call_tool(state, tools, "read_document", {"document_id": case.input["allowed_ids"][0]})
    with pytest.raises(BudgetExceeded, match="tool_executions"):
        call_tool(state, tools, "read_document", {"document_id": case.input["allowed_ids"][1]})
    assert tools.executions == 1
    tools.started = time.monotonic() - 181
    with pytest.raises(BudgetExceeded, match="deadline"):
        tools.check_deadline()


def test_evaluator_keeps_value_evidence_and_unknown_separate():
    sample = {
        "id": "sample",
        "gold": {"x": None, "y": "a"},
        "answer": {
            "x": {"value": "a", "source_ids": ["doc"]},
            "y": {"value": "a", "source_ids": ["missing"]},
        },
        "read_facts": {"doc": {"x": "a"}},
    }
    row = _score([sample])["rows"][0]
    assert row["checks"] == [
        {"field": "x", "value_ok": False, "evidence_ok": True, "unknown_ok": False},
        {"field": "y", "value_ok": True, "evidence_ok": False, "unknown_ok": True},
    ]
    assert not row["passed"]


@pytest.mark.parametrize("scenario", ["fault", "transfer"])
def test_approval_rechecked_by_host_even_if_student_grants_it(scenario):
    case, model, tools, _ = matching_environment(
        7, scenario, lambda case: case.expected["status"] != "completed"
    )
    state = load_case(case.task, model, tools)
    with pytest.raises(ProtocolError, match="审批"):
        call_tool(
            state,
            tools,
            "save_draft",
            {"proposal": case.input["proposal"], "digest": case.input["approval"]["digest"]},
        )
    assert tools.writes == tools.executions == 0


def test_protocol_cannot_skip_handshake_or_cross_document_scope():
    case, model, tools, _ = environment(9)
    state = load_case(case.task, model, tools)
    early = call_tool(
        state,
        tools,
        "exchange",
        {"message": {"jsonrpc": "2.0", "id": 42, "method": "tools/list", "params": {}}},
    )
    assert early["data"]["error"]["message"] == "not_initialized"
    for name, args in case.required[:2]:
        call_tool(state, tools, name, args)
    with pytest.raises(ProtocolError, match="授权"):
        call_tool(
            state,
            tools,
            "exchange",
            {
                "message": {
                    "jsonrpc": "2.0",
                    "id": 99,
                    "method": "tools/call",
                    "params": {"name": "read_document", "arguments": {"document_id": "outside"}},
                }
            },
        )
    assert not tools.successful_reads


def test_approval_binds_json_value_types_and_recomputes_submitted_digest():
    case, model, tools, _ = matching_environment(
        7, "normal", lambda case: case.expected["status"] == "completed"
    )
    state = load_case(case.task, model, tools)
    proposal = copy.deepcopy(case.input["proposal"])
    proposal["revision"] = float(proposal["revision"])
    assert proposal == case.input["proposal"]  # Python equality alone is insufficient.
    with pytest.raises(ProtocolError, match="审批"):
        call_tool(
            state,
            tools,
            "save_draft",
            {"proposal": proposal, "digest": case.input["approval"]["digest"]},
        )
    assert tools.executions == tools.writes == 0


@pytest.mark.parametrize("chapter", [7, 10])
@pytest.mark.parametrize("scenario", ["normal", "fault", "transfer"])
def test_scenario_label_does_not_determine_status(chapter, scenario):
    statuses = {make_case(chapter, scenario, seed=seed).expected["status"] for seed in range(80)}
    expected = {
        7: {
            "normal": {"completed", "rejected"},
            "fault": {"expired", "proposal_changed"},
            "transfer": {"completed", "rejected", "expired", "proposal_changed", "cancelled"},
        },
        10: {
            name: {"completed", "rejected", "review_failed"}
            for name in ["normal", "fault", "transfer"]
        },
    }
    assert statuses == expected[chapter][scenario]


def scenario_shortcut(task, model, tools, chapter):
    """Deliberately wrong learner implementation: never inspect approval or evidence."""
    state = load_case(task, model, tools)
    data = state["case"]["input"]
    scenario = state["case"]["scenario"]
    if chapter == 7:
        status = {"normal": "completed", "fault": "expired", "transfer": "proposal_changed"}[
            scenario
        ]
        receipt = None
        if status == "completed":
            receipt = call_tool(
                state,
                tools,
                "save_draft",
                {"proposal": data["proposal"], "digest": data["approval"]["digest"]},
            )["data"]["receipt"]
        return finish(
            state, {"status": status, "writes": int(receipt is not None), "receipt": receipt}
        )
    for sid in data["document_ids"]:
        response = call_tool(state, tools, "read_document", {"document_id": sid})
        if not response["ok"] and response["error"]["retryable"]:
            call_tool(state, tools, "read_document", {"document_id": sid})
    candidate = call_tool(state, tools, "draft_candidate", {})
    answer = candidate["data"]["answer"]
    status = {"normal": "completed", "fault": "review_failed", "transfer": "rejected"}[scenario]
    receipt = None
    if status == "completed":
        receipt = call_tool(state, tools, "save_draft", {"answer": answer})["data"]["receipt"]
    return finish(state, {"status": status, "answer": answer, "receipt": receipt})


@pytest.mark.parametrize("chapter", [7, 10])
@pytest.mark.parametrize("scenario", ["normal", "fault", "transfer"])
def test_scenario_mapping_shortcut_cannot_pass_variant_suite(chapter, scenario):
    failures = 0
    for seed in range(40):
        case, model, tools, limits = environment(chapter, scenario, seed=seed)
        try:
            outcome = scenario_shortcut(case.task, model, tools, chapter)
        except ProtocolError:
            assert tools.violations
            failures += 1
        else:
            checks = evaluate_chapter(case, outcome, model, tools, limits)
            failures += not all(check["passed"] for check in checks)
    assert failures >= 5, "The shortcut must fail repeatedly within each scenario family"


@pytest.mark.parametrize("implementation", ["manual", "langgraph"])
def test_known_value_cannot_be_hidden_as_unknown(implementation):
    seed = next(
        seed
        for seed in range(80)
        if make_case(10, seed=seed).private["candidate"]["date"]["value"] is None
    )
    case, model, tools, limits = environment(10, seed=seed)
    run = runpy.run_path(str(ROOT / "references" / f"ch10_{implementation}.py"))["run"]
    outcome = run(case.task, model, tools, limits, lambda _: None)
    assert outcome.answer["result"]["status"] == "review_failed"
    assert tools.successful_reads
    assert tools.writes == 0
    assert all(check["passed"] for check in evaluate_chapter(case, outcome, model, tools, limits))


@pytest.mark.parametrize(
    "arguments",
    [
        {},
        {"document_id": 123},
        {"document_id": True},
        {"document_id": ""},
        {"document_id": "D1", "force": True},
        ["D1"],
        None,
    ],
)
def test_pydantic_library_contract_rejects_coercion_and_extra_fields(arguments):
    module = runpy.run_path(str(ROOT / "references/ch02_langgraph.py"))
    decision = module["prepare"](
        {
            "allowed_ids": ["D1"],
            "requests": [{"id": "request", "name": "read_document", "arguments": arguments}],
        }
    )[0]
    assert decision["error_code"] == "invalid_arguments"
    assert not decision["accepted"]


@pytest.mark.parametrize("scenario", ["normal", "fault", "transfer"])
def test_plan_graph_routes_each_step_and_stops_before_an_invalid_graph(scenario):
    case, model, tools, limits = environment(5, scenario)
    run = runpy.run_path(str(ROOT / "references/ch05_langgraph.py"))["run"]
    events = []

    def observe(event):
        events.append((event["node"], len(tools.executed_steps)))

    outcome = run(case.task, model, tools, limits, observe)
    assert events == [("prepare", 0)] + [
        ("execute_step", index) for index in range(len(case.expected["order"]))
    ]
    assert tools.executed_steps == case.expected["order"]
    assert outcome.answer["result"] == case.expected


@pytest.mark.parametrize("chapter", range(2, 11))
def test_offline_library_references_disable_inherited_tracing(chapter, monkeypatch):
    from langsmith.utils import tracing_is_enabled

    monkeypatch.setenv("LANGSMITH_TRACING", "true")
    case, model, tools, limits = environment(chapter)
    run = runpy.run_path(str(ROOT / "references" / f"ch{chapter:02d}_langgraph.py"))["run"]
    observations = []
    run(case.task, model, tools, limits, lambda _: observations.append(tracing_is_enabled()))
    assert observations and not any(observations)


@pytest.mark.parametrize(
    "scenario,counts", [("normal", {1, 2}), ("fault", {0, 1, 2}), ("transfer", {1, 2, 3})]
)
def test_memory_eligibility_and_fields_vary_within_each_scenario(scenario, counts):
    cases = [make_case(4, scenario, seed=seed) for seed in range(60)]
    assert {len(case.expected["selected"]) for case in cases} == counts
    assert len({case.input["now"] for case in cases}) > 1
    assert len({item["field"] for case in cases for item in case.expected["selected"]}) == 3
    assert all(len(case.input["records"]) == 6 for case in cases)


@pytest.mark.parametrize("scenario", ["normal", "fault", "transfer"])
@pytest.mark.parametrize("strategy", ["legacy_suffix", "sorted_position"])
def test_memory_id_and_position_shortcuts_fail(scenario, strategy):
    failures = 0
    for seed in range(20):
        case, model, tools, limits = environment(4, scenario, seed=seed)
        state = load_case(case.task, model, tools)
        records = sorted(state["case"]["input"]["records"], key=lambda row: row["id"])
        if strategy == "legacy_suffix":
            suffixes = {"normal": ["M0"], "fault": [], "transfer": ["M0", "M5"]}[scenario]
            selected = [r for r in records if any(r["id"].endswith(suffix) for suffix in suffixes)]
        else:
            selected = records[: {"normal": 1, "fault": 0, "transfer": 2}[scenario]]
        result = []
        try:
            for record in selected:
                value = call_tool(state, tools, "read_memory", {"memory_id": record["id"]})["data"][
                    "value"
                ]
                result.append({"id": record["id"], "field": record["field"], "value": value})
        except ProtocolError:
            failures += 1
        else:
            checks = evaluate_chapter(
                case, finish(state, {"selected": result}), model, tools, limits
            )
            failures += not all(check["passed"] for check in checks)
    assert failures >= 5


@pytest.mark.parametrize("scenario", ["normal", "fault", "transfer"])
def test_evaluation_samples_vary_fields_and_failure_kinds(scenario):
    cases = [make_case(6, scenario, seed=seed) for seed in range(30)]
    fields = {tuple(sorted(sample["gold"])) for case in cases for sample in case.input["samples"]}
    patterns = {
        tuple(
            (check["value_ok"], check["evidence_ok"], check["unknown_ok"])
            for check in row["checks"]
        )
        for case in cases
        for row in case.expected["rows"]
    }
    assert len(fields) >= 3
    assert len(patterns) >= 6
    assert all(len(case.input["samples"]) == 5 for case in cases)


@pytest.mark.parametrize("scenario", ["normal", "fault", "transfer"])
@pytest.mark.parametrize("strategy", ["legacy_suffix", "sorted_position"])
def test_score_id_and_position_shortcuts_fail(scenario, strategy):
    failures = 0
    for seed in range(20):
        case, model, tools, limits = environment(6, scenario, seed=seed)
        state = load_case(case.task, model, tools)
        samples = state["case"]["input"]["samples"]
        positions = {
            sid: index for index, sid in enumerate(sorted(sample["id"] for sample in samples))
        }
        rows = []
        for sample in samples:
            # The old S0..S4 table never inspected gold, answer or read_facts.
            index = (
                positions[sample["id"]]
                if strategy == "sorted_position"
                else next((index for index in range(5) if sample["id"].endswith(f"S{index}")), 0)
            )
            checks = [
                {
                    "field": "audience",
                    "value_ok": index != 3,
                    "evidence_ok": index != 3,
                    "unknown_ok": index != 3,
                },
                {
                    "field": "date",
                    "value_ok": index != 2 or scenario == "transfer",
                    "evidence_ok": index not in [1, 2]
                    and not (index == 4 and scenario != "normal")
                    or index == 2
                    and scenario == "transfer",
                    "unknown_ok": True,
                },
            ]
            rows.append(
                {
                    "id": sample["id"],
                    "checks": checks,
                    "passed": all(
                        all(check[name] for name in ["value_ok", "evidence_ok", "unknown_ok"])
                        for check in checks
                    ),
                }
            )
        checks = evaluate_chapter(case, finish(state, {"rows": rows}), model, tools, limits)
        failures += not all(check["passed"] for check in checks)
    assert failures >= 5


@pytest.mark.parametrize("scenario", ["normal", "transfer"])
def test_plan_id_sorting_cannot_replace_dependency_checks(scenario):
    failures = 0
    for seed in range(20):
        case, model, tools, limits = environment(5, scenario, seed=seed)
        state = load_case(case.task, model, tools)
        data = state["case"]["input"]
        order = sorted(step["id"] for step in data["steps"])[: data["budget"]]
        try:
            for sid in order:
                call_tool(state, tools, "execute_step", {"step_id": sid})
        except ProtocolError:
            failures += 1
        else:
            outcome = finish(
                state,
                {
                    "status": "completed" if scenario == "normal" else "budget_exhausted",
                    "order": order,
                },
            )
            failures += not all(
                check["passed"] for check in evaluate_chapter(case, outcome, model, tools, limits)
            )
    assert failures >= 5


def test_multi_agent_faults_do_not_always_change_the_schedule_claim():
    rejected_roles = set()
    for seed in range(30):
        case = make_case(8, "fault", seed=seed)
        reports = case.input["reports"]
        rejected_roles.update(
            report["assigned_role"]
            for report in reports
            if report["id"] in case.expected["rejected_claims"]
            and report["source_id"]
            in case.input["assignments"][report["assigned_role"]]["source_ids"]
        )
    assert rejected_roles == {"schedule", "policy"}


def test_multi_agent_reading_then_guessing_rejections_from_roles_fails():
    from ngy_lab.chapter_runtime import _aggregate

    failures = 0
    for seed in range(20):
        case, model, tools, limits = environment(8, "fault", seed=seed)
        state = load_case(case.task, model, tools)
        data = state["case"]["input"]
        authorized = sorted(
            {
                (report["assigned_role"], report["source_id"])
                for report in data["reports"]
                if report["source_id"] in data["assignments"][report["assigned_role"]]["source_ids"]
                and report["field"] in data["assignments"][report["assigned_role"]]["fields"]
            }
        )
        for role, sid in authorized:
            # The shortcut performs reads but discards the actual facts.
            call_tool(state, tools, "read_document", {"role": role, "document_id": sid})
        accepted, rejected = {}, []
        for report in data["reports"]:
            role, sid = report["assigned_role"], report["source_id"]
            if (role, sid) not in authorized or role == "schedule":
                rejected.append(report["id"])
            else:
                accepted.setdefault(sid, {})[report["field"]] = report["value"]
        outcome = finish(
            state,
            {"fields": _aggregate(data["fields"], accepted), "rejected_claims": sorted(rejected)},
        )
        checks = {
            check["id"]: check["passed"]
            for check in evaluate_chapter(case, outcome, model, tools, limits)
        }
        assert checks["observed_actions"]
        failures += not checks["chapter_result"]
    assert failures >= 5
