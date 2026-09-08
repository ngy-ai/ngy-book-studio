"""Windows Credential Manager storage, isolated from the reader application's keys.

Callers supply a namespace-scoped target, never a key in an environment variable or file.
The CLI obtains new keys with getpass; this module does not prompt or print secrets.
"""

import ctypes
import hashlib
import os
import re
from ctypes import wintypes

from .contracts import LabError

NAMESPACE = "dev.moye.agent-foundations.endpoint-"
MAX_KEY_BYTES = 2560
_TARGET_PATTERN = re.compile(re.escape(NAMESPACE) + r"[0-9a-f]{64}\Z")


def target_for_endpoint(base_url: str) -> str:
    # Import lazily: live.py uses this module to resolve a key after URL validation.
    from .live import normalize_base_url

    endpoint = normalize_base_url(base_url)
    return NAMESPACE + hashlib.sha256(endpoint.encode("utf-8")).hexdigest()


def validate_target(target: str) -> None:
    if not isinstance(target, str) or not _TARGET_PATTERN.fullmatch(target):
        raise LabError("凭据目标必须属于本课程独立命名空间")


def validate_api_key(api_key: str) -> bytes:
    if not isinstance(api_key, str) or not api_key:
        raise LabError("API 密钥不能为空")
    if len(api_key) > MAX_KEY_BYTES or any(not 33 <= ord(c) <= 126 for c in api_key):
        raise LabError("API 密钥必须为不含空白的 ASCII 字符，且不超过 2560 字节")
    return api_key.encode("ascii")


class _CredentialAttribute(ctypes.Structure):
    _fields_ = [
        ("Keyword", wintypes.LPWSTR),
        ("Flags", wintypes.DWORD),
        ("ValueSize", wintypes.DWORD),
        ("Value", ctypes.POINTER(ctypes.c_ubyte)),
    ]


class _Credential(ctypes.Structure):
    _fields_ = [
        ("Flags", wintypes.DWORD),
        ("Type", wintypes.DWORD),
        ("TargetName", wintypes.LPWSTR),
        ("Comment", wintypes.LPWSTR),
        ("LastWritten", wintypes.FILETIME),
        ("CredentialBlobSize", wintypes.DWORD),
        ("CredentialBlob", ctypes.POINTER(ctypes.c_ubyte)),
        ("Persist", wintypes.DWORD),
        ("AttributeCount", wintypes.DWORD),
        ("Attributes", ctypes.POINTER(_CredentialAttribute)),
        ("TargetAlias", wintypes.LPWSTR),
        ("UserName", wintypes.LPWSTR),
    ]


def _native_api():
    if os.name != "nt":
        raise LabError("API 密钥存储仅支持 Windows Credential Manager")
    try:
        api = ctypes.WinDLL("Advapi32.dll", use_last_error=True)
        api.CredReadW.argtypes = [
            wintypes.LPCWSTR,
            wintypes.DWORD,
            wintypes.DWORD,
            ctypes.POINTER(ctypes.POINTER(_Credential)),
        ]
        api.CredReadW.restype = wintypes.BOOL
        api.CredWriteW.argtypes = [ctypes.POINTER(_Credential), wintypes.DWORD]
        api.CredWriteW.restype = wintypes.BOOL
        api.CredDeleteW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD]
        api.CredDeleteW.restype = wintypes.BOOL
        api.CredFree.argtypes = [ctypes.c_void_p]
        api.CredFree.restype = None
        return api
    except (OSError, AttributeError):
        raise LabError("无法打开 Windows Credential Manager") from None


def read_api_key(target: str) -> str | None:
    validate_target(target)
    api = _native_api()
    pointer = ctypes.POINTER(_Credential)()
    if not api.CredReadW(target, 1, 0, ctypes.byref(pointer)):
        if ctypes.get_last_error() == 1168:  # ERROR_NOT_FOUND
            return None
        raise LabError("读取课程 API 密钥失败，请检查 Windows 凭据权限")
    try:
        entry = pointer.contents
        if not 0 < entry.CredentialBlobSize <= MAX_KEY_BYTES or not entry.CredentialBlob:
            raise LabError("课程 API 密钥记录无效，请重新设置")
        try:
            key = ctypes.string_at(entry.CredentialBlob, entry.CredentialBlobSize).decode("ascii")
        except UnicodeError:
            raise LabError("课程 API 密钥记录无效，请重新设置") from None
        validate_api_key(key)
        return key
    finally:
        api.CredFree(pointer)


def write_api_key(target: str, api_key: str) -> None:
    validate_target(target)
    encoded = validate_api_key(api_key)
    api = _native_api()
    blob = (ctypes.c_ubyte * len(encoded)).from_buffer_copy(encoded)
    entry = _Credential(
        Type=1,  # CRED_TYPE_GENERIC
        TargetName=target,
        CredentialBlobSize=len(encoded),
        CredentialBlob=ctypes.cast(blob, ctypes.POINTER(ctypes.c_ubyte)),
        Persist=2,  # CRED_PERSIST_LOCAL_MACHINE: persist for this Windows user.
        UserName="moye-agent-foundations",
    )
    try:
        if not api.CredWriteW(ctypes.byref(entry), 0):
            raise LabError("保存课程 API 密钥失败，请检查 Windows 凭据权限")
    finally:
        ctypes.memset(blob, 0, len(encoded))


def delete_api_key(target: str) -> None:
    validate_target(target)
    api = _native_api()
    if not api.CredDeleteW(target, 1, 0) and ctypes.get_last_error() != 1168:
        raise LabError("删除课程 API 密钥失败，请检查 Windows 凭据权限")
