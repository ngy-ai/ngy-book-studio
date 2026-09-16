"""Run reviewed course code and produce checks from host-observed evidence."""

import copy
import hashlib
import importlib
import importlib.util
import json
import sys
import time
import traceback
import uuid
from collections.abc import Callable
from dataclasses import asdict
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from ngy_lab import COURSE_ID, COURSE_VERSION, REPORT_VERSION, RULES_VERSION
from ngy_lab.contracts import AgentOutcome, BudgetExceeded, LabError, ProtocolError, RunLimits
from ngy_lab.runtime import ObservedModel, ScriptedModel, ToolBroker, json_text
from ngy_lab.scenarios import Scenario, get_scenario

COURSE_ROOT = Path(__file__).resolve().parents[1]


def source_snapshot(implementation: str) -> dict[str, str]:
    paths = list((COURSE_ROOT / "ngy_lab").rglob("*.py"))
    paths += [COURSE_ROOT / "pyproject.toml", COURSE_ROOT / "uv.lock"]
    if (COURSE_ROOT / ".python-version").exists():
        paths.append(COURSE_ROOT / ".python-version")
    if implementation not in {"manual", "langgraph"}:
        path = (COURSE_ROOT / implementation).resolve()
        if not path.is_relative_to(COURSE_ROOT / "workspaces") or path.suffix != ".py":
            raise ValueError("学习者实现必须是本课程 workspaces/ 下的 Python 文件")
        if not path.is_file():
            raise ValueError("学习者实现文件不存在")
        paths += list(path.parent.rglob("*.py"))
    snapshot = {}
    for path in sorted(set(paths)):
        if path.is_symlink() or not path.resolve().is_relative_to(COURSE_ROOT):
            raise ValueError("代码快照不接受指向课程目录外的链接")
        data = path.read_bytes()
        if len(data) > 2 * 1024 * 1024:
            raise ValueError("单个代码快照文件超过 2 MiB")
        snapshot[path.relative_to(COURSE_ROOT).as_posix()] = data.decode("utf-8")
    return snapshot


def load_run(implementation: str) -> Callable[..., AgentOutcome]:
    if implementation in {"manual", "langgraph"}:
        name = "manual" if implementation == "manual" else "langgraph_agent"
        return importlib.import_module(f"ngy_lab.implementations.{name}").run
    path = (COURSE_ROOT / implementation).resolve()
    if not path.is_relative_to(COURSE_ROOT / "workspaces") or path.suffix != ".py":
        raise ValueError("学习者实现必须位于本课程 workspaces/ 下")
    spec = importlib.util.spec_from_file_location(f"course_submission_{uuid.uuid4().hex}", path)
    if spec is None or spec.loader is None:
        raise ValueError("无法载入学习者实现")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    try:
        spec.loader.exec_module(module)
    except BaseException:
        sys.modules.pop(spec.name, None)
        raise
    run = getattr(module, "run", None)
    if not callable(run):
        raise ValueError("实现必须提供 run(task, model, tools, limits, emit)")
    return run


class InjectInvalidCall:
    """Expose one documented fault to a live model; retain the provider's original response."""

    def __init__(self, inner: Any, scenario: Scenario):
        self.inner, self.scenario = inner, scenario
        self.injections: list[dict[str, Any]] = []

    @property
    def metadata(self):
        return self.inner.metadata

    def set_deadline(self, deadline: float):
        self.inner.set_deadline(deadline)

    def complete(self, messages, schemas):
        response = self.inner.complete(messages, schemas)
        if not self.injections and response.get("tool_calls"):
            original = copy.deepcopy(response)
            response = copy.deepcopy(response)
            function = response["tool_calls"][0]["function"]
            if self.scenario.name == "invalid_call":
                function["name"] = "unregistered_tool"
            else:
                function["arguments"] = "{}"
            self.injections.append({"provider_response": original, "exposed_response": response})
        return response


