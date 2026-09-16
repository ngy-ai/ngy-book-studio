"""Windows LPAC worker primitives; all failures keep untrusted code suspended.

Only a freshly staged runtime/workspace is granted access. This module neither
changes the installed interpreter nor grants network capabilities. The broker
must bound and validate pipe messages, output, and writable workspace contents.
"""

from __future__ import annotations

import ctypes as c
import json
import math
import os
import re
import stat
import subprocess
import uuid
from contextlib import contextmanager
from pathlib import Path
from typing import BinaryIO

DWORD = c.c_uint32
BOOL = c.c_int32
HANDLE = c.c_void_p
SIZE_T = c.c_size_t
PTR = c.c_void_p
PROFILE_PREFIX = "ngy-lab-"
PROFILE_MARKER = "sandbox-profile.json"
_PROFILE_PATTERN = re.compile(r"ngy-lab-[0-9a-f]{32}\Z")
_LPAC_POLICY = 1  # PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT
_BASE_CAPABILITIES = ("registryRead",)


class SandboxUnavailable(OSError):
    """Required Windows isolation could not be established."""


class _SecurityAttributes(c.Structure):
    _fields_ = [("length", DWORD), ("descriptor", PTR), ("inherit", BOOL)]


class _SecurityCapabilities(c.Structure):
    _fields_ = [("sid", PTR), ("capabilities", PTR), ("count", DWORD), ("reserved", DWORD)]


class _SidAndAttributes(c.Structure):
    _fields_ = [("sid", PTR), ("attributes", DWORD)]


class _TokenGroups(c.Structure):
    _fields_ = [("count", DWORD), ("groups", _SidAndAttributes * 1)]


class _StartupInfo(c.Structure):
    _fields_ = [
        ("cb", DWORD),
        ("reserved", c.c_wchar_p),
        ("desktop", c.c_wchar_p),
        ("title", c.c_wchar_p),
        ("x", DWORD),
        ("y", DWORD),
        ("x_size", DWORD),
        ("y_size", DWORD),
        ("x_chars", DWORD),
        ("y_chars", DWORD),
        ("fill", DWORD),
        ("flags", DWORD),
        ("show", c.c_uint16),
        ("reserved_size", c.c_uint16),
        ("reserved_bytes", PTR),
        ("stdin", HANDLE),
        ("stdout", HANDLE),
        ("stderr", HANDLE),
    ]


class _StartupInfoEx(c.Structure):
    _fields_ = [("startup", _StartupInfo), ("attributes", PTR)]


class _ProcessInformation(c.Structure):
    _fields_ = [("process", HANDLE), ("thread", HANDLE), ("pid", DWORD), ("tid", DWORD)]


class _BasicLimits(c.Structure):
    _fields_ = [
        ("process_time", c.c_int64),
        ("job_time", c.c_int64),
        ("flags", DWORD),
        ("min_working_set", SIZE_T),
        ("max_working_set", SIZE_T),
        ("active_processes", DWORD),
        ("affinity", SIZE_T),
        ("priority", DWORD),
        ("scheduling", DWORD),
    ]


class _IoCounters(c.Structure):
    _fields_ = [(name, c.c_uint64) for name in ("ro", "wo", "oo", "rt", "wt", "ot")]


class _ExtendedLimits(c.Structure):
    _fields_ = [
        ("basic", _BasicLimits),
        ("io", _IoCounters),
        ("process_memory", SIZE_T),
        ("job_memory", SIZE_T),
        ("peak_process_memory", SIZE_T),
        ("peak_job_memory", SIZE_T),
    ]


class _CpuRate(c.Structure):
    _fields_ = [("flags", DWORD), ("rate", DWORD)]


class _Trustee(c.Structure):
    _fields_ = [
        ("multiple", PTR),
        ("operation", DWORD),
        ("form", DWORD),
        ("kind", DWORD),
        ("name", PTR),
    ]


class _ExplicitAccess(c.Structure):
    _fields_ = [
        ("permissions", DWORD),
        ("mode", DWORD),
        ("inheritance", DWORD),
        ("trustee", _Trustee),
    ]


