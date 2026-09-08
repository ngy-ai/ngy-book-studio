"""第四章：记忆策略与工作上下文；演示存储仅在内存中，不写个人数据。"""

import copy
import json
from dataclasses import asdict, dataclass, replace
from datetime import date
from typing import TypedDict

from langgraph.graph import END, START, StateGraph
from langsmith import tracing_context


@dataclass(frozen=True)
class Memory:
    id: str
    user: str
    project: str
    field: str
    value: str
    source_id: str
    expires: str
    verified: bool = True
    status: str = "active"
    supersedes: str | None = None


INITIAL = (
    Memory("M1", "learner", "pine", "start_date", "2026-10-12", "D1", "2026-10-20"),
    Memory("M2", "learner", "pine", "allowed_operations", "旧范围", "OLD", "2026-10-01"),
    Memory("M3", "other-user", "pine", "start_date", "其他用户信息", "PRIVATE", "2026-12-31"),
    Memory("M4", "learner", "other-project", "start_date", "其他项目日期", "X", "2026-12-31"),
    Memory("M5", "learner", "pine", "audience", "据说所有人", "RUMOR", "2026-12-31", False),
)
AS_OF = date(2026, 10, 11)
CORRECTION = {
    "source_id": "D4",
    "project": "pine",
    "field": "start_date",
    "value": "2026-10-15",
    "supersedes_source": "D1",
    "quote": "松果项目试运行改为 2026-10-15；本更正取代 D1 中的原日期。",
}


class MemoryStore:
    def __init__(self):
        self.records = list(INITIAL)

    def recall(self, user: str, project: str, as_of: date) -> dict:
        accepted, rejected = [], {"scope": 0, "expired": 0, "unverified": 0, "superseded": 0}
        for item in self.records:
            if (item.user, item.project) != (user, project):
                rejected["scope"] += 1
            elif item.status != "active":
                rejected["superseded"] += 1
            elif date.fromisoformat(item.expires) <= as_of:
                rejected["expired"] += 1
            elif not item.verified:
                rejected["unverified"] += 1
            else:
                accepted.append(asdict(item))
        return {"records": accepted, "rejected_counts": rejected}

    def apply_verified_correction(self, user: str, project: str, evidence: dict) -> str:
        # 受控流程调用 read_current_source；相等检查仅匹配 fixture，不认证外部来源。
        if evidence != CORRECTION or project != evidence["project"]:
            raise ValueError("必须使用本次重新读取、可核验的更正证据")
        previous = next(
            (
                item
                for item in self.records
                if (item.user, item.project, item.field, item.source_id)
                == (user, project, evidence["field"], evidence["supersedes_source"])
            ),
            None,
        )
        if previous is None:
            raise ValueError("没有同一用户、项目、字段中的被更正记录")
        new = Memory(
            "M6",
            user,
            project,
            evidence["field"],
            evidence["value"],
            evidence["source_id"],
            "2026-10-20",
            supersedes=previous.id,
        )
        existing = next((item for item in self.records if item.id == new.id), None)
        if existing is not None:
            if existing != new:
                raise ValueError("记忆 ID 已存在但内容不同")
            return "already_applied"
        self.records = [
            replace(item, status="superseded") if item.id == previous.id else item
            for item in self.records
        ]
        self.records.append(new)
        return "applied"


def read_current_source() -> dict:
    """模拟宿主重新读取一份固定虚构更正；不信任召回文本发出的指令。"""
    return copy.deepcopy(CORRECTION)


def summarize(store: MemoryStore, before: dict, evidence: dict, write_status: str) -> dict:
    after = store.recall("learner", "pine", AS_OF)
    current = next(item for item in after["records"] if item["field"] == "start_date")
    return {
        "before": before,
        "fresh_evidence": evidence,
        "write_status": write_status,
        "after": after,
        "answer": {"value": current["value"], "source_ids": [current["source_id"]]},
        "later_recall": store.recall("learner", "pine", date(2026, 10, 21)),
        "scope": "memory-only demo; no files written",
    }


def run_manual() -> dict:
    store = MemoryStore()
    before = store.recall("learner", "pine", AS_OF)
    evidence = read_current_source()
    write_status = store.apply_verified_correction("learner", "pine", evidence)
    return summarize(store, before, evidence, write_status)


class MemoryState(TypedDict, total=False):
    task: str
    before: dict
    fresh_evidence: dict
    write_status: str
    output: dict


def run_framework() -> dict:
    store = MemoryStore()

    def recall(_state):
        return {"before": store.recall("learner", "pine", AS_OF)}

    def verify(_state):
        return {"fresh_evidence": read_current_source()}

    def remember(state):
        status = store.apply_verified_correction("learner", "pine", state["fresh_evidence"])
        return {"write_status": status}

    def answer(state):
        return {
            "output": summarize(
                store, state["before"], state["fresh_evidence"], state["write_status"]
            )
        }

    graph = StateGraph(MemoryState)
    for name, action in [
        ("recall", recall),
        ("verify", verify),
        ("remember", remember),
        ("answer", answer),
    ]:
        graph.add_node(name, action)
    for left, right in [
        (START, "recall"),
        ("recall", "verify"),
        ("verify", "remember"),
        ("remember", "answer"),
        ("answer", END),
    ]:
        graph.add_edge(left, right)
    with tracing_context(enabled=False):
        return graph.compile().invoke({"task": "重新核对松果项目的当前试运行日期"})["output"]


def main() -> None:
    manual, framework = run_manual(), run_framework()
    assert manual == framework
    assert manual["answer"]["source_ids"] == ["D4"]
    assert manual["later_recall"]["records"] == []
    print(json.dumps({"manual": manual, "framework": framework}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
