"""Real Windows isolation checks use generated files and loopback only."""

from __future__ import annotations

import json
import os
import queue
import shutil
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path

import pytest

import ngy_lab.sandbox_windows as sandbox_windows
from ngy_lab.sandbox_windows import (
    PROFILE_MARKER,
    AppContainerProfile,
    SandboxUnavailable,
    cleanup_stale_profile,
)

pytestmark = pytest.mark.skipif(os.name != "nt", reason="Windows LPAC kernel integration")


@pytest.fixture(scope="module")
def python_template(tmp_path_factory):
    """Copy, never ACL-modify, the installed CPython files."""
    source = Path(sys.base_prefix)
    target = tmp_path_factory.mktemp("lpac-python-template")
    shutil.copy2(source / "python.exe", target / "python.exe")
    for path in source.glob("*.dll"):
        shutil.copy2(path, target / path.name)
    shutil.copytree(source / "DLLs", target / "DLLs")
    shutil.copytree(
        source / "Lib",
        target / "Lib",
        ignore=shutil.ignore_patterns(
            "site-packages",
            "__pycache__",
            "test",
            "tests",
            "idlelib",
            "ensurepip",
            "tkinter",
            "turtledemo",
        ),
    )
    return target


@pytest.fixture
def staged(tmp_path, python_template):
    stage = tmp_path / "stage"
    runtime = stage / "runtime"
    workspace = stage / "workspace"
    shutil.copytree(python_template, runtime)
    workspace.mkdir()
    return stage, runtime, workspace


def worker(profile, runtime, workspace, code, **limits):
    return profile.launch(
        runtime / "python.exe",
        ["-I", "-S", "-B", "-u", "-c", code],
        cwd=workspace,
        env={
            "SystemRoot": os.environ["SystemRoot"],
            "TEMP": str(workspace),
            "TMP": str(workspace),
            "LOCALAPPDATA": str(workspace),
            "APPDATA": str(workspace),
            "USERPROFILE": str(workspace),
        },
        **limits,
    )


def test_profile_cleanup_marker_and_acl_scope(tmp_path):
    stage = tmp_path / "stage"
    stage.mkdir()
    private = tmp_path / "private.txt"
    private.write_text("generated fixture", encoding="utf-8")
    with AppContainerProfile(stage) as profile:
        assert profile.sid.startswith("S-1-15-2-")
        marker = tmp_path / PROFILE_MARKER
        assert json.loads(marker.read_text(encoding="utf-8")) == {"profile": profile.name}
        with pytest.raises(ValueError, match="stage"):
            profile.grant_access(private)
        hardlink = stage / "hardlink.txt"
        os.link(private, hardlink)
        with pytest.raises(ValueError, match="硬链接"):
            profile.grant_access(stage)
        hardlink.unlink()
        profile.grant_access(stage)
    assert not (tmp_path / PROFILE_MARKER).exists()
    assert cleanup_stale_profile(tmp_path) is False


@pytest.mark.parametrize("value", [{"profile": "other-app"}, {"profile": "ngy-lab-../"}, [], {}])
def test_cleanup_rejects_unowned_profile_names(tmp_path, value):
    marker = tmp_path / PROFILE_MARKER
    marker.write_text(json.dumps(value), encoding="utf-8")
    with pytest.raises(ValueError):
        cleanup_stale_profile(tmp_path)
    assert marker.exists()