class _Win32:
    def __init__(self) -> None:
        if os.name != "nt":
            raise SandboxUnavailable("此隔离运行器需要 Windows 10/11 的 LPAC 支持")
        self.kernel = c.WinDLL("kernel32", use_last_error=True)
        self.kernelbase = c.WinDLL("kernelbase", use_last_error=True)
        self.advapi = c.WinDLL("advapi32", use_last_error=True)
        self.userenv = c.WinDLL("userenv", use_last_error=True)
        self.ole32 = c.WinDLL("ole32", use_last_error=True)
        signatures = [
            (self.kernel, "CloseHandle", BOOL, [HANDLE]),
            (self.kernel, "LocalFree", PTR, [PTR]),
            (self.ole32, "CoTaskMemFree", None, [PTR]),
            (self.kernel, "CreateJobObjectW", HANDLE, [PTR, c.c_wchar_p]),
            (self.kernel, "SetInformationJobObject", BOOL, [HANDLE, DWORD, PTR, DWORD]),
            (self.kernel, "IsProcessInJob", BOOL, [HANDLE, HANDLE, c.POINTER(BOOL)]),
            (self.kernel, "TerminateJobObject", BOOL, [HANDLE, DWORD]),
            (self.kernel, "CreatePipe", BOOL, [c.POINTER(HANDLE), c.POINTER(HANDLE), PTR, DWORD]),
            (self.kernel, "SetHandleInformation", BOOL, [HANDLE, DWORD, DWORD]),
            (self.kernel, "InitializeProcThreadAttributeList", BOOL, [PTR, DWORD, DWORD, PTR]),
            (
                self.kernel,
                "UpdateProcThreadAttribute",
                BOOL,
                [PTR, DWORD, SIZE_T, PTR, SIZE_T, PTR, PTR],
            ),
            (self.kernel, "DeleteProcThreadAttributeList", None, [PTR]),
            (
                self.kernel,
                "CreateProcessW",
                BOOL,
                [c.c_wchar_p, c.c_wchar_p, PTR, PTR, BOOL, DWORD, PTR, c.c_wchar_p, PTR, PTR],
            ),
            (self.kernel, "ResumeThread", DWORD, [HANDLE]),
            (self.kernel, "WaitForSingleObject", DWORD, [HANDLE, DWORD]),
            (self.kernel, "GetExitCodeProcess", BOOL, [HANDLE, c.POINTER(DWORD)]),
            (
                self.kernel,
                "CreateFileW",
                HANDLE,
                [c.c_wchar_p, DWORD, DWORD, PTR, DWORD, DWORD, HANDLE],
            ),
            (
                self.kernelbase,
                "DeriveCapabilitySidsFromName",
                BOOL,
                [c.c_wchar_p, PTR, PTR, PTR, PTR],
            ),
            (self.advapi, "OpenProcessToken", BOOL, [HANDLE, DWORD, c.POINTER(HANDLE)]),
            (self.advapi, "OpenThreadToken", BOOL, [HANDLE, DWORD, BOOL, PTR]),
            (self.advapi, "ImpersonateLoggedOnUser", BOOL, [HANDLE]),
            (self.advapi, "SetThreadToken", BOOL, [PTR, HANDLE]),
            (self.advapi, "RevertToSelf", BOOL, []),
            (self.advapi, "GetTokenInformation", BOOL, [HANDLE, DWORD, PTR, DWORD, PTR]),
            (self.advapi, "EqualSid", BOOL, [PTR, PTR]),
            (self.advapi, "GetLengthSid", DWORD, [PTR]),
            (self.advapi, "ConvertSidToStringSidW", BOOL, [PTR, c.POINTER(PTR)]),
            (self.advapi, "ConvertStringSidToSidW", BOOL, [c.c_wchar_p, PTR]),
            (self.advapi, "FreeSid", PTR, [PTR]),
            (
                self.advapi,
                "GetNamedSecurityInfoW",
                DWORD,
                [c.c_wchar_p, DWORD, DWORD, PTR, PTR, PTR, PTR, PTR],
            ),
            (self.advapi, "SetEntriesInAclW", DWORD, [DWORD, PTR, PTR, PTR]),
            (self.advapi, "SetSecurityInfo", DWORD, [HANDLE, DWORD, DWORD, PTR, PTR, PTR, PTR]),
            (
                self.advapi,
                "SetNamedSecurityInfoW",
                DWORD,
                [c.c_wchar_p, DWORD, DWORD, PTR, PTR, PTR, PTR],
            ),
            (
                self.advapi,
                "ConvertStringSecurityDescriptorToSecurityDescriptorW",
                BOOL,
                [c.c_wchar_p, DWORD, PTR, PTR],
            ),
            (self.advapi, "GetSecurityDescriptorSacl", BOOL, [PTR, PTR, PTR, PTR]),
            (
                self.userenv,
                "CreateAppContainerProfile",
                c.c_int32,
                [c.c_wchar_p, c.c_wchar_p, c.c_wchar_p, PTR, DWORD, PTR],
            ),
            (self.userenv, "DeleteAppContainerProfile", c.c_int32, [c.c_wchar_p]),
            (self.userenv, "GetAppContainerFolderPath", c.c_int32, [c.c_wchar_p, PTR]),
            (self.userenv, "GetAppContainerRegistryLocation", c.c_int32, [DWORD, PTR]),
        ]
        for dll, name, result, args in signatures:
            function = getattr(dll, name)
            function.restype = result
            function.argtypes = args

    @staticmethod
    def check(value: object, operation: str) -> None:
        if not value:
            error = c.get_last_error()
            raise SandboxUnavailable(error, f"{operation}: {c.FormatError(error).strip()}")

    @staticmethod
    def check_code(code: int, operation: str) -> None:
        if code:
            raise SandboxUnavailable(code, f"{operation}: {c.FormatError(code).strip()}")

    def close(self, handle: int | None) -> None:
        if handle:
            self.kernel.CloseHandle(handle)


