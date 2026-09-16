"""Real Windows desktop isolation integration; no user books or real model service."""

import sys
import threading
import time

import pytest

from ngy_lab.desktop_compat import COMPATIBILITY_ID
from ngy_lab.sandbox import SandboxLimits, SandboxStop, _write_packet, execute_desktop_run

pytestmark = pytest.mark.skipif(sys.platform != "win32", reason="Windows LPAC integration")


def execute(
    tmp_path,
    *,
    code=None,
    implementation="manual",
    scenario="normal",
    resources=None,
    limits=None,
    cancel=None,
    progress=None,
):
    request = {
        "run_id": "pytest-isolation",
        "run_dir": str(tmp_path),
        "implementation": implementation,
        "scenario": scenario,
        "mode": "scripted",
    }
    if code is not None:
        request["code"] = code
    if resources is not None:
        request["resources"] = resources
    if limits is not None:
        request["limits"] = limits
    return execute_desktop_run(request, cancel=cancel, progress=progress)


@pytest.mark.parametrize("implementation", ["manual", "langgraph"])
@pytest.mark.parametrize(
    "scenario,decisions,executions",
    [
        ("normal", 3, 3),
        ("invalid_call", 4, 3),
        ("invalid_arguments", 4, 3),
        ("transient", 3, 4),
        ("missing", 3, 3),
        ("transfer", 3, 4),
        ("model_budget", 8, 0),
        ("tool_budget", 7, 6),
    ],
)
def test_reference_really_runs_across_isolated_rpc_and_host_grades(
    tmp_path, implementation, scenario, decisions, executions
):
    report = execute(tmp_path, implementation=implementation, scenario=scenario)
    assert report["isolation"]["verified"], report["error"]
    assert report["passed"], (report["error"], report["checks"], report["console_output"])
    assert report["metrics"]["model_decisions"] == decisions
    assert report["metrics"]["actual_tool_executions"] == executions
    if scenario in {"model_budget", "tool_budget"}:
        assert report["outcome"]["status"] == "budget_exhausted"
        assert report["outcome"]["stop_reason"] == scenario
    assert not (tmp_path / "sandbox-profile.json").exists()
    assert not (tmp_path / "stage" / "app" / "ngy_lab" / "scenarios.py").exists()
    assert not (tmp_path / "stage" / "app" / "ngy_lab" / "runner.py").exists()
    assert report["learning"]["mastery"] is None
    assert report["isolation"]["compatibility"] == COMPATIBILITY_ID
    assert "ngy_lab/desktop_compat.py" in report["code_snapshot"]


def test_worker_cannot_read_or_modify_ungranted_host_fixture(tmp_path):
    secret = tmp_path / "host-only.txt"
    secret.write_text("private-fixture-value", encoding="utf-8")
    code = f"""
from pathlib import Path
from ngy_lab.contracts import AgentOutcome
def run(task, model, tools, limits, emit):
    probe = {{}}
    assert Path(__file__).read_text("utf-8")
    for action in ("read", "write"):
        try:
            if action == "read":
                Path({str(secret)!r}).read_text("utf-8")
            else:
                Path({str(secret)!r}).write_text("changed", encoding="utf-8")
            probe[action] = "allowed"
        except PermissionError:
            probe[action] = "denied"
    emit({{"phase": "attack_fixture", "probe": probe}})
    return AgentOutcome("incomplete", "fixture_done", None, [])
"""
    report = execute(tmp_path, code=code)
    assert report["isolation"]["verified"], report["error"]
    assert report["learner_events"] == [
        {"phase": "attack_fixture", "probe": {"read": "denied", "write": "denied"}}
    ], report
    assert secret.read_text("utf-8") == "private-fixture-value"


def test_asyncio_import_succeeds_while_real_iocp_and_network_stay_denied(tmp_path):
    code = """
import asyncio
import importlib
import socket
from ngy_lab.contracts import AgentOutcome
def run(task, model, tools, limits, emit):
    results = {"asyncio_import": bool(asyncio.Future)}
    for attempt in range(2):
        extension = importlib.import_module("_overlapped")
        try:
            extension.CreateIoCompletionPort
            results[f"iocp_{attempt}"] = "ALLOWED"
        except PermissionError as error:
            results[f"iocp_{attempt}"] = error.winerror
    try:
        with socket.socket() as connection:
            connection.connect(("127.0.0.1", 9))
        results["network"] = "ALLOWED"
    except PermissionError as error:
        results["network"] = error.winerror
    emit(results)
    return AgentOutcome("incomplete", "fixture_done", None, [])
"""
    report = execute(tmp_path, code=code)
    assert report["isolation"]["verified"], report["error"]
    assert report["error"] is None, report["error"]
    assert report["learner_events"] == [
        {"asyncio_import": True, "iocp_0": 10013, "iocp_1": 10013, "network": 10013}
    ]


