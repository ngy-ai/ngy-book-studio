"""The common boundary for the hand-written and LangGraph implementations.

Messages use the chat-completions tool-call shape. No provider SDK types cross this boundary.
The runner owns observed model/tool calls; `emit` is learner instrumentation, not grading evidence.
"""

import math
from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any, Protocol

Message = dict[str, Any]
Emit = Callable[[dict[str, Any]], None]


class LabError(Exception):
    """An actionable, credential-free developer lab error."""


class ProtocolError(LabError):
    pass


class BudgetExceeded(LabError):
    def __init__(self, reason: str):
        self.reason = reason
        super().__init__(reason)


@dataclass(frozen=True)
class RunLimits:
    max_model_decisions: int = 8
    max_tool_executions: int = 6
    max_retries: int = 1
    timeout_seconds: float = 180.0

    def __post_init__(self):
        if type(self.max_model_decisions) is not int or not 1 <= self.max_model_decisions <= 8:
            raise ValueError("模型决策预算必须为 1..8")
        if type(self.max_tool_executions) is not int or not 1 <= self.max_tool_executions <= 6:
            raise ValueError("工具执行预算必须为 1..6")
        if type(self.max_retries) is not int or self.max_retries != 1:
            raise ValueError("本章只允许一次额外重试")
        if (
            isinstance(self.timeout_seconds, bool)
            or not isinstance(self.timeout_seconds, (float, int))
            or not math.isfinite(self.timeout_seconds)
            or not 0 < self.timeout_seconds <= 180
        ):
            raise ValueError("运行超时必须在 (0, 180] 秒内")


@dataclass
class AgentOutcome:
    status: str
    stop_reason: str
    answer: dict[str, Any] | None
    messages: list[Message] = field(default_factory=list)


class Model(Protocol):
    @property
    def metadata(self) -> dict[str, Any]: ...

    def complete(self, messages: list[Message], schemas: list[dict[str, Any]]) -> Message: ...


class Tools(Protocol):
    @property
    def schemas(self) -> list[dict[str, Any]]: ...

    def execute(self, call: dict[str, Any]) -> Message:
        """Return exactly one role=tool message, including for recoverable validation errors."""
        ...
