"""Regressions for incomplete budget histories and ordinary learner-code mistakes."""

import sys

import pytest

from moye_lab import runner
from moye_lab.implementations import manual


@pytest.mark.parametrize("scenario", ["model_budget", "tool_budget"])
@pytest.mark.parametrize("tampering", ["empty", "drop_last", "forged_last"])
def test_budget_outcome_cannot_pass_with_missing_or_forged_history(
    monkeypatch, scenario, tampering
):
    def altered_outcome(task, model, tools, limits, emit):
        outcome = manual.run(task, model, tools, limits, emit)
        if tampering == "empty":
            outcome.messages = []
        elif tampering == "drop_last":
            outcome.messages.pop()
        else:
            outcome.messages[-1] = {"role": "assistant", "content": "伪造停止记录"}
        return outcome

    monkeypatch.setattr(runner, "load_run", lambda _implementation: altered_outcome)
    report = runner.execute_run("manual", scenario)
    checks = {check["id"]: check["passed"] for check in report["checks"]}
    assert report["passed"] is False
    assert checks["explicit_stop"] is True
    assert checks["message_history"] is False


@pytest.mark.parametrize("event", ["thinking", [], None, 1])
def test_invalid_instrumentation_returns_an_actionable_error_report(monkeypatch, event):
    def invalid_emit(task, model, tools, limits, emit):
        emit(event)
        return manual.run(task, model, tools, limits, emit)

    monkeypatch.setattr(runner, "load_run", lambda _implementation: invalid_emit)
    report = runner.execute_run("manual", "normal")
    assert report["passed"] is False
    assert report["outcome"]["status"] == "error"
    assert report["error"]["type"] == "ProtocolError"
    assert "emit" in report["error"]["message"]
    assert any(
        frame["file"] == "moye_lab/runner.py" and frame["function"] == "emit"
        for frame in report["error"]["frames"]
    )
    assert report["metrics"]["model_decisions"] == 0
    assert report["learner_events"] == []


def test_workspace_loader_supports_dataclasses_with_future_annotations(tmp_path, monkeypatch):
    workspace = tmp_path / "workspaces" / "student"
    workspace.mkdir(parents=True)
    source = workspace / "manual.py"
    source.write_text(
        "from __future__ import annotations\n"
        "from dataclasses import dataclass\n"
        "@dataclass\n"
        "class State:\n"
        "    count: int = 3\n"
        "def run(*args):\n"
        "    return State().count\n",
        encoding="utf-8",
    )
    monkeypatch.setattr(runner, "COURSE_ROOT", tmp_path)
    run = runner.load_run("workspaces/student/manual.py")
    try:
        assert run() == 3
        assert sys.modules[run.__module__].State.__dataclass_fields__["count"].default == 3
    finally:
        sys.modules.pop(run.__module__, None)


def test_workspace_loader_removes_a_module_that_failed_during_import(tmp_path, monkeypatch):
    workspace = tmp_path / "workspaces" / "student"
    workspace.mkdir(parents=True)
    (workspace / "manual.py").write_text('raise RuntimeError("broken import")\n', encoding="utf-8")
    monkeypatch.setattr(runner, "COURSE_ROOT", tmp_path)
    before = {name for name in sys.modules if name.startswith("course_submission_")}

    with pytest.raises(RuntimeError, match="broken import"):
        runner.load_run("workspaces/student/manual.py")

    after = {name for name in sys.modules if name.startswith("course_submission_")}
    assert after == before