def _plain_path(path: Path) -> Path:
    """Reject reparse points before resolving, including ancestors of the stage."""
    absolute = Path(os.path.abspath(path))
    for ancestor in (*reversed(absolute.parents), absolute):
        details = ancestor.lstat()
        if getattr(details, "st_file_attributes", 0) & stat.FILE_ATTRIBUTE_REPARSE_POINT:
            raise ValueError(f"隔离目录不得包含重解析点: {ancestor}")
    return absolute


def cleanup_stale_profile(run_dir: Path) -> bool:
    """Delete only a recorded, randomly named profile created by this module.

    Call after the previous broker/worker has exited. A failure preserves the
    marker so startup can retry; a malformed marker is never acted upon.
    """
    marker = Path(run_dir) / PROFILE_MARKER
    if not marker.exists():
        return False
    _plain_path(marker)
    if marker.stat().st_size > 256:
        raise ValueError("隔离 profile 清理标记过大")
    value = json.loads(marker.read_text(encoding="utf-8"))
    if not isinstance(value, dict) or set(value) != {"profile"}:
        raise ValueError("隔离 profile 清理标记无效")
    name = value["profile"]
    if not isinstance(name, str) or not _PROFILE_PATTERN.fullmatch(name):
        raise ValueError("拒绝清理不属于课程运行器的 AppContainer profile")
    api = _Win32()
    api.check_code(api.userenv.DeleteAppContainerProfile(name), "DeleteAppContainerProfile")
    marker.unlink()
    return True


class SandboxProcess:
    """Own a Job, process handle, and three binary parent-side pipe streams."""

    def __init__(
        self,
        api: _Win32,
        process: int,
        job: int,
        pid: int,
        streams: tuple[BinaryIO, BinaryIO, BinaryIO],
    ) -> None:
        self._api = api
        self._process = process
        self._job = job
        self.pid = pid
        self.stdin, self.stdout, self.stderr = streams
        self.returncode: int | None = None
        self._closed = False

    def poll(self) -> int | None:
        if self.returncode is not None:
            return self.returncode
        if self._closed:
            raise ValueError("隔离进程句柄已关闭")
        result = self._api.kernel.WaitForSingleObject(self._process, 0)
        if result == 258:  # WAIT_TIMEOUT; STILL_ACTIVE is also a valid exit code.
            return None
        if result != 0:
            self._api.check(False, "WaitForSingleObject")
        exit_code = DWORD()
        self._api.check(
            self._api.kernel.GetExitCodeProcess(self._process, c.byref(exit_code)),
            "GetExitCodeProcess",
        )
        self.returncode = exit_code.value
        return self.returncode

    def wait(self, timeout: float | None = None) -> int:
        if self.returncode is not None:
            return self.returncode
        if self._closed:
            raise ValueError("隔离进程句柄已关闭")
        if timeout is not None and (not math.isfinite(timeout) or timeout < 0):
            raise ValueError("timeout 必须是有限非负数")
        milliseconds = 0xFFFFFFFF if timeout is None else min(math.ceil(timeout * 1000), 0xFFFFFFFE)
        result = self._api.kernel.WaitForSingleObject(self._process, milliseconds)
        if result == 258:
            raise subprocess.TimeoutExpired(f"sandbox worker {self.pid}", timeout)
        if result != 0:
            self._api.check(False, "WaitForSingleObject")
        code = self.poll()
        assert code is not None
        return code

    def terminate(self) -> None:
        if not self._closed and self.poll() is None:
            self._api.check(self._api.kernel.TerminateJobObject(self._job, 1), "TerminateJobObject")

    def close(self) -> None:
        if self._closed:
            return
        try:
            self.terminate()
            self.wait(timeout=10)
        finally:
            # Closing the noninheritable final Job handle also kills a worker if
            # explicit termination failed. Unbuffered streams cannot flush here.
            self._api.close(self._job)
            self._job = None
            for stream in (self.stdin, self.stdout, self.stderr):
                stream.close()
            self._api.close(self._process)
            self._process = None
            self._closed = True

    def __enter__(self) -> SandboxProcess:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()


