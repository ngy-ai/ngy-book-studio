"""Host-owned RPC and resource policy around the Windows AppContainer worker."""

import copy
import hashlib
import json
import math
import os
import queue
import shutil
import sys
import threading
import time
import uuid
from dataclasses import asdict, dataclass
from datetime import UTC, datetime
from pathlib import Path

from moye_lab import COURSE_ID, COURSE_VERSION, REPORT_VERSION, RULES_VERSION
from moye_lab.chapter_runtime import ChapterModel, ChapterTools, evaluate_chapter, make_case
from moye_lab.contracts import AgentOutcome, BudgetExceeded, LabError, ProtocolError, RunLimits
from moye_lab.desktop_compat import COMPATIBILITY_ID
from moye_lab.implementations.common import (
    _finite_json_float,
    _reject_non_finite,
    _unique_json_object,
    _validate_json_unicode,
)
from moye_lab.runner import COURSE_ROOT, evaluate, source_snapshot
from moye_lab.runtime import ObservedModel, ScriptedModel, ToolBroker, json_text
from moye_lab.scenarios import get_scenario

MAX_PACKET_BYTES = 1024 * 1024
MAX_CODE_BYTES = 128 * 1024


@dataclass(frozen=True)
class SandboxLimits:
    memory_bytes: int = 256 * 1024 * 1024
    cpu_time_seconds: float = 10.0
    cpu_rate_percent: int = 25
    output_bytes: int = 2 * 1024 * 1024
    max_rpc_messages: int = 512

    def __post_init__(self):
        if (
            type(self.memory_bytes) is not int
            or not 32 * 1024 * 1024 <= self.memory_bytes <= 512 * 1024 * 1024
        ):
            raise ValueError("隔离内存上限必须为 32..512 MiB")
        if (
            isinstance(self.cpu_time_seconds, bool)
            or not isinstance(self.cpu_time_seconds, (int, float))
            or not math.isfinite(self.cpu_time_seconds)
            or not 0 < self.cpu_time_seconds <= 30
        ):
            raise ValueError("隔离 CPU 时间上限必须在 (0, 30] 秒内")
        if type(self.cpu_rate_percent) is not int or not 1 <= self.cpu_rate_percent <= 100:
            raise ValueError("隔离 CPU 配额必须为 1..100%")
        if type(self.output_bytes) is not int or not 4096 <= self.output_bytes <= 4 * 1024 * 1024:
            raise ValueError("隔离总输出上限必须为 4 KiB..4 MiB")
        if type(self.max_rpc_messages) is not int or not 1 <= self.max_rpc_messages <= 1024:
            raise ValueError("隔离 RPC 数量上限必须为 1..1024")


class SandboxStop(LabError):
    def __init__(self, reason, *, exit_code=None):
        self.reason = reason
        self.exit_code = exit_code
        super().__init__(reason)


class WorkerFailure(LabError):
    def __init__(self, packet):
        self.worker_traceback = str(packet.get("traceback", ""))[-16384:]
        super().__init__(
            "学习代码运行失败："
            + str(packet.get("type", "Error"))[:64]
            + "："
            + str(packet.get("message", ""))[:1024]
        )


def _check_cancel(cancel):
    if cancel.is_set():
        raise SandboxStop("cancelled")


def _copy_tree(source, destination, cancel, *, skip=()):
    destination.mkdir(parents=True, exist_ok=True)
    for item in source.iterdir():
        _check_cancel(cancel)
        if item.name in {*skip, "__pycache__", ".git"}:
            continue
        if item.is_symlink() or item.is_junction():
            raise LabError("隔离运行时不接受符号链接或目录联接")
        target = destination / item.name
        if item.is_dir():
            _copy_tree(item, target, cancel)
        elif item.is_file():
            shutil.copyfile(item, target)


