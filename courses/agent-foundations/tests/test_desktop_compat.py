"""The synchronous worker defers real IOCP initialization without emulating it."""

import importlib
import sys
from types import ModuleType

import pytest

from moye_lab.desktop_compat import _DeferredOverlapped

pytestmark = pytest.mark.skipif(sys.platform != "win32", reason="Windows CPython extension")


def test_native_extension_is_deferred_then_loaded_once(monkeypatch):
    native = ModuleType("_overlapped")
    native.fixture_operation = object()
    calls = []
    deferred = _DeferredOverlapped()
    monkeypatch.setitem(sys.modules, "_overlapped", deferred)

    def load(name):
        calls.append(name)
        assert name not in sys.modules
        sys.modules[name] = native
        return native

    monkeypatch.setattr(importlib, "import_module", load)
    assert deferred.__file__.endswith("_overlapped.pyd")
    assert deferred.__spec__.name == "_overlapped"
    assert calls == []
    assert deferred.fixture_operation is native.fixture_operation
    assert deferred.fixture_operation is native.fixture_operation
    assert calls == ["_overlapped"]


def test_native_access_denial_is_preserved_and_each_retry_reloads(monkeypatch):
    deferred = _DeferredOverlapped()
    monkeypatch.setitem(sys.modules, "_overlapped", deferred)
    calls = []
    denied = PermissionError(10013, "native fixture denial")

    def load(name):
        calls.append(name)
        assert name not in sys.modules
        raise denied

    monkeypatch.setattr(importlib, "import_module", load)
    for _ in range(2):
        with pytest.raises(PermissionError) as error:
            deferred.CreateIoCompletionPort
        assert error.value is denied
        assert sys.modules["_overlapped"] is deferred
    assert calls == ["_overlapped", "_overlapped"]
