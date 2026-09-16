"""Both orchestrators must preserve the same observable agent protocol."""

import importlib.util
import json
from copy import deepcopy
from pathlib import Path

import pytest

from ngy_lab.contracts import BudgetExceeded, LabError, ProtocolError, RunLimits
from ngy_lab.implementations import common, langgraph_agent, manual


def answer(value=None, sources=None):
    return {
        name: {"value": value, "source_ids": list(sources or [])}
        for name in ("start_date", "audience", "allowed_operations")
    }


def final(value=None, sources=None):
    return {"role": "assistant", "content": json.dumps(answer(value, sources))}


def call(call_id="call-1", name="read_document", arguments=None):
    return {
        "id": call_id,
        "type": "function",
        "function": {"name": name, "arguments": json.dumps(arguments or {"document_id": "doc-1"})},
    }


def assistant(*calls):
    return {"role": "assistant", "content": "需要先读取原文。", "tool_calls": list(calls)}


def success(document_id="doc-1"):
    return {"ok": True, "data": {"document_id": document_id, "content": "课程资料"}}


def failure(retryable):
    return {
        "ok": False,
        "error": {"code": "unavailable", "message": "读取失败", "retryable": retryable},
    }


class FakeModel:
    metadata = {"mode": "test"}

    def __init__(self, replies):
        self.replies = list(replies)
        self.histories = []

    def complete(self, messages, schemas):
        self.histories.append(deepcopy(messages))
        assert schemas == FakeTools.schemas
        reply = self.replies.pop(0)
        if isinstance(reply, Exception):
            raise reply
        return deepcopy(reply)


class FakeTools:
    schemas = [{"type": "function", "function": {"name": "read_document"}}]

    def __init__(self, replies, max_executions=None):
        self.replies = list(replies)
        self.calls = []
        self.max_executions = max_executions

    def execute(self, tool_call):
        if self.max_executions is not None and len(self.calls) >= self.max_executions:
            raise BudgetExceeded("tool_budget")
        self.calls.append(deepcopy(tool_call))
        reply = self.replies.pop(0)
        if isinstance(reply, Exception):
            raise reply
        return {
            "role": "tool",
            "tool_call_id": tool_call["id"],
            "content": json.dumps(reply),
        }


@pytest.fixture(params=[manual.run, langgraph_agent.run], ids=["manual", "langgraph"])
def run_agent(request):
    return request.param


def test_preserves_assistant_content_and_every_tool_result_before_next_decision(run_agent):
    first = assistant(call("first"), call("second", arguments={"document_id": "doc-2"}))
    model = FakeModel([first, final("已读取", ["doc-1", "doc-2"])])
    tools = FakeTools([success(), success("doc-2")])
    events = []

    result = run_agent("根据实际资料回答", model, tools, RunLimits(), events.append)

    assert (result.status, result.stop_reason) == ("completed", "final_answer")
    assert result.answer == answer("已读取", ["doc-1", "doc-2"])
    second_history = model.histories[1]
    assert [message["role"] for message in second_history] == [
        "system",
        "user",
        "assistant",
        "tool",
        "tool",
    ]
    assert second_history[2] == first
    assert [message["tool_call_id"] for message in second_history[3:]] == ["first", "second"]
    assert result.messages[:-1] == second_history
    assert [event["phase"] for event in events if event["phase"] != "graph_node"] == [
        "model",
        "tool",
        "tool",
        "model",
    ]


def test_retryable_failure_executes_the_identical_call_once_more(run_agent):
    original_call = call()
    model = FakeModel([assistant(original_call), final("已读取", ["doc-1"])])
    tools = FakeTools([failure(True), success()])
    events = []

    result = run_agent("读取", model, tools, RunLimits(), events.append)

    assert result.status == "completed"
    assert tools.calls == [original_call, original_call]
    history = model.histories[1]
    assert len([message for message in history if message["role"] == "tool"]) == 1
    assert json.loads(history[-1]["content"])["ok"] is True
    assert [event["retry"] for event in events if event["phase"] == "tool"] == [False, True]


@pytest.mark.parametrize("retryable", [False, "true", 1, None])
def test_only_literal_true_permits_a_retry(run_agent, retryable):
    model = FakeModel([assistant(call()), final()])
    tools = FakeTools([failure(retryable)])

    result = run_agent("未知资料", model, tools, RunLimits(), lambda _event: None)

    assert result.status == "completed"
    assert len(tools.calls) == 1
    assert result.answer == answer()
    assert json.loads(model.histories[1][-1]["content"])["ok"] is False


def test_second_retryable_failure_is_returned_to_the_model_without_third_execution(run_agent):
    model = FakeModel([assistant(call()), final()])
    tools = FakeTools([failure(True), failure(True)])

    result = run_agent("读取失败", model, tools, RunLimits(), lambda _event: None)

    assert result.status == "completed"
    assert len(tools.calls) == 2
    assert len([message for message in result.messages if message["role"] == "tool"]) == 1


def test_model_budget_stops_before_an_extra_decision(run_agent):
    model = FakeModel([assistant(call())])
    tools = FakeTools([success()])

    result = run_agent(
        "继续检索", model, tools, RunLimits(max_model_decisions=1), lambda _event: None
    )

    assert (result.status, result.stop_reason, result.answer) == (
        "budget_exhausted",
        "model_budget",
        None,
    )
    assert len(model.histories) == 1
    assert len(tools.calls) == 1


def test_retry_also_consumes_the_tool_budget(run_agent):
    model = FakeModel([assistant(call())])
    tools = FakeTools([failure(True)], max_executions=1)

    result = run_agent(
        "临时失败", model, tools, RunLimits(max_tool_executions=1), lambda _event: None
    )

    assert (result.status, result.stop_reason, result.answer) == (
        "budget_exhausted",
        "tool_budget",
        None,
    )
    assert len(tools.calls) == 1
    assert len(model.histories) == 1