def test_forged_worker_pass_is_rejected_by_external_grader(tmp_path):
    code = """
from ngy_lab.contracts import AgentOutcome
def run(task, model, tools, limits, emit):
    emit({"passed": True, "phase": "assessment"})
    return AgentOutcome("completed", "final_answer", {
        "start_date": {"value": "2026-10-12", "source_ids": ["D1"]},
        "audience": {"value": "内部员工", "source_ids": ["D2"]},
        "allowed_operations": {"value": "资料检索与阅读", "source_ids": ["D2"]},
    }, [])
"""
    report = execute(tmp_path, code=code)
    assert report["isolation"]["verified"], report["error"]
    assert report["passed"] is False
    assert report["metrics"]["model_decisions"] == 0
    assert report["metrics"]["actual_tool_executions"] == 0
    assert report["learner_events"][0]["passed"] is True
    assert not next(check for check in report["checks"] if check["id"] == "observed_final")[
        "passed"
    ]


def test_stdout_flood_is_bounded_and_terminates_worker(tmp_path):
    report = execute(
        tmp_path,
        code='def run(*args):\n    while True: print("x" * 4096)\n',
        resources={"output_bytes": 8192},
    )
    assert report["isolation"]["verified"], report["error"]
    assert report["outcome"]["stop_reason"] == "output_limit", report["error"]
    assert sum(len(item["text"]) for item in report["console_output"]) <= 8192
    assert not (tmp_path / "sandbox-profile.json").exists()


def test_worker_cannot_import_host_grader_or_dispatch_arbitrary_rpc(tmp_path):
    code = """
import json
import sys
from ngy_lab.contracts import AgentOutcome
def run(task, model, tools, limits, emit):
    try:
        import ngy_lab.scenarios
        emit({"host_module": "visible"})
    except ModuleNotFoundError:
        emit({"host_module": "absent"})
    sys.__stdout__.write(json.dumps({"op": "read_host_file", "path": "unauthorized"}) + "\\n")
    sys.__stdout__.flush()
    return AgentOutcome("completed", "final_answer", {}, [])
"""
    report = execute(tmp_path, code=code)
    assert report["isolation"]["verified"], report["error"]
    assert report["learner_events"] == [{"host_module": "absent"}]
    assert report["passed"] is False
    assert report["outcome"]["status"] == "error"
    assert report["error"]["type"] == "ProtocolError"
    assert report["metrics"]["model_decisions"] == 0


def test_explicit_cancel_stops_busy_worker_and_cleans_profile(tmp_path):
    cancel = threading.Event()
    cancellation_time = []
    timer = None

    def progress(label, detail):
        nonlocal timer
        if label == "启动隔离进程":

            def request_cancel():
                cancellation_time.append(time.monotonic())
                cancel.set()

            timer = threading.Timer(2, request_cancel)
            timer.start()

    try:
        report = execute(
            tmp_path,
            code="def run(*args):\n    while True: pass\n",
            cancel=cancel,
            progress=progress,
            resources={"cpu_time_seconds": 30},
        )
    finally:
        if timer:
            timer.cancel()
    assert report["isolation"]["verified"], report["error"]
    assert report["outcome"]["status"] == "cancelled", report["error"]
    assert cancellation_time and time.monotonic() - cancellation_time[0] < 3
    assert not (tmp_path / "sandbox-profile.json").exists()


@pytest.mark.parametrize(
    "field,value",
    [
        ("memory_bytes", 1024),
        ("cpu_time_seconds", float("nan")),
        ("cpu_rate_percent", 0),
        ("output_bytes", 0),
        ("max_rpc_messages", 0),
    ],
)
def test_resource_policy_cannot_be_disabled(field, value):
    with pytest.raises(ValueError):
        SandboxLimits(**{field: value})


def test_live_mode_is_rejected_before_creating_a_worker(tmp_path):
    with pytest.raises(ValueError, match="scripted"):
        execute_desktop_run({"run_id": "invalid", "run_dir": str(tmp_path), "mode": "live"})
    assert list(tmp_path.iterdir()) == []


@pytest.mark.parametrize("stop", ["cancelled", "deadline"])
def test_host_response_write_remains_cancellable_when_worker_stops_reading(stop):
    release = threading.Event()
    writing = threading.Event()
    finished = threading.Event()
    cancel = threading.Event()

    class BlockedPipe:
        def write(self, value):
            writing.set()
            release.wait()
            finished.set()

        def flush(self):
            pass

    class Process:
        stdin = BlockedPipe()

        def poll(self):
            return None

    timer = threading.Timer(0.05, cancel.set) if stop == "cancelled" else None
    if timer:
        timer.start()
    started = time.monotonic()
    try:
        with pytest.raises(SandboxStop) as error:
            _write_packet(
                Process(),
                {"fixture": "response"},
                cancel,
                started,
                5 if stop == "cancelled" else 0.05,
            )
        assert error.value.reason == stop
        assert writing.is_set()
        assert time.monotonic() - started < 1
    finally:
        release.set()
        if timer:
            timer.cancel()
        assert finished.wait(1)