def test_lpac_pipes_files_network_and_child_process(staged):
    stage, runtime, workspace = staged
    private = stage.parent / "host-private.txt"
    private.write_text("generated host-private fixture", encoding="utf-8")
    denied = stage / "ungranted.txt"
    denied.write_text("generated ungranted fixture", encoding="utf-8")
    with socket.socket() as server:
        server.bind(("127.0.0.1", 0))
        server.listen(1)
        server.settimeout(0.1)
        code = f"""
import ctypes, json, os, socket, subprocess, sys, winreg
results = {{"echo": sys.stdin.buffer.readline().decode().strip()}}
for name, path in [("host_read", {str(private)!r}), ("ungranted_read", {str(denied)!r})]:
    try:
        open(path).read()
        results[name] = "ALLOWED"
    except PermissionError:
        results[name] = "denied"
try:
    open({str(runtime / "forbidden.txt")!r}, "w").write("bad")
    results["runtime_write"] = "ALLOWED"
except PermissionError:
    results["runtime_write"] = "denied"
try:
    open(PROFILE_STORAGE_PATH, "w").write("bad")
    results["profile_write"] = "ALLOWED"
except PermissionError:
    results["profile_write"] = "denied"
registry_api = ctypes.WinDLL("userenv", use_last_error=True).GetAppContainerRegistryLocation
registry_api.argtypes = [ctypes.c_uint32, ctypes.POINTER(ctypes.c_void_p)]
registry_api.restype = ctypes.c_int32
profile_key = ctypes.c_void_p()
registry_status = registry_api(winreg.KEY_READ | winreg.KEY_WRITE, ctypes.byref(profile_key))
if registry_status == 0:
    profile_registry = profile_key.value
    try:
        try:
            with winreg.CreateKeyEx(profile_registry, "ngy-fixture-probe", 0, winreg.KEY_SET_VALUE) as key:
                winreg.SetValueEx(key, "fixture", 0, winreg.REG_SZ, "generated probe")
            results["profile_registry_write"] = "ALLOWED"
            winreg.DeleteKey(profile_registry, "ngy-fixture-probe")
        except PermissionError:
            results["profile_registry_write"] = "denied"
    finally:
        winreg.CloseKey(profile_registry)
elif registry_status & 0xffff == 5:
    results["profile_registry_write"] = "denied"
else:
    raise RuntimeError("profile registry probe failed: " + hex(registry_status & 0xffffffff))
open("allowed.txt", "w").write("workspace output")
results["workspace_write"] = open("allowed.txt").read()
try:
    subprocess.run([sys.executable, "-I", "-S", "-c", "pass"], check=True)
    results["child"] = "ALLOWED"
except OSError:
    results["child"] = "denied"
try:
    socket.create_connection(("127.0.0.1", {server.getsockname()[1]}), timeout=1).close()
    results["network"] = "ALLOWED"
except OSError as error:
    results["network"] = error.winerror
print(json.dumps(results))
print("separate stderr", file=sys.stderr)
"""
        with AppContainerProfile(stage) as profile:
            profile.grant_access(runtime)
            profile.grant_access(workspace, writable=True)
            profile_folder = profile.restrict_profile_storage()
            code = code.replace("PROFILE_STORAGE_PATH", repr(str(profile_folder / "forbidden.txt")))
            with worker(profile, runtime, workspace, code) as process:
                process.stdin.write(b"rpc fixture\n")
                process.stdin.close()
                exit_code = process.wait(timeout=20)
                output, errors = process.stdout.read(), process.stderr.read()
                assert exit_code == 0, (exit_code, output, errors)
                assert json.loads(output) == {
                    "echo": "rpc fixture",
                    "host_read": "denied",
                    "ungranted_read": "denied",
                    "runtime_write": "denied",
                    "profile_write": "denied",
                    "profile_registry_write": "denied",
                    "workspace_write": "workspace output",
                    "child": "denied",
                    "network": 10013,
                }
                assert errors.strip() == b"separate stderr"
            with pytest.raises(RuntimeError, match="worker"):
                profile.grant_access(workspace)
        with pytest.raises(TimeoutError):
            server.accept()
    assert not (stage.parent / PROFILE_MARKER).exists()
    assert (workspace / "allowed.txt").read_text() == "workspace output"
    assert not (runtime / "forbidden.txt").exists()


def test_memory_cpu_and_force_termination(staged):
    stage, runtime, workspace = staged
    cases = [
        (
            "try:\n a = bytearray(256 * 1024 * 1024)\n print('ALLOWED')\nexcept MemoryError:\n print('bounded')",
            {"memory_bytes": 128 * 1024 * 1024},
            "memory",
        ),
        ("print('running', flush=True)\nwhile True: pass", {"cpu_time_seconds": 0.25}, "cpu"),
        ("import time; time.sleep(60)", {}, "cancel"),
    ]
    for code, limits, kind in cases:
        with AppContainerProfile(stage) as profile:
            profile.grant_access(runtime)
            profile.grant_access(workspace, writable=True)
            with worker(profile, runtime, workspace, code, **limits) as process:
                process.stdin.close()
                if kind == "cancel":
                    with pytest.raises(subprocess.TimeoutExpired):
                        process.wait(timeout=0.1)
                    assert process.poll() is None
                    process.terminate()
                exit_code = process.wait(timeout=15)
                output, errors = process.stdout.read(), process.stderr.read()
                if kind == "memory":
                    assert exit_code == 0 and output.strip() == b"bounded", (
                        exit_code,
                        output,
                        errors,
                    )
                else:
                    assert exit_code != 0
                    if kind == "cpu":
                        assert output.strip() == b"running", (exit_code, output, errors)
        assert not (stage.parent / PROFILE_MARKER).exists()


