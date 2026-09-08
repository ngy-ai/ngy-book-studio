"""第 3 章 manual 参考示范；阅读或运行示范不代表独立完成。"""

import json

from moye_lab.chapter_support import call_tool, finish, load_case


def prepare(data):
    # Only metadata participates in selection; it is not body evidence.
    return sorted(
        document["id"]
        for document in data["documents"]
        if document["id"] in data["allowed_ids"]
        and set(document["terms"]) & set(data["query_terms"])
    )


def perform(data, plan, state, tools):
    evidence = {}
    for sid in plan:
        reply = call_tool(state, tools, "read_document", {"document_id": sid})
        if reply["ok"]:
            evidence[sid] = reply["data"]["facts"]
    fields = {}
    for field in data["fields"]:
        entries = [(sid, facts[field]) for sid, facts in evidence.items() if field in facts]
        unique = {json.dumps(value, sort_keys=True) for _, value in entries}
        status = "unknown" if not entries else "supported" if len(unique) == 1 else "conflict"
        fields[field] = {
            "status": status,
            "value": entries[0][1] if status == "supported" else None,
            "source_ids": sorted(sid for sid, _ in entries),
        }
    return {"fields": fields}


def run(task, model, tools, limits, emit):
    state = load_case(task, model, tools)
    data = state["case"]["input"]
    plan = prepare(data)
    result = perform(data, plan, state, tools)
    return finish(state, result)