def stage_runtime(run_dir, code, cancel, progress):
    if sys.implementation.name != "cpython" or sys.version_info[:2] != (3, 12):
        raise LabError("桌面隔离课程需要 Windows CPython 3.12，请按课程 .python-version 重建环境")
    if (
        not run_dir.is_absolute()
        or not run_dir.is_dir()
        or run_dir.is_symlink()
        or run_dir.is_junction()
    ):
        raise LabError("run_dir 必须是已创建的独立绝对目录")
    if not isinstance(code, str) or len(code.encode("utf-8")) > MAX_CODE_BYTES:
        raise LabError("学习代码必须是至多 128 KiB 的 UTF-8 文本")
    stage = run_dir / "stage"
    stage.mkdir(exist_ok=False)
    runtime = stage / "runtime"
    runtime.mkdir()
    base = Path(sys.base_prefix)
    progress("准备隔离环境", "复制受控 Python 运行时和固定依赖")
    executable = base / "python.exe"
    if not executable.is_file() or not (base / "DLLs").is_dir() or not (base / "Lib").is_dir():
        raise LabError("需要完整的 Windows CPython 3.12 安装，不能使用仅有启动器的环境")
    for item in base.iterdir():
        if item.is_file() and (item.name == "python.exe" or item.suffix.lower() == ".dll"):
            shutil.copyfile(item, runtime / item.name)
    _copy_tree(base / "DLLs", runtime / "DLLs", cancel)
    _copy_tree(
        base / "Lib",
        runtime / "Lib",
        cancel,
        skip=("site-packages", "test", "idlelib", "ensurepip", "tkinter"),
    )
    site_packages = Path(sys.prefix) / "Lib" / "site-packages"
    if not (site_packages / "langgraph").is_dir():
        raise LabError("课程环境缺少锁定的 LangGraph 依赖，请先运行 uv sync --locked")
    _copy_tree(site_packages, runtime / "Lib" / "site-packages", cancel)
    app = stage / "app"
    package = app / "moye_lab"
    implementations = package / "implementations"
    implementations.mkdir(parents=True)
    (package / "__init__.py").write_text("", encoding="utf-8")
    (implementations / "__init__.py").write_text("", encoding="utf-8")
    for name in ("contracts.py", "chapter_support.py"):
        shutil.copyfile(COURSE_ROOT / "moye_lab" / name, package / name)
    shutil.copyfile(
        COURSE_ROOT / "moye_lab" / "implementations" / "common.py", implementations / "common.py"
    )
    for name in ("desktop_worker.py", "desktop_compat.py"):
        shutil.copyfile(COURSE_ROOT / "moye_lab" / name, app / name)
    (app / "submission.py").write_text(code, encoding="utf-8")
    (stage / "work").mkdir()
    return stage


def _read_pipe(stream, kind, packets, stopped):
    try:
        while not stopped.is_set():
            data = stream.readline(MAX_PACKET_BYTES + 1) if kind == "rpc" else stream.read(4096)
            if not data:
                break
            while not stopped.is_set():
                try:
                    packets.put((kind, data), timeout=0.05)
                    break
                except queue.Full:
                    continue
    except (OSError, ValueError):
        pass
    finally:
        if not stopped.is_set():
            try:
                packets.put((kind + "_eof", b""), timeout=0.05)
            except queue.Full:
                pass


def _write_packet(process, packet, cancel, started, timeout_seconds):
    encoded = (
        json.dumps(packet, ensure_ascii=True, allow_nan=False, separators=(",", ":")).encode(
            "ascii"
        )
        + b"\n"
    )
    if len(encoded) > MAX_PACKET_BYTES:
        raise ProtocolError("宿主 RPC 响应超过 1 MiB")
    # An untrusted peer may request a response and then stop reading. Keep the
    # blocking pipe write outside the supervisor so cancellation still kills
    # the Job and releases the writer's handle.
    completed = threading.Event()
    failures = []

    def write():
        try:
            process.stdin.write(encoded)
            process.stdin.flush()
        except (OSError, ValueError) as error:
            failures.append(error)
        finally:
            completed.set()

    threading.Thread(target=write, daemon=True).start()
    while not completed.wait(0.03):
        _check_cancel(cancel)
        if time.monotonic() - started >= timeout_seconds:
            raise SandboxStop("deadline")
        if process.poll() is not None:
            raise SandboxStop("worker_exited", exit_code=process.poll())
    if failures:
        raise LabError("隔离进程通信管道已关闭") from failures[0]


