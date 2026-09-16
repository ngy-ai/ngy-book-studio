"""Integration checks use broker observations, never a learner's success claims."""

import copy
import json
import time

import pytest

from ngy_lab import runner
from ngy_lab.contracts import AgentOutcome, ProtocolError, RunLimits
from ngy_lab.implementations.common import initial_messages
from ngy_lab.runtime import ObservedModel, ScriptedModel, ToolBroker, json_text
from ngy_lab.scenarios import get_scenario

EXPECTED_COUNTS = {
    "normal": (3, 3),
    "invalid_call": (4, 3),
    "invalid_arguments": (4, 3),
    "transient": (3, 4),
    "missing": (3, 3),
    "transfer": (3, 4),
    "model_budget": (8, 0),
    "tool_budget": (7, 6),
}


def checks(report):
    return {check["id"]: check["passed"] for check in report["checks"]}


@pytest.mark.parametrize("implementation", ["manual", "langgraph"])
@pytest.mark.parametrize("scenario_name,counts", EXPECTED_COUNTS.items())
def test_both_references_pass_all_scripted_scenarios_with_observed_counts(
    implementation, scenario_name, counts
):
    report = runner.execute_run(implementation, scenario_name)

    assert report["passed"], report["checks"]
    assert report["error"] is None
    assert (
        report["metrics"]["model_decisions"],
        report["metrics"]["actual_tool_executions"],
    ) == counts
    assert report["outcome"]["stop_reason"] == get_scenario(scenario_name).expected_stop
    assert report["learning"]["mastery"] is None
    if scenario_name == "tool_budget":
        assert report["metrics"]["tool_dispatch_attempts"] == 7
        refusal = report["observations"]["budget_events"][-1]
        assert refusal["reason"] == "tool_budget"
        assert refusal["dispatch_attempt"] == 7
        assert (
            refusal["refused_call"]
            == report["observations"]["model_calls"][-1]["response"]["tool_calls"][0]
        )
    if implementation == "langgraph":
        nodes = [
            event["node"] for event in report["learner_events"] if event["phase"] == "graph_node"
        ]
        assert report["metrics"]["framework_steps"] == len(nodes) > 0
        assert nodes[0] == "model"
        assert "tools" in nodes
    else:
        assert report["metrics"]["framework_steps"] == 0


@pytest.mark.parametrize("scenario_name", ["invalid_call", "invalid_arguments"])
def test_invalid_requests_are_returned_as_errors_without_counting_as_execution(scenario_name):
    report = runner.execute_run("manual", scenario_name)
    first = report["observations"]["tool_calls"][0]
    assert first["actual_execution"] is False
    assert first["execution_number"] is None
    assert first["result"]["error"]["retryable"] is False
    assert report["metrics"]["tool_dispatch_attempts"] == 4
    assert report["metrics"]["actual_tool_executions"] == 3


@pytest.mark.parametrize("implementation", ["manual", "langgraph"])
def test_transient_retry_retains_two_observations_but_only_the_final_message(implementation):
    report = runner.execute_run(implementation, "transient")
    attempts = [
        record
        for record in report["observations"]["tool_calls"]
        if record["arguments"] == {"document_id": "D2"}
    ]
    assert len(attempts) == 2
    assert attempts[0]["tool_call_id"] == attempts[1]["tool_call_id"]
    assert attempts[0]["result"]["error"]["retryable"] is True
    assert attempts[1]["result"]["ok"] is True
    final_history = report["observations"]["model_calls"][-1]["messages"]
    returned = [
        message
        for message in final_history
        if message.get("tool_call_id") == attempts[0]["tool_call_id"]
    ]
    assert len(returned) == 1
    assert json.loads(returned[0]["content"])["ok"] is True


@pytest.mark.parametrize("tampering", ["omitted_result", "omitted_assistant", "forged_result"])
def test_host_rejects_omitted_or_forged_history(monkeypatch, tampering):
    def broken(task, model, tools, limits, emit):
        messages = initial_messages(task, limits)
        assistant = model.complete(messages, tools.schemas)
        result = tools.execute(assistant["tool_calls"][0])
        messages += [assistant, result]
        if tampering == "omitted_result":
            messages.pop()
        elif tampering == "omitted_assistant":
            del messages[-2]
        else:
            messages[-1] = {**result, "content": json_text({"ok": True, "data": "伪造内容"})}
        model.complete(messages, tools.schemas)
        raise AssertionError("宿主接受了错误回填")

    monkeypatch.setattr(runner, "load_run", lambda _implementation: broken)
    report = runner.execute_run("manual", "normal")
    assert report["passed"] is False
    assert report["outcome"]["status"] == "error"
    assert report["error"]["type"] == "ProtocolError"
    assert report["metrics"]["model_decisions"] == 1


def test_host_requires_an_actual_result_for_every_tool_call():
    scenario, limits = get_scenario("normal"), RunLimits()
    tools = ToolBroker(scenario, limits, time.monotonic())
    model = ObservedModel(ScriptedModel(scenario), tools, limits)
    messages = initial_messages(scenario.task, limits)
    assistant = model.complete(messages, tools.schemas)

    with pytest.raises(ProtocolError, match="真实结果"):
        model.complete(messages + [assistant], tools.schemas)
    assert len(model.records) == 1
    assert tools.executions == 0


