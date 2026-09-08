"""Host-owned observation, authorization, retry accounting and deterministic replay.

These interfaces check reviewed learning code. They are not a hostile-code sandbox.
"""

import copy
import json
import time
from typing import Any

from moye_lab.contracts import BudgetExceeded, Message, Model, ProtocolError, RunLimits
from moye_lab.scenarios import Scenario

TOOL_SCHEMAS = [
    {
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": {
                "type": "object",
                "properties": {parameter: {"type": "string"}},
                "required": [parameter],
                "additionalProperties": False,
            },
        },
    }
    for name, parameter, description in (
        ("search_documents", "query", "搜索虚构资料，只返回最多三个 ID 和标题，不返回正文。"),
        ("read_document", "document_id", "读取一份虚构资料正文；失败时返回错误类型和可重试标记。"),
    )
]


def json_text(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def failure(code: str, message: str, retryable: bool = False) -> dict[str, Any]:
    return {"ok": False, "error": {"code": code, "message": message, "retryable": retryable}}


class ToolBroker:
    def __init__(self, scenario: Scenario, limits: RunLimits, started: float):
        self.scenario = scenario
        self.limits = limits
        self.started = started
        self.records: list[dict[str, Any]] = []
        self.executions = 0
        self.dispatch_attempts = 0
        self.successful_reads: dict[str, str] = {}
        self.pending: list[dict[str, Any]] = []
        self.results: dict[str, Message] = {}
        self.attempts: dict[str, list[dict[str, Any]]] = {}
        self.seen_call_ids: set[str] = set()
        self.search_failed = False
        self.read_failed: set[str] = set()
        self.violations: list[str] = []
        self.exhausted: str | None = None
        self.exhaustion_events: list[dict[str, Any]] = []

    @property
    def schemas(self) -> list[dict[str, Any]]:
        return copy.deepcopy(TOOL_SCHEMAS)

    def check_deadline(self) -> None:
        if time.monotonic() - self.started >= self.limits.timeout_seconds:
            self.exhausted = "deadline"
            self.exhaustion_events.append({"reason": "deadline"})
            raise BudgetExceeded("deadline")

    def register(self, calls: list[dict[str, Any]]) -> None:
        if len(calls) > 4:
            raise ProtocolError("每次模型决策最多请求四个工具调用")
        for call in calls:
            if (
                not isinstance(call, dict)
                or call.get("type") != "function"
                or not isinstance(call.get("id"), str)
                or not call["id"]
                or call["id"] in self.seen_call_ids
                or not isinstance(call.get("function"), dict)
                or not isinstance(call["function"].get("name"), str)
                or not isinstance(call["function"].get("arguments"), str)
                or len(call["function"]["arguments"].encode("utf-8")) > 4096
            ):
                raise ProtocolError("模型工具调用外壳无效或 call ID 重复")
            self.seen_call_ids.add(call["id"])
        self.pending = copy.deepcopy(calls)
        self.results = {}

    def turn_results(self) -> list[Message]:
        if any(call["id"] not in self.results for call in self.pending):
            raise ProtocolError("再次请求模型前，必须回填每个工具调用的真实结果")
        return [copy.deepcopy(self.results[call["id"]]) for call in self.pending]

    def execute(self, call: dict[str, Any]) -> Message:
        self.dispatch_attempts += 1
        self.check_deadline()
        if call not in self.pending:
            self.violations.append("unauthorized_dispatch")
            raise ProtocolError("工具执行必须对应当前模型实际返回的调用")
        name = call["function"]["name"]
        previous_result = self.results.get(call["id"])
        if previous_result is not None:
            previous_payload = json.loads(previous_result["content"])
            if previous_payload.get("error", {}).get("retryable") is not True:
                self.violations.append("retry_not_allowed")
                raise ProtocolError("同一 call ID 仅可在明确可重试失败后额外执行一次")
        args = None
        try:
            args = json.loads(call["function"]["arguments"])
        except (ValueError, TypeError):
            pass
        parameter = {"search_documents": "query", "read_document": "document_id"}.get(name)
        actual = False
        signature = ""
        if parameter is None:
            payload = failure("UNKNOWN_TOOL", "工具不存在，只能使用已登记的只读工具")
        elif (
            not isinstance(args, dict)
            or args.keys() != {parameter}
            or not isinstance(args[parameter], str)
            or not args[parameter].strip()
            or len(args[parameter]) > 256
        ):
            payload = failure("INVALID_ARGUMENTS", f"必须且只能提供非空字符串参数 {parameter}")
        else:
            signature = json_text([name, args])
            history = self.attempts.get(signature, [])
            if history and (
                len(history) >= 2
                or history[-1]["ok"]
                or history[-1].get("error", {}).get("retryable") is not True
            ):
                self.violations.append("retry_not_allowed")
                payload = failure("RETRY_NOT_ALLOWED", "只有明确可重试的失败允许额外尝试一次")
            else:
                if self.executions >= self.limits.max_tool_executions:
                    self.exhausted = "tool_budget"
                    self.exhaustion_events.append(
                        {
                            "reason": "tool_budget",
                            "refused_call": copy.deepcopy(call),
                            "actual_tool_executions": self.executions,
                            "dispatch_attempt": self.dispatch_attempts,
                        }
                    )
                    raise BudgetExceeded("tool_budget")
                self.executions += 1
                actual = True
                payload = self._backend(name, args)
                self.attempts.setdefault(signature, []).append(copy.deepcopy(payload))
        message = {"role": "tool", "tool_call_id": call["id"], "content": json_text(payload)}
        self.results[call["id"]] = copy.deepcopy(message)
        self.records.append(
            {
                "ordinal": len(self.records) + 1,
                "tool_call_id": call["id"],
                "name": name,
                "arguments": args,
                "actual_execution": actual,
                "execution_number": self.executions if actual else None,
                "elapsed_seconds": round(time.monotonic() - self.started, 6),
                "result": copy.deepcopy(payload),
                "signature": signature,
            }
        )
        self.check_deadline()
        return message

    def _backend(self, name: str, args: dict[str, str]) -> dict[str, Any]:
        if name == "search_documents":
            if self.scenario.transient_search and not self.search_failed:
                self.search_failed = True
                return failure("TIMEOUT", "搜索暂时超时", True)
            query = args["query"].strip().casefold()
            documents = [
                {"document_id": doc.document_id, "title": doc.title}
                for doc in self.scenario.documents
                if query in (doc.title + doc.body).casefold()
                or self.scenario.project.casefold() in query
            ][:3]
            return {"ok": True, "data": {"documents": documents}}
        document_id = args["document_id"]
        document = next(
            (doc for doc in self.scenario.documents if doc.document_id == document_id), None
        )
        if document is None or document_id in self.scenario.missing_ids:
            return failure("NOT_FOUND", "资料不存在", False)
        if document_id in self.scenario.transient_read_ids and document_id not in self.read_failed:
            self.read_failed.add(document_id)
            return failure("TIMEOUT", "资料读取暂时超时", True)
        self.successful_reads[document_id] = document.body
        return {
            "ok": True,
            "data": {
                "document_id": document_id,
                "title": document.title,
                "body": document.body,
            },
        }


class ObservedModel:
    def __init__(self, inner: Model, tools: ToolBroker, limits: RunLimits):
        self.inner, self.tools, self.limits = inner, tools, limits
        self.records: list[dict[str, Any]] = []
        self.previous: list[Message] | None = None
        self.exhausted: str | None = None

    @property
    def metadata(self) -> dict[str, Any]:
        return copy.deepcopy(self.inner.metadata)

    def complete(self, messages: list[Message], schemas: list[dict[str, Any]]) -> Message:
        self.tools.check_deadline()
        if len(self.records) >= self.limits.max_model_decisions:
            self.exhausted = "model_budget"
            raise BudgetExceeded("model_budget")
        if schemas != TOOL_SCHEMAS:
            raise ProtocolError("模型必须得到宿主提供的相同工具 schema")
        if self.previous is not None:
            expected = self.previous + self.tools.turn_results()
            if messages != expected:
                raise ProtocolError("消息回填错误：必须原样保留 assistant 和匹配的真实工具结果")
        elif (
            not isinstance(messages, list)
            or len(messages) != 2
            or messages[0].get("role") != "system"
            or messages[1].get("role") != "user"
            or messages[1].get("content") != self.tools.scenario.task
        ):
            raise ProtocolError("首轮必须包含 system 和本次运行的原始 user 任务")
        before = time.monotonic()
        record = {"decision": len(self.records) + 1, "messages": copy.deepcopy(messages)}
        self.records.append(record)  # Failed provider attempts also consume a decision.
        set_deadline = getattr(self.inner, "set_deadline", None)
        if set_deadline is not None:
            set_deadline(self.tools.started + self.limits.timeout_seconds)
        response = self.inner.complete(copy.deepcopy(messages), copy.deepcopy(schemas))
        record["duration_seconds"] = round(time.monotonic() - before, 6)
        self.tools.check_deadline()
        if not isinstance(response, dict) or response.get("role") != "assistant":
            raise ProtocolError("模型必须返回 assistant 消息")
        calls = response.get("tool_calls") or []
        if not isinstance(calls, list):
            raise ProtocolError("tool_calls 必须是列表")
        self.tools.register(calls)
        record["response"] = copy.deepcopy(response)
        self.previous = copy.deepcopy(messages) + [copy.deepcopy(response)]
        return copy.deepcopy(response)


class ScriptedModel:
    """Predesigned decisions; validates recovery results before advancing the script."""

    def __init__(self, scenario: Scenario):
        self.scenario = scenario
        self.index = 0

    @property
    def metadata(self) -> dict[str, Any]:
        return {"mode": "scripted", "model": "chapter-01-replay-v1", "network": False}

    def complete(self, messages: list[Message], schemas: list[dict[str, Any]]) -> Message:
        stage = self.index
        self.index += 1
        name = self.scenario.name
        if name == "model_budget":
            return self._calls([("unregistered_tool", {})])
        if name == "tool_budget":
            return self._calls([("search_documents", {"query": f"预算探针-{stage}"})])
        if name in {"invalid_call", "invalid_arguments"}:
            if stage == 0:
                return self._calls(
                    [("unregistered_tool" if name == "invalid_call" else "search_documents", {})]
                )
            if stage == 1:
                result = json.loads(messages[-1]["content"])
                expected = "UNKNOWN_TOOL" if name == "invalid_call" else "INVALID_ARGUMENTS"
                if result.get("error", {}).get("code") != expected:
                    raise ProtocolError("固定响应模式没有收到预期的非法调用错误")
            stage -= 1
        if stage == 0:
            return self._calls([("search_documents", {"query": self.scenario.project})])
        if stage == 1:
            result = json.loads(messages[-1]["content"])
            expected_ids = [doc.document_id for doc in self.scenario.documents]
            found = result.get("data", {}).get("documents", [])
            if (
                result.get("ok") is not True
                or [doc.get("document_id") for doc in found] != expected_ids
            ):
                raise ProtocolError("固定响应模式没有收到成功搜索结果；检查重试与回填")
            return self._calls(
                [("read_document", {"document_id": document["document_id"]}) for document in found]
            )
        if stage == 2:
            results = [json.loads(message["content"]) for message in messages[-2:]]
            for document, result in zip(self.scenario.documents, results, strict=True):
                if document.document_id in self.scenario.missing_ids:
                    valid = result.get("error", {}).get("code") == "NOT_FOUND"
                else:
                    valid = result.get("data", {}).get("body") == document.body
                if not valid:
                    raise ProtocolError("固定响应模式没有收到预期读取结果；检查结果保留与重试")
            return {"role": "assistant", "content": json_text(self.scenario.expected_answer)}
        raise ProtocolError("固定响应序列已经结束")

    def _calls(self, requests: list[tuple[str, dict[str, Any]]]) -> Message:
        return {
            "role": "assistant",
            "content": None,
            "tool_calls": [
                {
                    "id": f"call_{self.index}_{index}",
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": json_text(args),
                    },
                }
                for index, (name, args) in enumerate(requests)
            ],
        }