class AppContainerProfile:
    """One temporary profile and one worker rooted in a fresh staging directory."""

    def __init__(self, stage_root: Path) -> None:
        self._api = _Win32()
        self.stage_root = _plain_path(Path(stage_root))
        if not self.stage_root.is_dir() or self.stage_root.parent == self.stage_root:
            raise ValueError("stage_root 必须是专用临时目录")
        self.name = PROFILE_PREFIX + uuid.uuid4().hex
        self.sid = ""
        self._sid = PTR()
        self._entered = False
        self._used = False
        self._launched = False
        self._process: SandboxProcess | None = None
        self._capability_buffers: list[object] = []
        self._capabilities = None
        self._restrict_storage_requested = False

    def _derive_capabilities(self) -> None:
        api = self._api
        for name in _BASE_CAPABILITIES:
            groups, capabilities = PTR(), PTR()
            group_count, capability_count = DWORD(), DWORD()
            try:
                api.check(
                    api.kernelbase.DeriveCapabilitySidsFromName(
                        name,
                        c.byref(groups),
                        c.byref(group_count),
                        c.byref(capabilities),
                        c.byref(capability_count),
                    ),
                    "DeriveCapabilitySidsFromName(registryRead)",
                )
                if capability_count.value != 1:
                    raise SandboxUnavailable(
                        "系统返回的 registryRead capability 不符合固定允许列表"
                    )
                source = c.cast(capabilities, c.POINTER(PTR))[0]
                size = api.advapi.GetLengthSid(source)
                if not 8 <= size <= 68:
                    raise SandboxUnavailable("系统返回了无效的 capability SID")
                copied = c.create_string_buffer(size)
                c.memmove(copied, source, size)
                self._capability_buffers.append(copied)
            finally:
                for pointer, count in ((groups, group_count), (capabilities, capability_count)):
                    if pointer:
                        for index in range(count.value):
                            api.kernel.LocalFree(c.cast(pointer, c.POINTER(PTR))[index])
                        api.kernel.LocalFree(pointer)
        self._capabilities = (_SidAndAttributes * len(self._capability_buffers))(
            *(_SidAndAttributes(c.cast(buffer, PTR), 4) for buffer in self._capability_buffers)
        )

    def __enter__(self) -> AppContainerProfile:
        if self._used:
            raise RuntimeError("AppContainerProfile 不可重复进入")
        self._used = True
        marker = self.stage_root.parent / PROFILE_MARKER
        with marker.open("x", encoding="utf-8") as stream:
            json.dump({"profile": self.name}, stream)
            stream.flush()
            os.fsync(stream.fileno())
        try:
            self._derive_capabilities()
            self._api.check_code(
                self._api.userenv.CreateAppContainerProfile(
                    self.name,
                    "墨页课程临时运行器",
                    "一次性隔离 Python worker",
                    self._capabilities,
                    len(self._capabilities),
                    c.byref(self._sid),
                ),
                "CreateAppContainerProfile",
            )
            text_sid = PTR()
            self._api.check(
                self._api.advapi.ConvertSidToStringSidW(self._sid, c.byref(text_sid)),
                "ConvertSidToStringSidW",
            )
            try:
                self.sid = c.wstring_at(text_sid)
            finally:
                self._api.kernel.LocalFree(text_sid)
            self._entered = True
            return self
        except BaseException:
            if self._sid:
                self._api.advapi.FreeSid(self._sid)
                self._sid = PTR()
            cleanup_stale_profile(self.stage_root.parent)
            raise

    def _stage_path(self, path: Path) -> Path:
        if not self._entered:
            raise RuntimeError("必须先进入 AppContainerProfile context")
        checked = _plain_path(Path(path))
        if not checked.is_relative_to(self.stage_root):
            raise ValueError("拒绝修改或使用临时 stage 之外的路径")
        return checked

    def grant_access(self, path: Path, writable: bool = False) -> None:
        if self._launched:
            raise RuntimeError("worker 创建后不能再修改隔离目录权限")
        target = self._stage_path(path)
        if target.is_file() and target.stat().st_nlink != 1:
            raise ValueError("隔离 stage 内不得包含硬链接")
        if target.is_dir():
            for directory, directories, files in os.walk(target, followlinks=False):
                for name in (*directories, *files):
                    details = (Path(directory) / name).lstat()
                    if (
                        getattr(details, "st_file_attributes", 0)
                        & stat.FILE_ATTRIBUTE_REPARSE_POINT
                    ):
                        raise ValueError("隔离 stage 内不得包含重解析点")
                    if stat.S_ISREG(details.st_mode) and details.st_nlink != 1:
                        raise ValueError("隔离 stage 内不得包含硬链接")
        permissions = 0x1200A9  # FILE_GENERIC_READ | FILE_GENERIC_EXECUTE.
        if writable:
            permissions |= 0x120116 | 0x10000  # write and delete, never WRITE_DAC.
        self._set_acl(target, permissions, mode=1)  # GRANT_ACCESS
        if writable:
            self._low_integrity(target)

    def restrict_profile_storage(self) -> Path:
        """Deny this SID filesystem writes to its own newly created profile.

        This is deliberately separate from grant_access: it touches only the
        profile just created by this context, never another user directory.
        The final directories and per-app registry are sealed again after
        process creation, before its suspended initial thread is resumed.
        This is a write-access policy, not a filesystem/registry disk quota.
        """
        if not self._entered or self._launched:
            raise RuntimeError("profile 存储限制只能在创建后、启动 worker 前设置")
        self._restrict_storage_requested = True
        return self._restrict_profile_files()

    def _restrict_profile_files(self) -> Path:
        raw_path = PTR()
        api = self._api
        api.check_code(
            api.userenv.GetAppContainerFolderPath(self.sid, c.byref(raw_path)),
            "GetAppContainerFolderPath",
        )
        try:
            folder = _plain_path(Path(c.wstring_at(raw_path)))
        finally:
            api.ole32.CoTaskMemFree(raw_path)
        # API results still must name precisely this context's unique profile.
        names = [part.casefold() for part in folder.parts]
        if self.name.casefold() not in names:
            raise SandboxUnavailable("系统返回的 profile 路径与本次创建的名称不符")
        root = Path(*folder.parts[: names.index(self.name.casefold()) + 1])
        _plain_path(root)
        # AppContainer profile children can have protected DACLs. Updating just
        # the package root does not propagate into these existing directories.
        # Validate the fresh profile tree, then replace every existing object's
        # DACL with a protected, inheritable read-only package allowlist.
        targets = [root]

        def fail_walk(error: OSError) -> None:
            raise error

        for directory, directories, files in os.walk(root, followlinks=False, onerror=fail_walk):
            for name in (*directories, *files):
                target = Path(directory) / name
                details = target.lstat()
                if getattr(details, "st_file_attributes", 0) & stat.FILE_ATTRIBUTE_REPARSE_POINT:
                    raise SandboxUnavailable("本次 profile 包含意外重解析点，拒绝修改权限")
                if stat.S_ISREG(details.st_mode) and details.st_nlink != 1:
                    raise SandboxUnavailable("本次 profile 包含意外硬链接，拒绝修改权限")
                targets.append(target)
        for target in targets:
            self._set_profile_read_only(target)
        return folder

    def _set_profile_read_only(self, target: Path) -> None:
        new_acl = self._new_profile_acl(0x1200A9, 0x1F01FF, 3 if target.is_dir() else 0)
        try:
            self._api.check_code(
                self._api.advapi.SetNamedSecurityInfoW(
                    str(target), 1, 0x80000004, None, None, new_acl, None
                ),
                "SetNamedSecurityInfoW(protected profile)",
            )
        finally:
            self._api.kernel.LocalFree(new_acl)

    def _new_profile_acl(
        self,
        package_rights: int,
        host_rights: int,
        inheritance: int,
        package_sid: PTR | None = None,
    ) -> PTR:
        api = self._api
        host_token, system_sid, new_acl = HANDLE(), PTR(), PTR()
        api.check(
            api.advapi.OpenProcessToken(HANDLE(-1), 8, c.byref(host_token)),
            "OpenProcessToken(host)",
        )
        try:
            user = c.create_string_buffer(256)
            size = DWORD()
            api.check(
                api.advapi.GetTokenInformation(host_token, 1, user, len(user), c.byref(size)),
                "GetTokenInformation(host user)",
            )
            api.check(
                api.advapi.ConvertStringSidToSidW("S-1-5-18", c.byref(system_sid)),
                "ConvertStringSidToSidW(SYSTEM)",
            )
            entries = (_ExplicitAccess * 3)()
            for entry, sid, rights in zip(
                entries,
                (
                    PTR.from_buffer(user),
                    system_sid,
                    self._sid if package_sid is None else package_sid,
                ),
                (host_rights, host_rights, package_rights),
                strict=True,
            ):
                entry.permissions = rights
                entry.mode = 1
                entry.inheritance = inheritance
                entry.trustee.form = 0
                entry.trustee.name = sid
            api.check_code(
                api.advapi.SetEntriesInAclW(3, entries, None, c.byref(new_acl)),
                "SetEntriesInAclW(profile allowlist)",
            )
            result, new_acl = new_acl, PTR()
            return result
        finally:
            if new_acl:
                api.kernel.LocalFree(new_acl)
            if system_sid:
                api.kernel.LocalFree(system_sid)
            api.close(host_token.value)

    @contextmanager
    def _worker_identity(self, token: HANDLE):
        api = self._api
        previous = HANDLE()
        try:
            if not api.advapi.OpenThreadToken(HANDLE(-2), 8 | 4, True, c.byref(previous)):
                if c.get_last_error() != 1008:
                    api.check(False, "OpenThreadToken")
            api.check(
                api.advapi.ImpersonateLoggedOnUser(token),
                "ImpersonateLoggedOnUser(verified worker)",
            )
            try:
                yield
            finally:
                restored = (
                    api.advapi.SetThreadToken(None, previous)
                    if previous.value
                    else api.advapi.RevertToSelf()
                )
                api.check(restored, "Restore broker thread token")
        finally:
            api.close(previous.value)

    def _restrict_profile_registry(self, process: int) -> None:
        """Use the verified worker identity to locate only its own registry store."""
        import winreg

        api = self._api
        token, root_key = HANDLE(), HANDLE()
        api.check(
            api.advapi.OpenProcessToken(process, 8 | 2, c.byref(token)),
            "OpenProcessToken(registry scope)",
        )
        try:
            with self._worker_identity(token):
                status = api.userenv.GetAppContainerRegistryLocation(
                    winreg.KEY_READ | 0x40000, c.byref(root_key)
                )
            api.check_code(status, "GetAppContainerRegistryLocation(verified worker)")
            if not root_key.value or 0x80000000 <= (root_key.value & 0xFFFFFFFF) <= 0x80000060:
                raise SandboxUnavailable("拒绝使用非专属 profile registry 句柄")

            def seal(key: int, depth: int = 0) -> None:
                if depth > 32:
                    raise SandboxUnavailable("本次 profile registry 结构过深")
                # Only fresh profile keys exist here: untrusted code remains
                # suspended. Detect registry links before following children.
                try:
                    _, kind = winreg.QueryValueEx(key, "SymbolicLinkValue")
                    if kind == winreg.REG_LINK:
                        raise SandboxUnavailable("本次 profile registry 包含意外符号链接")
                except FileNotFoundError:
                    pass
                names = []
                for index in range(4097):
                    try:
                        names.append(winreg.EnumKey(key, index))
                    except OSError as error:
                        if error.winerror == 259:
                            break
                        raise
                else:
                    raise SandboxUnavailable("本次 profile registry 子项过多")
                for name in names:
                    # REG_OPTION_OPEN_LINK prevents following a symbolic link.
                    with winreg.OpenKey(key, name, 8, winreg.KEY_READ | 0x40000) as child:
                        seal(int(child), depth + 1)
                acl = self._new_profile_acl(winreg.KEY_READ, winreg.KEY_ALL_ACCESS, 2)
                try:
                    api.check_code(
                        api.advapi.SetSecurityInfo(key, 4, 0x80000004, None, None, acl, None),
                        "SetSecurityInfo(protected profile registry)",
                    )
                finally:
                    api.kernel.LocalFree(acl)

            seal(root_key.value)
        finally:
            if root_key.value:
                winreg.CloseKey(root_key.value)
            api.close(token.value)

    def _set_acl(self, target: Path, permissions: int, *, mode: int) -> None:
        descriptor, old_acl, new_acl = PTR(), PTR(), PTR()
        api = self._api
        try:
            api.check_code(
                api.advapi.GetNamedSecurityInfoW(
                    str(target), 1, 4, None, None, c.byref(old_acl), None, c.byref(descriptor)
                ),
                "GetNamedSecurityInfoW",
            )
            access = _ExplicitAccess()
            access.permissions = permissions
            access.mode = mode
            access.inheritance = 3 if target.is_dir() else 0
            access.trustee.form = 0  # TRUSTEE_IS_SID
            access.trustee.kind = 5  # TRUSTEE_IS_WELL_KNOWN_GROUP
            access.trustee.name = self._sid
            api.check_code(
                api.advapi.SetEntriesInAclW(1, c.byref(access), old_acl, c.byref(new_acl)),
                "SetEntriesInAclW",
            )
            api.check_code(
                api.advapi.SetNamedSecurityInfoW(str(target), 1, 4, None, None, new_acl, None),
                "SetNamedSecurityInfoW(DACL)",
            )
        finally:
            if new_acl:
                api.kernel.LocalFree(new_acl)
            if descriptor:
                api.kernel.LocalFree(descriptor)

    def _low_integrity(self, path: Path) -> None:
        descriptor, sacl = PTR(), PTR()
        present, defaulted = BOOL(), BOOL()
        api = self._api
        flags = "OICI" if path.is_dir() else ""
        api.check(
            api.advapi.ConvertStringSecurityDescriptorToSecurityDescriptorW(
                f"S:(ML;{flags};NW;;;LW)", 1, c.byref(descriptor), None
            ),
            "ConvertStringSecurityDescriptorToSecurityDescriptorW",
        )
        try:
            api.check(
                api.advapi.GetSecurityDescriptorSacl(
                    descriptor, c.byref(present), c.byref(sacl), c.byref(defaulted)
                ),
                "GetSecurityDescriptorSacl",
            )
            api.check_code(
                api.advapi.SetNamedSecurityInfoW(str(path), 1, 0x10, None, None, None, sacl),
                "SetNamedSecurityInfoW(Low integrity)",
            )
        finally:
            api.kernel.LocalFree(descriptor)

    def _verify_token(self, process: int) -> None:
        token = HANDLE()
        api = self._api
        api.check(api.advapi.OpenProcessToken(process, 8 | 2, c.byref(token)), "OpenProcessToken")
        try:
            for information, expected in ((29, 1), (30, len(self._capabilities))):
                # TOKEN_GROUPS (capabilities) is variable sized, even when empty.
                buffer = c.create_string_buffer(256 if information == 30 else c.sizeof(DWORD))
                size = DWORD()
                api.check(
                    api.advapi.GetTokenInformation(
                        token, information, buffer, len(buffer), c.byref(size)
                    ),
                    f"GetTokenInformation({information})",
                )
                if DWORD.from_buffer(buffer).value != expected:
                    raise SandboxUnavailable("worker token 的 AppContainer/capability 状态不符")
                if information == 30:
                    actual = (_SidAndAttributes * expected).from_buffer(
                        buffer, _TokenGroups.groups.offset
                    )
                    remaining = list(self._capabilities)
                    for capability in actual:
                        match = next(
                            (
                                item
                                for item in remaining
                                if api.advapi.EqualSid(capability.sid, item.sid)
                            ),
                            None,
                        )
                        if match is None or not capability.attributes & 4:
                            raise SandboxUnavailable(
                                "worker token 含不在固定允许列表中的 capability"
                            )
                        remaining.remove(match)
            # The token information includes a pointer plus the copied SID.
            buffer = c.create_string_buffer(256)
            size = DWORD()
            api.check(
                api.advapi.GetTokenInformation(token, 31, buffer, len(buffer), c.byref(size)),
                "GetTokenInformation(TokenAppContainerSid)",
            )
            if not api.advapi.EqualSid(PTR.from_buffer(buffer), self._sid):
                raise SandboxUnavailable("worker AppContainer SID 与本次 profile 不符")
            self._verify_lpac(token)
        finally:
            api.close(token.value)

    def _verify_lpac(self, token: HANDLE) -> None:
        # TokenInformationClass 46 is rejected by some current Windows builds,
        # and CheckTokenMembershipEx does not distinguish the implicit AAP grant.
        # Test actual kernel access to a host-created AAP-only file instead.
        # A regular AC can read it; an LPAC must receive ERROR_ACCESS_DENIED.
        api = self._api
        all_packages, acl = PTR(), PTR()
        probe = self.stage_root.parent / f"lpac-access-probe-{uuid.uuid4().hex}.dat"
        with probe.open("xb") as stream:
            stream.write(b"ngy LPAC access probe")
        try:
            api.check(
                api.advapi.ConvertStringSidToSidW("S-1-15-2-1", c.byref(all_packages)),
                "ConvertStringSidToSidW(ALL_APPLICATION_PACKAGES)",
            )
            acl = self._new_profile_acl(0x120089, 0x1F01FF, 0, all_packages)
            api.check_code(
                api.advapi.SetNamedSecurityInfoW(str(probe), 1, 0x80000004, None, None, acl, None),
                "SetNamedSecurityInfoW(LPAC probe)",
            )
            with self._worker_identity(token):
                handle = api.kernel.CreateFileW(str(probe), 0x80000000, 7, None, 3, 0x80, None)
                error = c.get_last_error()
                opened = handle != HANDLE(-1).value
                if opened:
                    api.close(handle)
            if opened:
                raise SandboxUnavailable(
                    "worker 仍可使用 ALL_APPLICATION_PACKAGES 文件权限，拒绝普通 AppContainer"
                )
            if error != 5:
                api.check_code(error or 1, "LPAC probe did not receive ACCESS_DENIED")
        finally:
            if acl:
                api.kernel.LocalFree(acl)
            if all_packages:
                api.kernel.LocalFree(all_packages)
            probe.unlink()

    def launch(
        self,
        executable: Path,
        args: list[str],
        *,
        cwd: Path,
        env: dict[str, str],
        memory_bytes: int = 512 * 1024 * 1024,
        cpu_time_seconds: float = 30,
        cpu_rate_percent: int = 50,
    ) -> SandboxProcess:
        import msvcrt

        if self._launched:
            raise RuntimeError("每个 AppContainerProfile 只能启动一个 worker")
        executable = self._stage_path(executable)
        cwd = self._stage_path(cwd)
        if not executable.is_file() or not cwd.is_dir():
            raise ValueError("executable/cwd 类型无效")
        if not isinstance(memory_bytes, int) or not 0 < memory_bytes <= 2**40:
            raise ValueError("memory_bytes 必须是 1..2**40 的整数")
        if not math.isfinite(cpu_time_seconds) or not 0 < cpu_time_seconds <= 86400:
            raise ValueError("cpu_time_seconds 必须大于 0 且不超过一天")
        if not isinstance(cpu_rate_percent, int) or not 1 <= cpu_rate_percent <= 100:
            raise ValueError("cpu_rate_percent 必须是 1..100 的整数")
        if any(not isinstance(arg, str) or "\0" in arg for arg in args):
            raise ValueError("进程参数必须是无 NUL 字符串")
        if any(
            not isinstance(key, str)
            or not isinstance(value, str)
            or not key
            or "=" in key
            or "\0" in key + value
            for key, value in env.items()
        ):
            raise ValueError("进程环境变量无效")
        environment_values = {key.upper(): value for key, value in env.items()}
        if len(environment_values) != len(env):
            raise ValueError("进程环境变量名称不得仅大小写不同")
        # Windows' AppContainer environment construction expects these keys.
        # Missing profile keys can make CreateProcessW fail with error 203.
        # Supply only staging paths, never inherit the broker's user profile.
        for key in ("LOCALAPPDATA", "APPDATA", "USERPROFILE"):
            environment_values.setdefault(key, str(cwd))
        self._launched = True
        api = self._api
        job = api.kernel.CreateJobObjectW(None, None)
        api.check(job, "CreateJobObjectW")
        process_info = _ProcessInformation()
        handles: set[int] = set()
        streams: list[BinaryIO] = []
        attributes = None
        attribute_initialized = False
        try:
            limits = _ExtendedLimits()
            # PROCESS_TIME | ACTIVE_PROCESS | PROCESS_MEMORY | KILL_ON_JOB_CLOSE.
            limits.basic.flags = 0x2 | 0x8 | 0x100 | 0x2000
            limits.basic.process_time = max(1, math.ceil(cpu_time_seconds * 10_000_000))
            limits.basic.active_processes = 1
            limits.process_memory = memory_bytes
            api.check(
                api.kernel.SetInformationJobObject(job, 9, c.byref(limits), c.sizeof(limits)),
                "Job limits",
            )
            rate = _CpuRate(0x1 | 0x4, cpu_rate_percent * 100)
            api.check(
                api.kernel.SetInformationJobObject(job, 15, c.byref(rate), c.sizeof(rate)),
                "Job CPU hard cap",
            )
            parent_handles, child_handles = [], []
            for child_reads in (True, False, False):
                read, write = HANDLE(), HANDLE()
                security = _SecurityAttributes(c.sizeof(_SecurityAttributes), None, True)
                api.check(
                    api.kernel.CreatePipe(c.byref(read), c.byref(write), c.byref(security), 0),
                    "CreatePipe",
                )
                handles.update((read.value, write.value))
                parent, child = (
                    (write.value, read.value) if child_reads else (read.value, write.value)
                )
                api.check(api.kernel.SetHandleInformation(parent, 1, 0), "SetHandleInformation")
                parent_handles.append(parent)
                child_handles.append(child)
            size = SIZE_T()
            api.kernel.InitializeProcThreadAttributeList(None, 5, 0, c.byref(size))
            if c.get_last_error() != 122 or not size.value:
                api.check(False, "InitializeProcThreadAttributeList(size)")
            attributes = c.create_string_buffer(size.value)
            api.check(
                api.kernel.InitializeProcThreadAttributeList(attributes, 5, 0, c.byref(size)),
                "InitializeProcThreadAttributeList",
            )
            attribute_initialized = True
            capabilities = _SecurityCapabilities(
                self._sid, c.cast(self._capabilities, PTR), len(self._capabilities), 0
            )
            handle_list = (HANDLE * 3)(*child_handles)
            job_list = (HANDLE * 1)(job)
            lpac, child_policy = DWORD(_LPAC_POLICY), DWORD(1)
            for key, value in (
                (0x20009, capabilities),
                (0x20002, handle_list),
                (0x2000D, job_list),
                (0x2000F, lpac),
                (0x2000E, child_policy),
            ):
                api.check(
                    api.kernel.UpdateProcThreadAttribute(
                        attributes, 0, key, c.byref(value), c.sizeof(value), None, None
                    ),
                    f"UpdateProcThreadAttribute({key:#x})",
                )
            startup = _StartupInfoEx()
            startup.startup.cb = c.sizeof(startup)
            startup.startup.flags = 0x100  # STARTF_USESTDHANDLES
            startup.startup.stdin, startup.startup.stdout, startup.startup.stderr = child_handles
            startup.attributes = c.cast(attributes, PTR)
            command = c.create_unicode_buffer(subprocess.list2cmdline([str(executable), *args]))
            environment = c.create_unicode_buffer(
                "\0".join(f"{key}={value}" for key, value in sorted(environment_values.items()))
                + "\0\0"
            )
            # SUSPENDED | DETACHED_PROCESS | UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO.
            # The worker communicates only over explicitly inherited pipes.
            flags = 0x4 | 0x8 | 0x400 | 0x80000
            api.check(
                api.kernel.CreateProcessW(
                    str(executable),
                    command,
                    None,
                    None,
                    True,
                    flags,
                    environment,
                    str(cwd),
                    c.byref(startup),
                    c.byref(process_info),
                ),
                "CreateProcessW(LPAC)",
            )
            for handle in child_handles:
                api.close(handle)
                handles.remove(handle)
            self._verify_token(process_info.process)
            in_job = BOOL()
            api.check(
                api.kernel.IsProcessInJob(process_info.process, job, c.byref(in_job)),
                "IsProcessInJob",
            )
            if not in_job.value:
                raise SandboxUnavailable("worker 未加入受限 Job")
            if self._restrict_storage_requested:
                # Process creation can populate/reinitialize per-app storage.
                # Seal its final ACLs while the initial thread is suspended.
                self._restrict_profile_files()
                self._restrict_profile_registry(process_info.process)
            for index, handle in enumerate(parent_handles):
                flags = os.O_BINARY | (os.O_WRONLY if index == 0 else os.O_RDONLY)
                fd = msvcrt.open_osfhandle(handle, flags)
                handles.remove(handle)  # fd now owns this Windows handle.
                try:
                    stream = os.fdopen(fd, "wb" if index == 0 else "rb", buffering=0)
                except BaseException:
                    os.close(fd)
                    raise
                streams.append(stream)
            if api.kernel.ResumeThread(process_info.thread) == 0xFFFFFFFF:
                api.check(False, "ResumeThread")
            api.close(process_info.thread)
            process_info.thread = None
            result = SandboxProcess(
                api, process_info.process, job, process_info.pid, tuple(streams)
            )
            self._process = result
            return result
        except BaseException:
            # Job assignment happened inside CreateProcess: even startup failure
            # cannot leave an unconfined running interpreter behind.
            api.kernel.TerminateJobObject(job, 1)
            if process_info.process:
                api.kernel.WaitForSingleObject(process_info.process, 10000)
            api.close(process_info.thread)
            api.close(process_info.process)
            api.close(job)
            for stream in streams:
                stream.close()
            raise
        finally:
            for handle in handles:
                api.close(handle)
            if attribute_initialized:
                api.kernel.DeleteProcThreadAttributeList(attributes)

    def close(self) -> None:
        if not self._entered:
            return
        if self._process is not None:
            self._process.close()
        cleanup_stale_profile(self.stage_root.parent)
        self._api.advapi.FreeSid(self._sid)
        self._sid = PTR()
        self._entered = False

    def __exit__(self, *_: object) -> None:
        self.close()