def _history(model, tools):
    if isinstance(tools, ChapterTools):
        return copy.deepcopy(tools.history)
    return list(model.previous or []) + [
        tools.results[call["id"]] for call in tools.pending if call["id"] in tools.results
    ]


def _serve(
    process,
    scenario,
    model,
    tools,
    limits,
    resources,
    cancel,
    progress,
    learner_events,
    output,
    started,
    pump_stop,
):
    packets = queue.Queue(maxsize=32)
    for kind, stream in (("rpc", process.stdout), ("stderr", process.stderr)):
        threading.Thread(
            target=_read_pipe, args=(stream, kind, packets, pump_stop), daemon=True
        ).start()
    _write_packet(
        process,
        {
            "task": scenario.task,
            "limits": asdict(limits),
            "model": model.metadata,
            "schemas": tools.schemas,
        },
        cancel,
        started,
        limits.timeout_seconds,
    )
    received = 0
    packet_count = 0
    last_request_id = 0
    while True:
        _check_cancel(cancel)
        if time.monotonic() - started >= limits.timeout_seconds:
            raise SandboxStop("deadline")
        try:
            kind, data = packets.get(timeout=0.03)
        except queue.Empty:
            if process.poll() is not None:
                raise SandboxStop("worker_exited", exit_code=process.poll())
            continue
        received += len(data)
        if received > resources.output_bytes:
            raise SandboxStop("output_limit")
        if kind.endswith("_eof"):
            if kind == "rpc_eof":
                raise SandboxStop("worker_exited", exit_code=process.poll())
            continue
        if kind == "stderr":
            output.append({"stream": "stderr", "text": data.decode("utf-8", "replace")})
            continue
        if len(data) > MAX_PACKET_BYTES or not data.endswith(b"\n"):
            raise SandboxStop("output_limit")
        packet_count += 1
        if packet_count > resources.max_rpc_messages:
            raise SandboxStop("rpc_limit")
        try:
            packet = json.loads(
                data.decode("utf-8"),
                object_pairs_hook=_unique_json_object,
                parse_constant=_reject_non_finite,
                parse_float=_finite_json_float,
            )
            _validate_json_unicode(packet)
        except (ValueError, UnicodeError, RecursionError) as error:
            raise ProtocolError("隔离进程输出了无效 RPC，运行已终止") from error
        if not isinstance(packet, dict):
            raise ProtocolError("隔离进程 RPC 必须为对象")
        operation = packet.get("op")
        if operation == "output":
            if packet.get("stream") not in {"stdout", "stderr"} or not isinstance(
                packet.get("text"), str
            ):
                raise ProtocolError("隔离进程日志格式无效")
            output.append({"stream": packet["stream"], "text": packet["text"]})
            continue
        if operation == "emit":
            event = packet.get("event")
            if not isinstance(event, dict):
                raise ProtocolError("emit 必须接收字典")
            if len(learner_events) < 256:
                learner_events.append(event)
            continue
        if operation == "finished":
            value = packet.get("outcome")
            if (
                not isinstance(value, dict)
                or value.keys() != {"status", "stop_reason", "answer", "messages"}
                or not isinstance(value["messages"], list)
            ):
                raise ProtocolError("隔离进程返回的 AgentOutcome 无效")
            return AgentOutcome(**value)
        if operation == "raised":
            if packet.get("kind") == "budget":
                return AgentOutcome(
                    "budget_exhausted", packet.get("reason"), None, _history(model, tools)
                )
            if packet.get("kind") == "memory":
                raise SandboxStop("memory_limit")
            raise WorkerFailure(packet)
        if operation not in {"model", "tool"}:
            raise ProtocolError("隔离进程请求了未授权的 RPC 操作")
        request_id = packet.get("id")
        if type(request_id) is not int or request_id != last_request_id + 1:
            raise ProtocolError("隔离进程 RPC 序号无效")
        last_request_id = request_id
        try:
            if operation == "model":
                progress("模型决策", f"宿主正在处理第 {len(model.records) + 1} 次决策")
                value = model.complete(packet.get("messages"), packet.get("schemas"))
            else:
                progress("读取课程资料", "宿主正在校验并执行工具请求")
                value = tools.execute(packet.get("call"))
            response = {"id": request_id, "ok": True, "value": value}
        except BudgetExceeded as error:
            response = {
                "id": request_id,
                "ok": False,
                "error": {"kind": "budget", "reason": error.reason},
            }
        except ProtocolError as error:
            # A protocol violation is terminal even if student code catches errors.
            raise ProtocolError("宿主拒绝隔离进程请求：" + str(error)) from error
        _write_packet(process, response, cancel, started, limits.timeout_seconds)