def test_failure_before_resume_cleans_worker(staged, monkeypatch):
    stage, runtime, workspace = staged
    with AppContainerProfile(stage) as profile:
        profile.grant_access(runtime)
        profile.grant_access(workspace, writable=True)

        def reject(_process):
            raise SandboxUnavailable("injected token verification failure")

        monkeypatch.setattr(profile, "_verify_token", reject)
        with pytest.raises(SandboxUnavailable, match="injected"):
            worker(profile, runtime, workspace, "open('must-not-exist.txt', 'w').write('bad')")
    assert not (workspace / "must-not-exist.txt").exists()
    assert not (stage.parent / PROFILE_MARKER).exists()


def test_regular_appcontainer_is_rejected_before_resume(staged, monkeypatch):
    stage, runtime, workspace = staged
    monkeypatch.setattr(sandbox_windows, "_LPAC_POLICY", 0)
    with AppContainerProfile(stage) as profile:
        profile.grant_access(runtime)
        profile.grant_access(workspace, writable=True)
        with pytest.raises(SandboxUnavailable, match="ALL_APPLICATION_PACKAGES"):
            worker(profile, runtime, workspace, "open('must-not-exist.txt', 'w').write('bad')")
    assert not (workspace / "must-not-exist.txt").exists()
    assert not (stage.parent / PROFILE_MARKER).exists()


def test_host_hard_kill_terminates_worker_and_recovers_profile(staged):
    stage, runtime, workspace = staged
    package_root = Path(__file__).resolve().parents[1]
    code = f"""
import sys, time
from pathlib import Path
sys.path.insert(0, {str(package_root)!r})
from ngy_lab.sandbox_windows import AppContainerProfile
with AppContainerProfile(Path({str(stage)!r})) as profile:
    profile.grant_access(Path({str(runtime)!r}))
    profile.grant_access(Path({str(workspace)!r}))
    profile.restrict_profile_storage()
    with profile.launch(
        Path({str(runtime / "python.exe")!r}),
        ["-I", "-S", "-B", "-u", "-c", "while True: pass"],
        cwd=Path({str(workspace)!r}),
        env={{"SystemRoot": {os.environ["SystemRoot"]!r}, "TEMP": {str(workspace)!r}, "TMP": {str(workspace)!r}}},
        cpu_time_seconds=60,
    ) as process:
        print(process.pid, flush=True)
        time.sleep(60)
"""
    host = subprocess.Popen(
        [sys.executable, "-I", "-S", "-u", "-c", code],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        creationflags=subprocess.CREATE_NO_WINDOW,
    )
    api = sandbox_windows._Win32()
    worker_handle = None
    marker = stage.parent / PROFILE_MARKER
    try:
        messages = queue.Queue()
        threading.Thread(target=lambda: messages.put(host.stdout.readline()), daemon=True).start()
        line = messages.get(timeout=20)
        if not line:
            host.wait(timeout=5)
            pytest.fail(f"Trusted test host failed: {host.stderr.read()!r}")
        pid = int(line)
        open_process = api.kernel.OpenProcess
        open_process.argtypes = [sandbox_windows.DWORD, sandbox_windows.BOOL, sandbox_windows.DWORD]
        open_process.restype = sandbox_windows.HANDLE
        worker_handle = open_process(0x100000, False, pid)  # SYNCHRONIZE only.
        api.check(worker_handle, "OpenProcess(test worker)")
        assert api.kernel.WaitForSingleObject(worker_handle, 0) == 258
        assert marker.exists()
        deadline = time.monotonic() + 2
        host.kill()  # TerminateProcess: no Python context/finally cleanup runs.
        host.wait(timeout=2)
        remaining_ms = max(0, int((deadline - time.monotonic()) * 1000))
        assert api.kernel.WaitForSingleObject(worker_handle, remaining_ms) == 0
        assert marker.exists(), "Hard-killed host cannot remove its cleanup marker"
        assert cleanup_stale_profile(stage.parent) is True
        assert not marker.exists()
        assert cleanup_stale_profile(stage.parent) is False
    finally:
        if host.poll() is None:
            host.kill()
        host.wait(timeout=5)
        api.close(worker_handle)
        host.stdout.close()
        host.stderr.close()
        if marker.exists():
            cleanup_stale_profile(stage.parent)
