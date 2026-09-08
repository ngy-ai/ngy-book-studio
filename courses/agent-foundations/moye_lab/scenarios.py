"""Versioned, fictional fixtures. Expected facts belong to the checker, not the agent prompt."""

from dataclasses import dataclass
from typing import Any


@dataclass(frozen=True)
class Document:
    document_id: str
    title: str
    body: str
    facts: dict[str, str]


@dataclass(frozen=True)
class Scenario:
    name: str
    project: str
    documents: tuple[Document, ...]
    missing_ids: frozenset[str] = frozenset()
    transient_read_ids: frozenset[str] = frozenset()
    transient_search: bool = False
    expected_stop: str = "final_answer"
    version: str = "1.0.0"

    @property
    def task(self) -> str:
        return (
            f"根据资料，查明“{self.project}”的试运行日期、面向对象和允许操作，"
            "为每项结论提供实际读取的资料来源；无法确认的字段标记未知。"
            "日期使用 YYYY-MM-DD；对象与操作范围使用资料中的原词。"
        )

    @property
    def expected_answer(self) -> dict[str, Any]:
        answer = {
            name: {"value": None, "source_ids": []}
            for name in ("start_date", "audience", "allowed_operations")
        }
        for document in self.documents:
            if document.document_id not in self.missing_ids:
                for name, value in document.facts.items():
                    answer[name] = {"value": value, "source_ids": [document.document_id]}
        return answer


BASE_DOCUMENTS = (
    Document(
        "D1",
        "松果项目：试运行安排",
        "【虚构教学资料】松果项目的试运行从 2026-10-12 开始。本通知只说明时间。",
        {"start_date": "2026-10-12"},
    ),
    Document(
        "D2",
        "松果项目：参与和操作范围",
        "【虚构教学资料】松果项目试运行面向内部员工，仅允许资料检索与阅读。",
        {"audience": "内部员工", "allowed_operations": "资料检索与阅读"},
    ),
)
TRANSFER_DOCUMENTS = (
    Document(
        "R7",
        "海鸥项目：新一轮试运行公告",
        "【虚构教学资料】海鸥项目试运行从 2027-02-18 开始。参与和操作另见范围文件。",
        {"start_date": "2027-02-18"},
    ),
    Document(
        "R9",
        "海鸥项目：参与和操作范围",
        "【虚构教学资料】海鸥项目试运行面向受邀志愿者，仅允许目录查询与标注。",
        {"audience": "受邀志愿者", "allowed_operations": "目录查询与标注"},
    ),
)

SCENARIOS = {
    "normal": Scenario("normal", "松果项目", BASE_DOCUMENTS),
    "invalid_call": Scenario("invalid_call", "松果项目", BASE_DOCUMENTS),
    "invalid_arguments": Scenario("invalid_arguments", "松果项目", BASE_DOCUMENTS),
    "transient": Scenario(
        "transient", "松果项目", BASE_DOCUMENTS, transient_read_ids=frozenset({"D2"})
    ),
    "missing": Scenario("missing", "松果项目", BASE_DOCUMENTS, missing_ids=frozenset({"D2"})),
    "transfer": Scenario(
        "transfer",
        "海鸥项目",
        TRANSFER_DOCUMENTS,
        missing_ids=frozenset({"R9"}),
        transient_search=True,
    ),
    "model_budget": Scenario(
        "model_budget", "松果项目", BASE_DOCUMENTS, expected_stop="model_budget"
    ),
    "tool_budget": Scenario("tool_budget", "松果项目", BASE_DOCUMENTS, expected_stop="tool_budget"),
}


def get_scenario(name: str) -> Scenario:
    try:
        return SCENARIOS[name]
    except KeyError as error:
        raise ValueError(f"未知场景：{name}") from error
