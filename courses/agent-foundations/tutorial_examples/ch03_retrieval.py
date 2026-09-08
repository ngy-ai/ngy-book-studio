"""第三章：本地词项检索与可追溯证据；不是向量搜索，也不调用模型。"""

import json
import re
from dataclasses import dataclass
from typing import TypedDict

from langgraph.graph import END, START, StateGraph
from langsmith import tracing_context


@dataclass(frozen=True)
class Document:
    id: str
    scope: str
    text: str


@dataclass(frozen=True)
class Chunk:
    id: str
    document_id: str
    scope: str
    text: str


DOCUMENTS = (
    Document("D1", "pine", "松果项目试运行从 2026-10-12 开始。\n\n本页只说明日期。"),
    Document("D2", "pine", "松果项目面向内部员工，仅允许资料检索与阅读。"),
    Document("D3", "pine", "松果项目试运行从 2026-10-15 开始。"),
    Document("PRIVATE", "other", "松果项目试运行开始日期、面向对象、允许操作均在此。"),
)
QUESTION = "松果项目试运行日期、面向对象和允许操作是什么？"


def split_documents(documents=DOCUMENTS) -> list[Chunk]:
    return [
        Chunk(f"{document.id}:p{index}", document.id, document.scope, paragraph)
        for document in documents
        for index, paragraph in enumerate(document.text.split("\n\n"), start=1)
        if paragraph.strip()
    ]


def terms(text: str) -> set[str]:
    result = set(re.findall(r"[a-z0-9]+", text.lower()))
    for segment in re.findall(r"[\u4e00-\u9fff]+", text):
        result.update(segment[index : index + 2] for index in range(len(segment) - 1))
    return result


def retrieve(question: str, chunks: list[Chunk], scope: str, limit: int = 4) -> list[dict]:
    query_terms = terms(question)
    candidates = []
    for chunk in chunks:
        if chunk.scope != scope:  # 权限过滤先于相关性排序。
            continue
        score = len(query_terms & terms(chunk.text))
        if score:
            candidates.append(
                {"chunk_id": chunk.id, "document_id": chunk.document_id, "score": score}
            )
    return sorted(candidates, key=lambda item: (-item["score"], item["chunk_id"]))[:limit]


def read_evidence(hits: list[dict], chunks: list[Chunk], scope: str) -> list[dict]:
    by_id = {chunk.id: chunk for chunk in chunks}
    evidence = []
    for hit in hits:
        chunk = by_id.get(hit["chunk_id"])
        if chunk is None or chunk.scope != scope:
            continue  # 加载正文时再次校验归属，不能相信外部传回的候选 ID。
        evidence.append({"source_id": chunk.id, "quote": chunk.text})
    return evidence


def compose(evidence: list[dict]) -> dict:
    patterns = {
        "start_date": r"试运行从 (\d{4}-\d{2}-\d{2}) 开始",
        "audience": r"面向([^，。]+)",
        "allowed_operations": r"仅允许([^。]+)",
    }
    answer = {}
    for field, pattern in patterns.items():
        claims: dict[str, list[str]] = {}
        for item in evidence:
            for match in re.finditer(pattern, item["quote"]):
                claims.setdefault(match.group(1), []).append(item["source_id"])
        if len(claims) == 1:
            value, sources = next(iter(claims.items()))
            answer[field] = {
                "status": "supported",
                "value": value,
                "source_ids": sorted(set(sources)),
            }
        else:
            answer[field] = {
                "status": "conflict" if claims else "unknown",
                "value": None,
                "source_ids": [],
                "alternatives": [
                    {"value": value, "source_ids": sorted(set(sources))}
                    for value, sources in sorted(claims.items())
                ],
            }
    return answer


def result(hits: list[dict], evidence: list[dict]) -> dict:
    return {
        "retrieval": "local_terms",
        "hits": hits,
        "evidence": evidence,
        "answer": compose(evidence),
    }


def run_manual() -> dict:
    chunks = split_documents()
    hits = retrieve(QUESTION, chunks, "pine")
    evidence = read_evidence(hits, chunks, "pine")
    return result(hits, evidence)


class ResearchState(TypedDict, total=False):
    question: str
    hits: list[dict]
    evidence: list[dict]
    output: dict


def run_framework() -> dict:
    chunks = split_documents()
    trusted_scope = "pine"  # 宿主闭包，不让候选资料控制权限。

    def search(state):
        return {"hits": retrieve(state["question"], chunks, trusted_scope)}

    def read(state):
        return {"evidence": read_evidence(state["hits"], chunks, trusted_scope)}

    def answer(state):
        return {"output": result(state["hits"], state["evidence"])}

    graph = StateGraph(ResearchState)
    graph.add_node("retrieve", search)
    graph.add_node("read", read)
    graph.add_node("answer", answer)
    graph.add_edge(START, "retrieve")
    graph.add_edge("retrieve", "read")
    graph.add_edge("read", "answer")
    graph.add_edge("answer", END)
    with tracing_context(enabled=False):
        return graph.compile().invoke({"question": QUESTION})["output"]


def main() -> None:
    manual, framework = run_manual(), run_framework()
    assert manual == framework
    assert manual["answer"]["start_date"]["status"] == "conflict"
    assert all(hit["document_id"] != "PRIVATE" for hit in manual["hits"])
    print(json.dumps({"manual": manual, "framework": framework}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
