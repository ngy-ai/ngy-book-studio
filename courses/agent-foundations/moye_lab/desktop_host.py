"""JSONL desktop host: python -I -u <absolute path>/moye_lab/desktop_host.py."""

import json
import os
import sys
import threading
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from moye_lab.sandbox import execute_desktop_run  # noqa: E402

_write_lock = threading.Lock()


def send(event):
    with _write_lock:
        sys.stdout.write(json.dumps(event, ensure_ascii=True, allow_nan=False) + "\n")
        sys.stdout.flush()


def read_control_line(buffer, maximum):
    # A daemon blocked in sys.stdin.buffer.readline can hold Python's buffered
    # I/O lock during interpreter shutdown. Raw OS reads have no such lock.
    while b"\n" not in buffer:
        chunk = os.read(sys.stdin.fileno(), 65536)
        if not chunk:
            return b""
        buffer.extend(chunk)
        if len(buffer) > maximum:
            raise ValueError("控制命令超过大小上限")
    end = buffer.index(b"\n") + 1
    line = bytes(buffer[:end])
    del buffer[:end]
    return line


def main():
    if len(sys.argv) == 3 and sys.argv[1] == "--cleanup":
        from moye_lab.sandbox_windows import cleanup_stale_profile

        cleanup_stale_profile(Path(sys.argv[2]))
        return 0
    buffer = bytearray()
    try:
        line = read_control_line(buffer, 4 * 1024 * 1024)
        if not line:
            raise ValueError("缺少运行请求")
        request = json.loads(line)
        if not isinstance(request, dict) or request.get("command") != "run":
            raise ValueError("首条命令必须为 run")
    except (ValueError, UnicodeError, OSError):
        send({"event": "finished", "run_id": "", "error": "无效运行请求", "report": None})
        return 2
    run_id = request.get("run_id", "")
    cancel = threading.Event()

    def listen():
        while True:
            try:
                message = read_control_line(buffer, 65536)
                if not message:
                    cancel.set()
                    return
                value = json.loads(message)
                if not isinstance(value, dict):
                    raise ValueError("控制命令必须为对象")
            except (ValueError, UnicodeError, OSError):
                cancel.set()
                return
            if value.get("command") == "cancel" and value.get("run_id") == run_id:
                cancel.set()
                return

    threading.Thread(target=listen, daemon=True).start()
    send({"event": "started", "run_id": run_id})
    try:
        report = execute_desktop_run(
            request,
            cancel,
            lambda label, detail: send(
                {
                    "event": "progress",
                    "run_id": run_id,
                    "label": label,
                    "detail": detail,
                }
            ),
        )
    except Exception as error:
        send(
            {
                "event": "finished",
                "run_id": run_id,
                "report": None,
                "error": str(error) if isinstance(error, ValueError) else "隔离运行请求无法执行",
            }
        )
        return 2
    send({"event": "finished", "run_id": run_id, "report": report})
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