def test_host_rejects_tool_calls_changed_after_the_model_returned_them():
    scenario, limits = get_scenario("normal"), RunLimits()
    tools = ToolBroker(scenario, limits, time.monotonic())
    model = ObservedModel(ScriptedModel(scenario), tools, limits)
    assistant = model.complete(initial_messages(scenario.task, limits), tools.schemas)
    forged = copy.deepcopy(assistant["tool_calls"][0])
    forged["function"]["arguments"] = json_text({"query": "未经模型请求的查询"})

    with pytest.raises(ProtocolError, match="当前模型"):
        tools.execute(forged)
    assert tools.executions == 0
    assert tools.violations == ["unauthorized_dispatch"]


@pytest.mark.parametrize("implementation", ["manual", "langgraph"])
def test_search_titles_do_not_prove_a_cited_fact(monkeypatch, implementation):
    class TitlesOnlyModel:
        metadata = {"mode": "scripted", "model": "title-only-negative-control", "network": False}

        def __init__(self, scenario):
            self.scenario, self.index = scenario, 0

        def complete(self, messages, schemas):
            self.index += 1
            if self.index == 1:
                return {
                    "role": "assistant",
                    "content": None,
                    "tool_calls": [
                        {
                            "id": "search-only",
                            "type": "function",
                            "function": {
                                "name": "search_documents",
                                "arguments": json_text({"query": self.scenario.project}),
                            },
                        }
                    ],
                }
            return {"role": "assistant", "content": json_text(self.scenario.expected_answer)}

    monkeypatch.setattr(runner, "ScriptedModel", TitlesOnlyModel)
    report = runner.execute_run(implementation, "normal")

    assert report["passed"] is False
    assert report["outcome"]["status"] == "completed"
    assert report["observations"]["successful_reads"] == {}
    assert checks(report)["observed_final"] is True
    assert checks(report)["message_history"] is True
    for field in ("start_date", "audience", "allowed_operations"):
        assert checks(report)[f"{field}.value"] is True
        assert checks(report)[f"{field}.sources"] is False
    titles = report["observations"]["tool_calls"][0]["result"]["data"]["documents"]
    assert all(document.keys() == {"document_id", "title"} for document in titles)


def test_retry_after_permanent_failure_cannot_pass_host_checks():
    scenario, limits = get_scenario("missing"), RunLimits()
    tools = ToolBroker(scenario, limits, time.monotonic())
    model = ObservedModel(ScriptedModel(scenario), tools, limits)
    messages = initial_messages(scenario.task, limits)
    search = model.complete(messages, tools.schemas)
    search_result = tools.execute(search["tool_calls"][0])
    read = model.complete(messages + [search, search_result], tools.schemas)
    missing = next(
        call
        for call in read["tool_calls"]
        if json.loads(call["function"]["arguments"])["document_id"] == "D2"
    )
    first = tools.execute(missing)
    with pytest.raises(ProtocolError):
        tools.execute(missing)

    assert json.loads(first["content"])["error"]["code"] == "NOT_FOUND"
    assert tools.executions == 2  # One search and the first missing read.
    assert tools.dispatch_attempts == 3
    assert tools.violations == ["retry_not_allowed"]
    outcome = AgentOutcome("completed", "final_answer", scenario.expected_answer, model.previous)
    result_checks = runner.evaluate(scenario, outcome, model, tools, limits, True)
    assert all(check["passed"] for check in result_checks) is False
    assert (
        next(check for check in result_checks if check["id"] == "authorized_dispatch")["passed"]
        is False
    )


def test_learner_emit_passed_claim_has_no_grading_authority(monkeypatch):
    def claims_success(task, model, tools, limits, emit):
        emit({"phase": "assessment", "passed": True, "mastery": 100})
        return AgentOutcome("completed", "final_answer", get_scenario("normal").expected_answer, [])

    monkeypatch.setattr(runner, "load_run", lambda _implementation: claims_success)
    report = runner.execute_run("manual", "normal")

    assert report["learner_events"][0]["passed"] is True
    assert report["passed"] is False
    assert checks(report)["observed_final"] is False
    assert report["learning"]["mastery"] is None
    assert report["metrics"]["model_decisions"] == 0
    assert report["metrics"]["actual_tool_executions"] == 0


@pytest.mark.parametrize("implementation", ["manual", "langgraph"])
@pytest.mark.parametrize(
    "limits,reason,counts",
    [
        (RunLimits(max_model_decisions=1), "model_budget", (1, 1)),
        (RunLimits(max_tool_executions=1), "tool_budget", (2, 1)),
    ],
)
def test_lower_budgets_stop_without_fabricating_a_completed_answer(
    implementation, limits, reason, counts
):
    report = runner.execute_run(implementation, "normal", limits=limits)
    assert report["passed"] is False  # The normal task was not completed.
    assert report["outcome"]["status"] == "budget_exhausted"
    assert report["outcome"]["stop_reason"] == reason
    assert report["outcome"]["answer"] is None
    assert (
        report["metrics"]["model_decisions"],
        report["metrics"]["actual_tool_executions"],
    ) == counts
    assert checks(report)["model_limit"] is True
    assert checks(report)["tool_limit"] is True
