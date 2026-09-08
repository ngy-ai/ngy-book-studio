"""Exercise the exact JSONL host command used by the desktop application."""

import json
import subprocess
import sys

import pytest

from moye_lab.runner import COURSE_ROOT

pytestmark = pytest.mark.skipif(sys.platform != "win32", reason="Windows desktop host")


def command():
    return [sys.executable, "-I", "-u", str(COURSE_ROOT / "moye_lab" / "desktop_host.py")]


def test_closing_desktop_control_pipe_cancels_the_attempt(tmp_path):
    request = {
        "command": "run",
        "run_id": "desktop-eof-fixture",
        "implementation": "manual",
        "scenario": "normal",
        "mode": "scripted",
        "run_dir": str(tmp_path),
    }
    completed = subprocess.run(
        command(),
        input=json.dumps(request).encode("ascii") + b"\n",
        capture_output=True,
        timeout=15,
        creationflags=subprocess.CREATE_NO_WINDOW,
    )
    assert completed.returncode == 1, completed.stderr
    events = [json.loads(line) for line in completed.stdout.splitlines()]
    assert events[0]["event"] == "started"
    assert events[-1]["event"] == "finished"
    assert all(event["run_id"] == request["run_id"] for event in events)
    assert events[-1]["report"]["outcome"]["status"] == "cancelled"
    assert events[-1]["report"]["passed"] is False
    assert not (tmp_path / "sandbox-profile.json").exists()


@pytest.mark.parametrize("payload", [b"", b"[]\n", b'{"command":"cancel"}\n'])
def test_invalid_initial_control_message_returns_structured_failure(payload):
    completed = subprocess.run(
        command(),
        input=payload,
        capture_output=True,
        timeout=10,
        creationflags=subprocess.CREATE_NO_WINDOW,
    )
    assert completed.returncode == 2
    event = json.loads(completed.stdout)
    assert event["event"] == "finished"
    assert event["report"] is None
    assert event["error"] == "无效运行请求"
