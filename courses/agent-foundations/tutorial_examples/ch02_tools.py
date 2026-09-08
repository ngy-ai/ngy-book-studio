"""第二章：工具契约与宿主授权。仅使用虚构内存资料，无网络或持久写入。"""

import copy
import json
from dataclasses import dataclass, field

from langchain_core.tools import tool
from langsmith import tracing_context
from pydantic import BaseModel, ConfigDict, Field, field_validator

DOCUMENTS = {
    "D1": {"body": "松果项目试运行从 2026-10-12 开始。", "facts": {"start_date": "2026-10-12"}},
    "D2": {
        "body": "松果项目面向内部员工，仅允许资料检索与阅读。",
        "facts": {"audience": "内部员工", "allowed_operations": "资料检索与阅读"},
    },
    "PRIVATE": {"body": "另一个虚构项目的内部资料。", "facts": {}},
}
FIELDS = ("start_date", "audience", "allowed_operations")
READ_SCHEMA = {
    "name": "read_document",
    "description": "读取本次授权资料的正文，不能访问任意文件。",
    "parameters": {
        "type": "object",
        "properties": {"document_id": {"type": "string", "minLength": 1, "maxLength": 64}},
        "required": ["document_id"],
        "additionalProperties": False,
    },
}


class ReadArgs(BaseModel):
    model_config = ConfigDict(extra="forbid", strict=True)
    document_id: str = Field(min_length=1, max_length=64)

    @field_validator("document_id")
    @classmethod
    def not_blank(cls, value: str) -> str:
        if not value.strip():
            raise ValueError("资料 ID 不得全为空白")
        return value


class Fact(BaseModel):
    model_config = ConfigDict(extra="forbid", strict=True)
    value: str | None
    source_ids: list[str]


class ResearchAnswer(BaseModel):
    model_config = ConfigDict(extra="forbid", strict=True)
    start_date: Fact
    audience: Fact
    allowed_operations: Fact


def validate_manual(arguments: object) -> dict:
    if not isinstance(arguments, dict) or arguments.keys() != {"document_id"}:
        raise ValueError("参数必须且只能包含 document_id")
    value = arguments["document_id"]
    if not isinstance(value, str) or not value.strip() or len(value) > 64:
        raise ValueError("document_id 必须为 1..64 字符的非空字符串")
    return arguments


@dataclass
class Host:
    allowed_ids: frozenset[str] = frozenset({"D1", "D2"})
    reads: dict = field(default_factory=dict)
    audit: list = field(default_factory=list)

    def read(self, document_id: str) -> dict:
        # 授权来自宿主；模型参数中没有 allowed_ids，也不靠正文来决定权限。
        if document_id not in self.allowed_ids:
            return {"ok": False, "error": {"code": "FORBIDDEN", "retryable": False}}
        document = DOCUMENTS.get(document_id)
        if document is None:
            return {"ok": False, "error": {"code": "NOT_FOUND", "retryable": False}}
        self.reads[document_id] = copy.deepcopy(document)
        return {"ok": True, "data": {"document_id": document_id, **copy.deepcopy(document)}}

    def dispatch(self, name: str, arguments: object, invoke) -> dict:
        if name != READ_SCHEMA["name"]:
            result = {"ok": False, "error": {"code": "UNKNOWN_TOOL", "retryable": False}}
        else:
            try:
                result = invoke(arguments)
            except ValueError:
                result = {"ok": False, "error": {"code": "INVALID_ARGUMENTS", "retryable": False}}
        code = "OK" if result["ok"] else result["error"]["code"]
        self.audit.append({"tool": name, "arguments": arguments, "code": code})
        return result


def validate_manual_answer(answer: object) -> dict:
    if not isinstance(answer, dict) or answer.keys() != set(FIELDS):
        raise ValueError("回答必须恰有三个字段")
    for fact in answer.values():
        if not isinstance(fact, dict) or fact.keys() != {"value", "source_ids"}:
            raise ValueError("每项事实必须包含 value 与 source_ids")
        if fact["value"] is not None and not isinstance(fact["value"], str):
            raise ValueError("value 必须为字符串或 None")
        if not isinstance(fact["source_ids"], list) or not all(
            isinstance(source, str) for source in fact["source_ids"]
        ):
            raise ValueError("source_ids 必须为字符串列表")
    return answer


def validate_framework_answer(answer: object) -> dict:
    return ResearchAnswer.model_validate(answer).model_dump()


def supported(answer: dict, reads: dict, validate=validate_manual_answer) -> bool:
    try:
        parsed = validate(answer)
    except ValueError:
        return False
    for name, fact in parsed.items():
        value, sources = fact["value"], fact["source_ids"]
        if value is None:
            if sources:
                return False
        elif (
            not sources
            or len(sources) != len(set(sources))
            or not all(
                source in reads and reads[source]["facts"].get(name) == value for source in sources
            )
        ):
            return False
    return True


def exercise(invoke, host: Host, validate=validate_manual_answer) -> dict:
    for name, arguments in [
        ("delete_document", {"document_id": "D1"}),
        ("read_document", {"document_id": 42}),
        ("read_document", {"document_id": "D1", "allowed_ids": ["PRIVATE"]}),
        ("read_document", {"document_id": "PRIVATE"}),
        ("read_document", {"document_id": "D1"}),
        ("read_document", {"document_id": "D2"}),
    ]:
        host.dispatch(name, arguments, invoke)
    answer = {name: {"value": None, "source_ids": []} for name in FIELDS}
    for source, document in host.reads.items():
        for name, value in document["facts"].items():
            answer[name] = {"value": value, "source_ids": [source]}
    assert supported(answer, host.reads, validate)
    forged = copy.deepcopy(answer)
    forged["audience"]["value"] = "所有人"
    return {
        "answer": answer,
        "successful_reads": sorted(host.reads),
        "audit": host.audit,
        "forged_answer_accepted": supported(forged, host.reads, validate),
    }


def run_manual() -> dict:
    host = Host()

    def invoke(arguments):
        validated = validate_manual(arguments)
        return host.read(validated["document_id"])

    return exercise(invoke, host)


def run_framework() -> dict:
    host = Host()

    @tool("read_document", args_schema=ReadArgs)
    def read_document(document_id: str) -> dict:
        """读取本次宿主授权的虚构资料，返回正文或结构化错误。"""
        return host.read(document_id)

    with tracing_context(enabled=False):
        return exercise(read_document.invoke, host, validate_framework_answer)


def main() -> None:
    manual, framework = run_manual(), run_framework()
    assert manual == framework
    print(json.dumps({"manual": manual, "framework": framework}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