def execute_desktop_run(request, cancel=None, progress=None):
    """Run one scripted course attempt. Never falls back to unisolated execution."""
    cancel = cancel or threading.Event()
    progress = progress or (lambda _label, _detail: None)
    run_id = request.get("run_id") or uuid.uuid4().hex
    if not isinstance(run_id, str) or not run_id or len(run_id) > 128:
        raise ValueError("run_id 无效")
    if request.get("mode", "scripted") != "scripted":
        raise ValueError("桌面隔离首版仅支持 scripted 模式")
    implementation = request.get("implementation", "manual")
    if implementation not in {"manual", "langgraph"}:
        raise ValueError("实现必须为 manual 或 langgraph")
    chapter = request.get("chapter", 1)
    if type(chapter) is not int or not 1 <= chapter <= 10:
        raise ValueError("chapter 必须为 1..10 整数")
    course_id = COURSE_ID if chapter == 1 else f"agent-foundations.chapter-{chapter:02d}"
    for key, expected in {
        "course_id": course_id,
        "course_version": COURSE_VERSION,
        "rules_version": RULES_VERSION,
        "task_version": "1.0.0",
    }.items():
        if key in request and request[key] != expected:
            raise ValueError(f"{key} 与当前章节契约不一致")
    scenario_name = request.get("scenario", "normal")
    scenario = get_scenario(scenario_name) if chapter == 1 else make_case(chapter, scenario_name)
    limits = RunLimits(**request.get("limits", {}))
    resources = SandboxLimits(**request.get("resources", {}))
    run_dir = Path(request["run_dir"])
    code = request.get("code")
    if code is None:
        if chapter == 1:
            name = "manual.py" if implementation == "manual" else "langgraph_agent.py"
            code = (COURSE_ROOT / "moye_lab" / "implementations" / name).read_text("utf-8")
        else:
            code = (COURSE_ROOT / "references" / f"ch{chapter:02d}_{implementation}.py").read_text(
                "utf-8"
            )
    snapshot = source_snapshot(implementation)
    snapshot[f"desktop_submission/{implementation}.py"] = code
    started = time.monotonic()
    if chapter == 1:
        tools = ToolBroker(scenario, limits, started)
        model = ObservedModel(ScriptedModel(scenario), tools, limits)
    else:
        tools = ChapterTools(scenario, limits, started)
        model = ChapterModel(scenario, tools, limits)
    learner_events, output = [], []
    error_info = None
    isolation = {
        "backend": "windows_lpac_job",
        "verified": False,
        "network": False,
        "capabilities": ["registryRead"],
        "compatibility": COMPATIBILITY_ID,
        "python_version": sys.version.split()[0],
        "execution_contract": "synchronous_run",
        "resources": asdict(resources),
    }
    try:
        if sys.platform != "win32":
            raise LabError("隔离运行目前只支持 Windows 10/11")
        from moye_lab.sandbox_windows import AppContainerProfile

        stage = stage_runtime(run_dir, code, cancel, progress)
        _check_cancel(cancel)
        with AppContainerProfile(stage) as profile:
            profile.grant_access(stage)
            profile.restrict_profile_storage()
            environment = {
                "SystemRoot": os.environ.get("SystemRoot", "C:\\Windows"),
                "WINDIR": os.environ.get("SystemRoot", "C:\\Windows"),
                "TEMP": str(stage / "work"),
                "TMP": str(stage / "work"),
                "USERPROFILE": str(stage / "work"),
                "LOCALAPPDATA": str(stage / "work"),
                "APPDATA": str(stage / "work"),
                "PATH": str(stage / "runtime"),
                "LANGSMITH_TRACING": "false",
                "LANGCHAIN_TRACING_V2": "false",
            }
            progress("启动隔离进程", "正在校验系统隔离与资源限制")
            with profile.launch(
                stage / "runtime" / "python.exe",
                ["-I", "-S", "-B", "-u", str(stage / "app" / "desktop_worker.py")],
                cwd=stage / "work",
                env=environment,
                memory_bytes=resources.memory_bytes,
                cpu_time_seconds=resources.cpu_time_seconds,
                cpu_rate_percent=resources.cpu_rate_percent,
            ) as process:
                isolation["verified"] = True
                isolation["worker_pid"] = process.pid
                pump_stop = threading.Event()
                try:
                    outcome = _serve(
                        process,
                        scenario,
                        model,
                        tools,
                        limits,
                        resources,
                        cancel,
                        progress,
                        learner_events,
                        output,
                        started,
                        pump_stop,
                    )
                finally:
                    pump_stop.set()
                    process.terminate()
    except SandboxStop as error:
        if error.reason == "cancelled":
            status = "cancelled"
        elif error.reason == "worker_exited":
            status = "error"
        else:
            status = "budget_exhausted"
        outcome = AgentOutcome(status, error.reason, None, _history(model, tools))
        error_info = {"type": "SandboxStop", "message": error.reason}
        if error.exit_code is not None:
            error_info["worker_exit_code"] = error.exit_code
    except Exception as error:
        status = "error" if isolation["verified"] else "unavailable"
        reason = "runtime_error" if isolation["verified"] else "isolation_unavailable"
        outcome = AgentOutcome(status, reason, None, _history(model, tools))
        error_info = {
            "type": type(error).__name__,
            "message": str(error)
            if isinstance(error, (LabError, OSError))
            else "隔离环境无法完成运行，请检查 Python 环境和课程依赖",
        }
        if isinstance(error, WorkerFailure):
            error_info["worker_traceback"] = error.worker_traceback
    elapsed = time.monotonic() - started
    checks = (
        evaluate(scenario, outcome, model, tools, limits, True)
        if chapter == 1
        else evaluate_chapter(scenario, outcome, model, tools, limits)
    )
    checks.append(
        {
            "id": "os_isolation",
            "passed": isolation["verified"],
            "detail": "运行必须通过 LPAC token 与 Job Object 校验",
        }
    )
    return {
        "report_version": REPORT_VERSION,
        "run_id": run_id,
        "created_at": datetime.now(UTC).isoformat(),
        "request": {
            "chapter": chapter,
            "course_id": course_id,
            "course_version": COURSE_VERSION,
            "task_id": scenario.name,
            "task_version": scenario.version,
            "rules_version": RULES_VERSION,
            "task": scenario.task,
            "implementation": implementation,
            "model_mode": "scripted",
            "model": model.metadata,
            "limits": asdict(limits),
            "code_sha256": hashlib.sha256(json_text(snapshot).encode("utf-8")).hexdigest(),
        },
        "outcome": asdict(outcome),
        "error": error_info,
        "isolation": isolation,
        "observations": {
            "model_calls": model.records,
            "tool_calls": tools.records,
            "successful_reads": tools.successful_reads,
            "budget_events": tools.exhaustion_events,
            "policy_violations": tools.violations,
        },
        "metrics": {
            "model_decisions": len(model.records),
            "actual_tool_executions": tools.executions,
            "tool_dispatch_attempts": tools.dispatch_attempts,
            "elapsed_seconds": round(elapsed, 6),
            "framework_steps": sum(event.get("phase") == "graph_node" for event in learner_events),
            "provider_usage": None,
        },
        "checks": checks,
        "passed": all(check["passed"] for check in checks),
        "learner_events": learner_events,
        "console_output": output,
        "code_snapshot": snapshot,
        "learning": {
            "status": "pending_review",
            "mastery": None,
            "note": "隔离执行和作品检查不等于独立掌握",
        },
    }
