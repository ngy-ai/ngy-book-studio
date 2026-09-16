"""Copied into an OS-isolated worker; contains no fixtures, providers or grader."""

import importlib.util
import io
import json
import sys
import threading
import traceback
from dataclasses import asdict
from pathlib import Path

APP_ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(APP_ROOT))
sys.path.append(str(APP_ROOT.parent / "runtime" / "Lib" / "site-packages"))

from desktop_compat import install_deferred_overlapped  # noqa: E402

from ngy_lab.contracts import (  # noqa: E402
    AgentOutcome,
    BudgetExceeded,
    LabError,
    ProtocolError,
    RunLimits,
)

MAX_PACKET = 1024 * 1024
_output = sys.stdout
_input = sys.stdin
_lock = threading.Lock()
_sequence = 0


def send(packet):
    encoded = json.dumps(packet, ensure_ascii=True, allow_nan=False, separators=(",", ":"))
    if len(encoded) > MAX_PACKET:
        raise ProtocolError("隔离进程 RPC 消息超过 1 MiB")
    with _lock:
        _output.write(encoded + "\n")
        _output.flush()


def receive():
    line = _input.readline(MAX_PACKET + 1)
    if not line or len(line) > MAX_PACKET or not line.endswith("\n"):
        raise ProtocolError("隔离进程 RPC 输入无效或连接关闭")
    return json.loads(line)


def request(operation, **arguments):
    global _sequence
    _sequence += 1
    request_id = _sequence
    send({"op": operation, "id": request_id, **arguments})
    response = receive()
    if response.get("id") != request_id:
        raise ProtocolError("隔离进程 RPC 响应序号不匹配")
    if response.get("ok") is True:
        return response.get("value")
    error = response.get("error", {})
    if error.get("kind") == "budget":
        raise BudgetExceeded(error["reason"])
    if error.get("kind") == "protocol":
        raise ProtocolError(error.get("message", "宿主拒绝 RPC"))
    raise LabError(error.get("message", "宿主操作失败"))


class ModelProxy:
    def __init__(self, metadata):
        self.metadata = metadata

    def complete(self, messages, schemas):
        return request("model", messages=messages, schemas=schemas)


class ToolsProxy:
    def __init__(self, schemas):
        self.schemas = schemas

    def execute(self, call):
        return request("tool", call=call)


class Console(io.TextIOBase):
    def __init__(self, stream):
        self.stream = stream

    def write(self, text):
        if not isinstance(text, str):
            raise TypeError("console writes require text")
        for offset in range(0, len(text), 4096):
            send({"op": "output", "stream": self.stream, "text": text[offset : offset + 4096]})
        return len(text)

    def flush(self):
        pass


def main():
    initial = receive()
    sys.stdout, sys.stderr = Console("stdout"), Console("stderr")
    try:
        install_deferred_overlapped()
        name = "ngy_isolated_submission"
        spec = importlib.util.spec_from_file_location(name, APP_ROOT / "submission.py")
        module = importlib.util.module_from_spec(spec)
        sys.modules[name] = module
        spec.loader.exec_module(module)
        run = getattr(module, "run", None)
        if not callable(run):
            raise ProtocolError("代码必须提供 run(task, model, tools, limits, emit)")
        outcome = run(
            initial["task"],
            ModelProxy(initial["model"]),
            ToolsProxy(initial["schemas"]),
            RunLimits(**initial["limits"]),
            lambda event: send({"op": "emit", "event": event}),
        )
        if not isinstance(outcome, AgentOutcome):
            raise ProtocolError("run 必须返回 AgentOutcome")
        send({"op": "finished", "outcome": asdict(outcome)})
    except BudgetExceeded as error:
        send({"op": "raised", "kind": "budget", "reason": error.reason})
    except MemoryError:
        send({"op": "raised", "kind": "memory", "message": "隔离进程达到内存上限"})
    except BaseException as error:
        # The host does not treat this self-reported diagnostic as grading evidence.
        send(
            {
                "op": "raised",
                "kind": "worker",
                "type": type(error).__name__,
                "message": str(error)[:1024],
                "traceback": "".join(traceback.format_exception(error, limit=20))[-16384:],
            }
        )


if __name__ == "__main__":
    main()
