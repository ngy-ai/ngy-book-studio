"""Real LPAC acceptance for all nine chapters; no mocks replace isolation here."""

import json
import shutil
import sys
import threading
from pathlib import Path

import pytest

from moye_lab.sandbox import execute_desktop_run

ROOT = Path(__file__).resolve().parents[1]
pytestmark = pytest.mark.skipif(sys.platform != "win32", reason="Windows LPAC integration")


@pytest.fixture
def run_dir(tmp_path):
    yield tmp_path
    # These tests create many isolated runtime copies. Remove only this test's
    # checked stage subtree, after the host has terminated its Job/profile.
    stage = tmp_path / "stage"
    assert stage.resolve().parent == tmp_path.resolve()
    if stage.exists():
        shutil.rmtree(stage)


def execute(run_dir, chapter, implementation="manual", scenario="normal", **kwargs):
    request = {
        "chapter": chapter,
        "run_id": "chapter-lpac-test",
        "run_dir": str(run_dir),
        "implementation": implementation,
        "scenario": scenario,
    }
    request.update(kwargs.pop("request", {}))
    report = execute_desktop_run(request, **kwargs)
    assert report["request"]["chapter"] == chapter
    assert report["request"]["course_id"] == f"agent-foundations.chapter-{chapter:02d}"
    assert report["request"]["task_id"] == scenario
    assert report["request"]["rules_version"] == "1.0.0"
    assert report["request"]["task_version"] == "1.0.0"
    assert not (run_dir / "sandbox-profile.json").exists()
    return report


@pytest.mark.parametrize("chapter", range(2, 11))
@pytest.mark.parametrize("scenario", ["normal", "fault", "transfer"])
@pytest.mark.parametrize("implementation", ["manual", "langgraph"])
def test_all_chapter_references_in_real_lpac(run_dir, chapter, scenario, implementation):
    report = execute(run_dir, chapter, implementation, scenario)
    assert report["isolation"]["verified"], report["error"]
    assert report["passed"], (report["error"], report["checks"], report["console_output"])
    assert report["metrics"]["model_decisions"] == 1
    assert report["metrics"]["actual_tool_executions"] <= 5
    expected_steps = (
        (1 + report["metrics"]["actual_tool_executions"] if chapter == 5 else 2)
        if implementation == "langgraph"
        else 0
    )
    assert report["metrics"]["framework_steps"] == expected_steps
    source = (ROOT / "references" / f"ch{chapter:02d}_{implementation}.py").read_text("utf-8")
    assert report["code_snapshot"][f"desktop_submission/{implementation}.py"] == source
    package = run_dir / "stage" / "app" / "moye_lab"
    assert (package / "chapter_support.py").exists()
    for host_file in ["chapter_runtime.py", "scenarios.py", "runner.py", "runtime.py"]:
        assert not (package / host_file).exists()
    assert not (run_dir / "stage" / "app" / "references").exists()
    assert report["learning"]["mastery"] is None


@pytest.mark.parametrize("implementation", ["manual", "langgraph"])
def test_unfinished_starter_cannot_pass_in_lpac(run_dir, implementation):
    code = (ROOT / "starters" / f"ch06_{implementation}.py").read_text("utf-8")
    report = execute(run_dir, 6, implementation, request={"code": code})
    assert report["isolation"]["verified"]
    assert not report["passed"]
    assert report["outcome"]["status"] == "error"
    assert "NotImplementedError" in report["error"]["message"]


@pytest.mark.parametrize("chapter", range(2, 11))
def test_student_success_declaration_cannot_forge_host_report(run_dir, chapter):
    code = """
from moye_lab.chapter_support import load_case, finish
def run(task, model, tools, limits, emit):
    state = load_case(task, model, tools)
    emit({"passed": True, "tests_passed": 999})
    print("ALL TESTS PASSED")
    return finish(state, {"passed": True})
"""
    report = execute(run_dir, chapter, request={"code": code})
    assert report["isolation"]["verified"]
    assert not report["passed"]
    assert report["learner_events"] == [{"passed": True, "tests_passed": 999}]
    assert not next(check for check in report["checks"] if check["id"] == "chapter_result")[
        "passed"
    ]


