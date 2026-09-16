"""Exercise the learner's actual CLI path in a copied, temporary course directory."""

import hashlib
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from ngy_lab.runner import COURSE_ROOT
from ngy_lab.runtime import json_text


@pytest.fixture
def course(tmp_path):
    for directory in ("ngy_lab", "starters", "worksheets"):
        shutil.copytree(
            COURSE_ROOT / directory,
            tmp_path / directory,
            ignore=shutil.ignore_patterns("__pycache__"),
        )
    for filename in ("pyproject.toml", "uv.lock"):
        shutil.copyfile(COURSE_ROOT / filename, tmp_path / filename)
    return tmp_path


def cli(course: Path, *arguments: str):
    environment = dict(os.environ, PYTHONUTF8="1", PYTHONDONTWRITEBYTECODE="1")
    return subprocess.run(
        [sys.executable, "-B", "-m", "ngy_lab", *arguments],
        cwd=course,
        env=environment,
        text=True,
        encoding="utf-8",
        capture_output=True,
        timeout=45,
    )


def test_pair_produces_trace_and_verifiable_code_snapshots(course):
    result = cli(course, "compare", "--scenario", "transient")
    assert result.returncode == 0, result.stdout + result.stderr
    paths = list((course / "runs").glob("*/report.json"))
    assert len(paths) == 2
    for path in paths:
        report = json.loads(path.read_text(encoding="utf-8"))
        assert report["passed"]
        assert report["metrics"]["model_decisions"] == 3
        assert report["metrics"]["actual_tool_executions"] == 4
        assert report["learning"]["status"] == "pending_review"
        assert report["learning"]["mastery"] is None
        assert report["request"]["course_version"]
        assert report["request"]["task_version"]
        assert report["request"]["rules_version"]
        snapshot_dir = path.parent / "code"
        snapshot = {
            p.relative_to(snapshot_dir).as_posix(): p.read_bytes().decode("utf-8")
            for p in snapshot_dir.rglob("*")
            if p.is_file()
        }
        digest = hashlib.sha256(json_text(snapshot).encode("utf-8")).hexdigest()
        assert report["request"]["code_sha256"] == digest
        assert "uv.lock" in snapshot
        assert "ngy_lab/implementations/common.py" in snapshot
        assert "ngy_lab/scenarios.py" in snapshot
        timeout = [
            r
            for r in report["observations"]["tool_calls"]
            if r["result"].get("error", {}).get("code") == "TIMEOUT"
        ]
        assert len(timeout) == 1
    comparison = json.loads(next((course / "runs").glob("comparison-*.json")).read_text("utf-8"))
    assert comparison[0]["same_answer"] is True
    assert comparison[0]["metric_difference_langgraph_minus_manual"]["model_decisions"] == 0
    assert comparison[0]["metric_difference_langgraph_minus_manual"]["framework_steps"] == 5


def test_new_learner_can_create_attempt_and_preserve_first_work(course):
    result = cli(course, "new-workspace", "first-agent")
    assert result.returncode == 0, result.stderr
    workspace = course / "workspaces" / "first-agent"
    assert (workspace / "learning-record.md").is_file()
    assert (workspace / "langgraph_agent.py").is_file()
    original = (workspace / "manual.py").read_bytes()
    duplicate = cli(course, "new-workspace", "first-agent")
    assert duplicate.returncode == 2
    assert (workspace / "manual.py").read_bytes() == original
    attempt = cli(course, "run", "--implementation", "workspaces/first-agent/manual.py")
    assert attempt.returncode == 1, attempt.stdout + attempt.stderr
    report = json.loads(next((course / "runs").glob("*/report.json")).read_text("utf-8"))
    assert report["outcome"]["status"] == "incomplete"
    assert report["passed"] is False
    assert report["metrics"]["model_decisions"] == 0
    assert "workspaces/first-agent/manual.py" in {
        p.relative_to(next((course / "runs").glob("*/code"))).as_posix()
        for p in next((course / "runs").glob("*/code")).rglob("*.py")
    }


def test_custom_code_path_is_actually_executed(course):
    assert cli(course, "new-workspace", "custom").returncode == 0
    implementation = course / "workspaces" / "custom" / "manual.py"
    implementation.write_text("from ngy_lab.implementations.manual import run\n", encoding="utf-8")
    result = cli(course, "run", "--implementation", "workspaces/custom/manual.py")
    assert result.returncode == 0, result.stdout + result.stderr
    report = json.loads(next((course / "runs").glob("*/report.json")).read_text("utf-8"))
    assert report["request"]["implementation"] == "workspaces/custom/manual.py"
    assert report["learning"]["mastery"] is None  # Importing a solution is no mastery evidence.


@pytest.mark.parametrize("name", ["..", "../outside", "CON", "a/b", "", "a" * 49])
def test_workspace_path_validation(course, name):
    result = cli(course, "new-workspace", name)
    assert result.returncode == 2
    assert not (course / "workspaces").exists()


@pytest.mark.parametrize(
    "arguments",
    [
        ["run", "--mode", "live"],
        ["run", "--mode", "live", "--base-url", "https://example.com/v1", "--model", "example"],
        ["run", "--model", "unexpected"],
        ["run", "--json-mode"],
        ["run", "--request-timeout-seconds", "10"],
        ["run", "--max-output-tokens", "512"],
        ["run", "--token-limit-field", "max_tokens"],
        ["run", "--max-model-decisions", "9"],
        ["run", "--timeout-seconds", "nan"],
        ["run", "--implementation", "../../outside.py"],
    ],
)
def test_invalid_request_does_not_run_or_send_content(course, arguments):
    result = cli(course, *arguments)
    assert result.returncode == 2, result.stdout + result.stderr
    assert not (course / "runs").exists()
