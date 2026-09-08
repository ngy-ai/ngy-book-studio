"""Approval and recovery over fictional in-memory drafts; no external effects."""

from __future__ import annotations

import hashlib
import json
from copy import deepcopy
from typing import TypedDict

from langgraph.checkpoint.memory import InMemorySaver
from langgraph.graph import END, START, StateGraph
from langgraph.types import Command, interrupt
from langsmith import tracing_context


def proposal() -> dict:
    return {
        "operation": "save_memory_draft",
        "action_id": "songguo-draft-1",
        "revision": 1,
        "text": "松果项目于 2026-10-12 开始。",
        "source_ids": ["D1"],
    }


def proposal_digest(value: dict) -> str:
    encoded = json.dumps(value, sort_keys=True, ensure_ascii=False).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def approval_request(value: dict) -> dict:
    return {"proposal": deepcopy(value), "digest": proposal_digest(value), "expires_at": 30}


class DraftStore:
    """Single-process simulation; real exactly-once effects need a durable transaction."""

    def __init__(self) -> None:
        self.entries: dict[str, dict] = {}
        self.writes = 0

    def put(self, value: dict) -> str:
        key, digest = value["action_id"], proposal_digest(value)
        previous = self.entries.get(key)
        if previous:
            if previous["digest"] != digest:
                raise ValueError("idempotency_conflict")
            return previous["receipt"]
        receipt = f"memory:{key}"
        self.entries[key] = {"digest": digest, "receipt": receipt, "proposal": deepcopy(value)}
        self.writes += 1
        return receipt


def decide(request: dict, reply: dict, now: int, cancelled: bool, current: dict) -> str:
    if cancelled:
        return "cancelled"
    if now >= request["expires_at"]:
        return "expired"
    if proposal_digest(current) != request["digest"]:
        return "proposal_changed"
    if reply.get("digest") != request["digest"]:
        return "invalid_approval"
    return "approved" if reply.get("decision") == "approve" else "rejected"


class ApprovalState(TypedDict):
    proposal: dict
    request: dict
    reply: dict
    status: str
    receipt: str | None


def exercise(scenario: str, use_framework: bool) -> dict:
    store = DraftStore()
    current = proposal()
    request = approval_request(current)
    clock = {"now": 0, "cancelled": False}
    state: ApprovalState = {
        "proposal": current,
        "request": request,
        "reply": {},
        "status": "awaiting_approval",
        "receipt": None,
    }

    def commit(s: ApprovalState) -> dict:
        status = decide(s["request"], s["reply"], clock["now"], clock["cancelled"], current)
        receipt = store.put(current) if status == "approved" else None
        return {"status": "completed" if receipt else status, "receipt": receipt}

    app = None
    config = {"configurable": {"thread_id": f"demo-{scenario}"}}
    if use_framework:

        def ask(s: ApprovalState) -> dict:
            # This node starts again on resume. Nothing before interrupt writes a draft.
            return {"reply": interrupt(s["request"])}

        graph = StateGraph(ApprovalState)
        graph.add_node("ask", ask)
        graph.add_node("commit", commit)
        graph.add_edge(START, "ask")
        graph.add_edge("ask", "commit")
        graph.add_edge("commit", END)
        app = graph.compile(checkpointer=InMemorySaver())
        with tracing_context(enabled=False):
            paused = app.invoke(state, config=config)
        shown = paused["__interrupt__"][0].value
    else:
        # A serialized checkpoint is still only an in-memory string in this demo.
        checkpoint = json.dumps(state, ensure_ascii=False)
        state = json.loads(checkpoint)
        shown = state["request"]

    writes_before_resume = store.writes
    reply = {"decision": "approve", "digest": shown["digest"]}
    if scenario == "rejected":
        reply["decision"] = "reject"
    elif scenario == "expired":
        clock["now"] = 31
    elif scenario == "cancelled":
        clock["cancelled"] = True
    elif scenario == "tampered":
        reply["digest"] = "not-the-approved-content"
    elif scenario == "changed":
        current = {**current, "revision": 2, "text": "待重新审核的不同草稿"}

    if app is not None:
        with tracing_context(enabled=False):
            state = app.invoke(Command(resume=reply), config=config)
    else:
        state["reply"] = reply
        state.update(commit(state))

    duplicate_receipt = None
    if state["status"] == "completed":
        # Simulate replay at the storage boundary after a lost acknowledgement.
        duplicate_receipt = store.put(current)
    return {
        "status": state["status"],
        "receipt": state["receipt"],
        "duplicate_receipt": duplicate_receipt,
        "writes_before_resume": writes_before_resume,
        "writes": store.writes,
        "shown_request": shown,
        "drafts": [entry["proposal"] for entry in store.entries.values()],
    }


def run_manual() -> dict:
    return {
        name: exercise(name, False)
        for name in ("approved", "rejected", "expired", "cancelled", "tampered", "changed")
    }


def run_framework() -> dict:
    return {
        name: exercise(name, True)
        for name in ("approved", "rejected", "expired", "cancelled", "tampered", "changed")
    }


if __name__ == "__main__":
    manual, framework = run_manual(), run_framework()
    print(
        json.dumps(
            {"manual": manual, "framework": framework, "equivalent": manual == framework},
            ensure_ascii=False,
            indent=2,
        )
    )