def test_rejected_requests_do_not_spend_an_implementation_side_tool_budget(run_agent):
    model = FakeModel([assistant(call(f"invalid-{index}")) for index in range(8)])
    tools = FakeTools([failure(False) for _index in range(8)])

    result = run_agent(
        "无效请求仍受模型预算限制",
        model,
        tools,
        RunLimits(max_tool_executions=1),
        lambda _event: None,
    )

    assert (result.status, result.stop_reason) == ("budget_exhausted", "model_budget")
    assert len(model.histories) == 8
    assert len(tools.calls) == 8


@pytest.mark.parametrize("reason", ["model_budget", "tool_budget", "deadline"])
def test_host_budget_reason_is_preserved(run_agent, reason):
    model = FakeModel([BudgetExceeded(reason)])
    result = run_agent("运行", model, FakeTools([]), RunLimits(), lambda _event: None)
    assert (result.status, result.stop_reason, result.answer) == ("budget_exhausted", reason, None)


def test_expired_run_cannot_commit_a_late_final_answer(run_agent, monkeypatch):
    clock = [0.0]
    monkeypatch.setattr(common.time, "monotonic", lambda: clock[0])

    class LateModel(FakeModel):
        def complete(self, messages, schemas):
            reply = super().complete(messages, schemas)
            clock[0] = 2.0
            return reply

    result = run_agent(
        "运行",
        LateModel([final()]),
        FakeTools([]),
        RunLimits(timeout_seconds=1),
        lambda _event: None,
    )
    assert (result.status, result.stop_reason, result.answer) == (
        "budget_exhausted",
        "deadline",
        None,
    )


@pytest.mark.parametrize(
    "bad_answer",
    [
        "not json",
        "```json\n{}\n```",
        json.dumps({**answer(), "extra": "unsupported"}),
        json.dumps({**answer(), "audience": {"value": [], "source_ids": []}}),
        json.dumps({**answer(), "audience": {"value": None, "source_ids": ["doc-1"]}}),
        json.dumps({**answer(), "audience": {"value": "known", "source_ids": "doc-1"}}),
        json.dumps({**answer(), "audience": {"value": "known", "source_ids": ["", "doc-1"]}}),
    ],
)
def test_final_answer_requires_the_strict_contract(run_agent, bad_answer):
    model = FakeModel([{"role": "assistant", "content": bad_answer}])
    with pytest.raises(ProtocolError):
        run_agent("运行", model, FakeTools([]), RunLimits(), lambda _event: None)


@pytest.mark.parametrize(
    "content",
    [
        json.dumps(answer()).replace(
            '{"start_date":', '{"audience": {"value": null, "source_ids": []}, "start_date":', 1
        ),
        json.dumps(answer()).replace('"value": null', '"value": "hidden", "value": null', 1),
        json.dumps({**answer(), "audience": {"value": "\ud800", "source_ids": ["D1"]}}),
        json.dumps({**answer(), "audience": {"value": "known", "source_ids": ["\udfff"]}}),
        json.dumps(
            {**answer(), "audience": {"value": "\ud800", "source_ids": ["D1"]}},
            ensure_ascii=False,
        ),
        *[
            json.dumps(answer()).replace('"value": null', f'"value": {token}', 1)
            for token in ("NaN", "Infinity", "-Infinity", "1e999")
        ],
    ],
)
def test_final_answer_rejects_ambiguous_or_non_unicode_json(run_agent, content):
    model = FakeModel([{"role": "assistant", "content": content}])
    with pytest.raises(ProtocolError):
        run_agent("解析最终回答", model, FakeTools([]), RunLimits(), lambda _event: None)


@pytest.mark.parametrize("value", ["🌱", "NaN 和 Infinity 在此处只是文字"])
def test_final_answer_keeps_valid_unicode_and_string_literals(run_agent, value):
    model = FakeModel([final(value, ["D1"])])
    result = run_agent("解析最终回答", model, FakeTools([]), RunLimits(), lambda _event: None)
    assert result.status == "completed"
    assert result.answer == answer(value, ["D1"])


@pytest.mark.parametrize("where", ["model", "tool"])
def test_unexpected_lab_error_is_not_swallowed_as_an_answer(run_agent, where):
    error = LabError("可操作错误")
    model = FakeModel([error] if where == "model" else [assistant(call())])
    tools = FakeTools([] if where == "model" else [error])
    with pytest.raises(LabError, match="可操作错误"):
        run_agent("运行", model, tools, RunLimits(), lambda _event: None)


def test_malformed_arguments_reach_the_tool_boundary_for_a_validation_result(run_agent):
    malformed = call()
    malformed["function"]["arguments"] = "{invalid json"
    model = FakeModel([assistant(malformed), final()])
    tools = FakeTools([failure(False)])
    result = run_agent("运行", model, tools, RunLimits(), lambda _event: None)
    assert result.status == "completed"
    assert tools.calls == [malformed]


@pytest.mark.parametrize("name", ["manual", "langgraph_agent"])
def test_unfinished_starters_are_explicitly_incomplete_and_do_not_call_brokers(name):
    path = Path(__file__).parents[1] / "starters" / f"{name}.py"
    spec = importlib.util.spec_from_file_location(f"starter_{name}", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    model, tools = FakeModel([]), FakeTools([])
    result = module.run("练习", model, tools, RunLimits(), lambda _event: None)
    assert (result.status, result.stop_reason, result.answer) == (
        "incomplete",
        "starter_todo",
        None,
    )
    assert model.histories == []
    assert tools.calls == []