def evaluate(
    scenario: Scenario,
    outcome: AgentOutcome,
    model: ObservedModel,
    tools: ToolBroker,
    limits: RunLimits,
    snapshot_unchanged: bool,
) -> list[dict[str, Any]]:
    checks: list[dict[str, Any]] = []

    def check(name: str, passed: bool, detail: str):
        checks.append({"id": name, "passed": bool(passed), "detail": detail})

    check("code_snapshot", snapshot_unchanged, "执行前后课程代码摘要一致")
    check("model_limit", len(model.records) <= limits.max_model_decisions, "模型请求尝试也计入预算")
    check("tool_limit", tools.executions <= limits.max_tool_executions, "非法请求不计真实工具执行")
    check("authorized_dispatch", not tools.violations, "不允许绕过调用授权或反复读取永久失败资料")
    retry_valid = all(
        len(history) <= 2
        and (len(history) < 2 or history[0].get("error", {}).get("retryable") is True)
        for history in tools.attempts.values()
    )
    check("retry_policy", retry_valid, "同一工具和参数仅在明确可重试失败后额外执行一次")
    if scenario.expected_stop != "final_answer":
        reason = scenario.expected_stop
        evidence = (
            len(model.records) == limits.max_model_decisions
            if reason == "model_budget"
            else tools.exhausted == "tool_budget"
        )
        check(
            "explicit_stop",
            outcome.status == "budget_exhausted" and outcome.stop_reason == reason and evidence,
            "预算终止必须由宿主计数或拒绝记录支持",
        )
        check("no_invented_answer", outcome.answer is None, "预算耗尽不伪造完整答案")
        observed_history = list(model.previous or []) + [
            tools.results[call["id"]] for call in tools.pending if call["id"] in tools.results
        ]
        check(
            "message_history",
            outcome.messages == observed_history,
            "预算结束也要保留已观察到的完整对话与已返回工具消息",
        )
        return checks
    check(
        "explicit_stop",
        outcome.status == "completed" and outcome.stop_reason == "final_answer",
        "必须明确完成；错误、TODO、超时不算完成",
    )
    final = model.records[-1].get("response", {}) if model.records else {}
    try:
        model_answer = json.loads(final.get("content") or "null")
    except (ValueError, TypeError):
        model_answer = None
    check(
        "observed_final",
        bool(model.records)
        and not final.get("tool_calls")
        and isinstance(model_answer, dict)
        and outcome.answer == model_answer,
        "答案必须对应实际模型最后一轮结果，不能依赖学生打印或 emit",
    )
    check(
        "message_history", outcome.messages == model.previous, "最终轨迹与宿主观察到的完整对话一致"
    )
    answer = outcome.answer if isinstance(outcome.answer, dict) else {}
    check("answer_shape", answer.keys() == scenario.expected_answer.keys(), "恰有三个规定字段")
    for name, expected in scenario.expected_answer.items():
        item = answer.get(name)
        valid_shape = isinstance(item, dict) and item.keys() == {"value", "source_ids"}
        check(
            f"{name}.value",
            valid_shape and item.get("value") == expected["value"],
            "使用资料原词或明确的未知 null；不进行不透明的 AI 自动判分",
        )
        sources = item.get("source_ids") if isinstance(item, dict) else None
        grounded = (
            isinstance(sources, list)
            and sources == expected["source_ids"]
            and all(
                isinstance(source, str) and source in tools.successful_reads for source in sources
            )
        )
        check(
            f"{name}.sources",
            valid_shape and grounded,
            "每个已知字段的来源必须支持该事实且正文实际读取成功；搜索标题无效",
        )
    results = [record["result"] for record in tools.records]
    if scenario.name in {"invalid_call", "invalid_arguments"}:
        code = "UNKNOWN_TOOL" if scenario.name == "invalid_call" else "INVALID_ARGUMENTS"
        check(
            "invalid_recovery",
            any(r.get("error", {}).get("code") == code for r in results)
            and any(r["actual_execution"] for r in tools.records),
            "先拒绝非法调用，再允许纠正",
        )
    if scenario.transient_read_ids or scenario.transient_search:
        check(
            "transient_recovery",
            any(
                len(history) == 2
                and history[0].get("error", {}).get("code") == "TIMEOUT"
                and history[1].get("ok") is True
                for history in tools.attempts.values()
            ),
            "暂时失败后恰好重试一次并恢复，已有结果继续保留",
        )
    for missing_id in scenario.missing_ids:
        reads = [
            r
            for r in tools.records
            if r["name"] == "read_document"
            and r["arguments"] == {"document_id": missing_id}
            and r["actual_execution"]
        ]
        check(
            f"missing.{missing_id}",
            len(reads) == 1 and reads[0]["result"].get("error", {}).get("code") == "NOT_FOUND",
            "永久缺失只读取一次，并保留其他有依据的字段",
        )
    return checks