def test_old_answer_cannot_be_replayed_with_new_case_id(run_dir):
    old_dir = run_dir / "first"
    old_dir.mkdir()
    previous = execute(old_dir, 3)
    assert previous["passed"]
    result = json.dumps(previous["outcome"]["answer"]["result"], ensure_ascii=True)
    code = f"""
import json
from moye_lab.chapter_support import load_case, finish
def run(task, model, tools, limits, emit):
    state = load_case(task, model, tools)
    return finish(state, json.loads({result!r}))
"""
    report = execute(run_dir, 3, scenario="transfer", request={"code": code})
    assert report["isolation"]["verified"]
    assert not report["passed"]
    assert report["metrics"]["actual_tool_executions"] == 0
    # first/ was created exclusively in this test; verify before recursive cleanup.
    assert old_dir.resolve().parent == run_dir.resolve()
    shutil.rmtree(old_dir)


def test_worker_cannot_import_host_or_reference_answer(run_dir):
    code = """
import importlib
from moye_lab.chapter_support import load_case, finish
def run(task, model, tools, limits, emit):
    state = load_case(task, model, tools)
    for name in ["moye_lab.chapter_runtime", "references.ch02_manual", "tutorial_examples.ch02_tools"]:
        try:
            importlib.import_module(name)
        except ImportError:
            emit({"module": name, "import": "denied"})
        else:
            emit({"module": name, "import": "ALLOWED"})
    return finish(state, {})
"""
    report = execute(run_dir, 2, request={"code": code})
    assert report["isolation"]["verified"]
    assert [event["import"] for event in report["learner_events"]] == ["denied"] * 3
    assert not report["passed"]


def test_caught_unauthorized_call_is_still_terminal_and_zero_execution(run_dir):
    code = """
from moye_lab.chapter_support import load_case, call_tool, finish
def run(task, model, tools, limits, emit):
    state = load_case(task, model, tools)
    try:
        call_tool(state, tools, "read_document", {"document_id": "outside-scope"})
    except Exception:
        pass
    return finish(state, {"passed": True})
"""
    report = execute(run_dir, 3, request={"code": code})
    assert report["isolation"]["verified"]
    assert not report["passed"]
    assert report["outcome"]["status"] == "error"
    assert report["metrics"]["actual_tool_executions"] == 0
    assert report["observations"]["policy_violations"]


def test_new_chapter_tool_budget_ends_explicitly(run_dir):
    report = execute(run_dir, 10, request={"limits": {"max_tool_executions": 1}})
    assert report["isolation"]["verified"]
    assert report["outcome"]["status"] == "budget_exhausted"
    assert report["outcome"]["stop_reason"] == "tool_executions"
    assert report["metrics"]["actual_tool_executions"] == 1
    assert not report["passed"]


def test_cancelled_new_chapter_retains_identity_and_kills_worker(run_dir):
    cancel = threading.Event()
    timer = None

    def progress(label, _detail):
        nonlocal timer
        if label == "模型决策" and timer is None:
            timer = threading.Timer(0.1, cancel.set)
            timer.start()

    code = """
from moye_lab.chapter_support import load_case
def run(task, model, tools, limits, emit):
    load_case(task, model, tools)
    while True:
        pass
"""
    try:
        report = execute(run_dir, 8, request={"code": code}, cancel=cancel, progress=progress)
    finally:
        if timer is not None:
            timer.cancel()
    assert report["isolation"]["verified"]
    assert report["outcome"]["status"] == "cancelled"
    assert report["outcome"]["stop_reason"] == "cancelled"
    assert not report["passed"]
