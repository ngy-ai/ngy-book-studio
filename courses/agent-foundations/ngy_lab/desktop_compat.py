"""Worker-only lazy initialization for CPython 3.12's optional IOCP extension.

Importing asyncio imports _overlapped even for synchronous consumers. That
native module opens a Winsock socket while initializing and cannot initialize
inside a network-denied LPAC. Defer its real initialization until an IOCP
attribute is actually used. No native operation or constant is emulated, and
the installed interpreter and third-party packages remain unchanged.
"""

import importlib
import importlib.util
import sys
import threading
from types import ModuleType

COMPATIBILITY_ID = "cpython312_deferred_overlapped_v1"


class _DeferredOverlapped(ModuleType):
    def __init__(self):
        super().__init__("_overlapped")
        self.__spec__ = importlib.util.find_spec("_overlapped")
        if self.__spec__ is None:
            raise ImportError("CPython 运行时缺少 _overlapped 扩展")
        self.__loader__ = self.__spec__.loader
        self.__file__ = self.__spec__.origin
        self.__package__ = self.__spec__.parent
        self._load_lock = threading.RLock()
        self._native = None

    def __getattr__(self, name):
        if name.startswith("__") and name.endswith("__"):
            raise AttributeError(name)
        with self._load_lock:
            if self._native is None:
                if sys.modules.get(self.__name__) is self:
                    del sys.modules[self.__name__]
                try:
                    self._native = importlib.import_module(self.__name__)
                except BaseException:
                    # Preserve retry behavior without swallowing the actual
                    # native initialization error, including LPAC access denial.
                    sys.modules[self.__name__] = self
                    raise
            return getattr(self._native, name)


def install_deferred_overlapped():
    """Apply only in the fresh Windows CPython 3.12 course worker."""
    if sys.platform != "win32" or sys.version_info[:2] != (3, 12):
        raise RuntimeError("桌面同步课程隔离兼容层要求 Windows CPython 3.12")
    if "_overlapped" not in sys.modules:
        sys.modules["_overlapped"] = _DeferredOverlapped()