def execute_run(
    implementation: str,
    scenario: str | Scenario,
    mode: str = "scripted",
    limits: RunLimits | None = None,
    live_config: Any = None,
) -> dict[str, Any]:
    limits = limits or RunLimits()
    scenario = get_scenario(scenario) if isinstance(scenario, str) else scenario
    if mode not in {"scripted", "live"}:
        raise ValueError("模型模式必须为 scripted 或 live")
    if mode == "live" and scenario.expected_stop != "final_answer":
        raise ValueError("预算压力序列仅用于 scripted 模式")
    snapshot = source_snapshot(implementation)
    digest = hashlib.sha256(json_text(snapshot).encode("utf-8")).hexdigest()
    if mode == "scripted":
        raw_model = ScriptedModel(scenario)
    else:
        from ngy_lab.live import OpenAICompatibleModel

        if live_config is None:
            raise ValueError("live 模式必须显式配置端点与模型")
        raw_model = OpenAICompatibleModel(live_config)
    provider = raw_model
    if mode == "live" and scenario.name in {"invalid_call", "invalid_arguments"}:
        raw_model = InjectInvalidCall(raw_model, scenario)
    started = time.monotonic()
    tools = ToolBroker(scenario, limits, started)
    model = ObservedModel(raw_model, tools, limits)
    learner_events: list[dict[str, Any]] = []
    error_info = None

    def emit(event: dict[str, Any]) -> None:
        # A student's instrumentation is displayed separately and never graded.
        if not isinstance(event, dict):
            raise ProtocolError("emit 必须接收字典，例如 {'phase': 'model'}")
        if len(learner_events) < 256:
            encoded = json_text(event)
            if len(encoded.encode("utf-8")) <= 4096:
                learner_events.append(json.loads(encoded))

    try:
        run = load_run(implementation)
        outcome = run(scenario.task, model, tools, limits, emit)
        if not isinstance(outcome, AgentOutcome):
            raise ValueError("run 必须返回 AgentOutcome")
        tools.check_deadline()
    except BudgetExceeded as error:
        outcome = AgentOutcome("budget_exhausted", error.reason, None, model.previous or [])
    except NotImplementedError:
        outcome = AgentOutcome("incomplete", "not_implemented", None)
    except KeyboardInterrupt:
        outcome = AgentOutcome("cancelled", "keyboard_interrupt", None)
    except Exception as error:
        outcome = AgentOutcome("error", "runtime_error", None)
        # Unknown student/provider exceptions can contain secrets. Only safe lab errors get text.
        error_info = {
            "type": type(error).__name__,
            "message": str(error)
            if isinstance(error, LabError)
            else "运行失败；异常正文未写入报告，请在已审查的本地代码中定位",
            "frames": [
                {
                    "file": Path(frame.filename).resolve().relative_to(COURSE_ROOT).as_posix(),
                    "line": frame.lineno,
                    "function": frame.name,
                }
                for frame in traceback.extract_tb(error.__traceback__)
                if Path(frame.filename).resolve().is_relative_to(COURSE_ROOT)
            ],
        }
        if isinstance(error, SyntaxError) and error.filename:
            location = Path(error.filename).resolve()
            if location.is_relative_to(COURSE_ROOT):
                error_info["frames"].append(
                    {
                        "file": location.relative_to(COURSE_ROOT).as_posix(),
                        "line": error.lineno,
                        "function": "module_syntax",
                    }
                )
    elapsed = time.monotonic() - started
    unchanged = source_snapshot(implementation) == snapshot
    checks = evaluate(scenario, outcome, model, tools, limits, unchanged)
    return {
        "report_version": REPORT_VERSION,
        "run_id": uuid.uuid4().hex,
        "created_at": datetime.now(UTC).isoformat(),
        "request": {
            "course_id": COURSE_ID,
            "course_version": COURSE_VERSION,
            "task_id": scenario.name,
            "task_version": scenario.version,
            "rules_version": RULES_VERSION,
            "task": scenario.task,
            "implementation": implementation,
            "model_mode": mode,
            "model": model.metadata,
            "limits": asdict(limits),
            "code_sha256": digest,
            "fixture_sha256": hashlib.sha256(
                json_text(
                    {
                        "name": scenario.name,
                        "project": scenario.project,
                        "documents": [asdict(doc) for doc in scenario.documents],
                        "missing_ids": sorted(scenario.missing_ids),
                        "transient_read_ids": sorted(scenario.transient_read_ids),
                        "transient_search": scenario.transient_search,
                    }
                ).encode("utf-8")
            ).hexdigest(),
        },
        "outcome": asdict(outcome),
        "error": error_info,
        "observations": {
            "model_calls": model.records,
            "tool_calls": tools.records,
            "successful_reads": tools.successful_reads,
            "fault_injections": getattr(raw_model, "injections", []),
            "budget_events": tools.exhaustion_events,
            "policy_violations": tools.violations,
        },
        "metrics": {
            "model_decisions": len(model.records),
            "actual_tool_executions": tools.executions,
            "tool_dispatch_attempts": tools.dispatch_attempts,
            "elapsed_seconds": round(elapsed, 6),
            "provider_usage": copy.deepcopy(getattr(provider, "usage", None)),
            "provider_usage_complete": (
                mode == "live"
                and provider.metadata.get("requests_with_usage") == len(model.records)
                and bool(model.records)
            ),
            "framework_steps": sum(
                isinstance(e, dict) and e.get("phase") == "graph_node" for e in learner_events
            ),
        },
        "checks": checks,
        "passed": all(check["passed"] for check in checks),
        "learning": {
            "status": "pending_review",
            "mastery": None,
            "note": "作品检查不推断独立掌握；需首次预测、解释、提示记录及未见变式证据。",
        },
        "learner_events": learner_events,
        "code_snapshot": snapshot,
    }


def save_report(report: dict[str, Any], root: Path | None = None) -> Path:
    run_dir = (root or COURSE_ROOT / "runs") / report["run_id"]
    run_dir.mkdir(parents=True, exist_ok=False)
    snapshot = report["code_snapshot"]
    for name, content in snapshot.items():
        target = run_dir / "code" / name
        if not target.resolve().is_relative_to((run_dir / "code").resolve()):
            raise ValueError("快照路径越界")
        target.parent.mkdir(parents=True, exist_ok=True)
        with target.open("w", encoding="utf-8", newline="") as handle:
            handle.write(content)
    path = run_dir / "report.json"
    stored = {key: value for key, value in report.items() if key != "code_snapshot"}
    stored["code_snapshot_directory"] = "code"
    path.write_text(json.dumps(stored, ensure_ascii=False, indent=2), encoding="utf-8")
    return path
