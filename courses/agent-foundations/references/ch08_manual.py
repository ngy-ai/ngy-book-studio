"""第 8 章 manual 参考示范；阅读或运行示范不代表独立完成。"""

import json

from moye_lab.chapter_support import call_tool, finish, load_case


def prepare(data):
    pairs = set()
    for report in data["reports"]:
        role = report["assigned_role"]
        assignment = data["assignments"][role]
        if (
            report["source_id"] in assignment["source_ids"]
            and report["field"] in assignment["fields"]
        ):
            pairs.add((role, report["source_id"]))
    return sorted(pairs)


def perform(data, plan, state, tools):
    observed = {}
    for role, sid in plan:
        reply = call_tool(state, tools, "read_document", {"role": role, "document_id": sid})
        if reply["ok"]:
            observed[(role, sid)] = reply["data"]["facts"]
    accepted, rejected = {}, []
    for report in data["reports"]:
        role, sid, field = report["assigned_role"], report["source_id"], report["field"]
        facts = observed.get((role, sid), {})
        if (
            (role, sid) in observed
            and field in data["assignments"][role]["fields"]
            and field in facts
            and facts[field] == report["value"]
        ):
            accepted.setdefault(sid, {})[field] = report["value"]
        else:
            rejected.append(report["id"])
    fields = {}
    for field in data["fields"]:
        entries = [(sid, facts[field]) for sid, facts in accepted.items() if field in facts]
        unique = {json.dumps(value, sort_keys=True) for _, value in entries}
        status = "unknown" if not entries else "supported" if len(unique) == 1 else "conflict"
        fields[field] = {
            "status": status,
            "value": entries[0][1] if status == "supported" else None,
            "source_ids": sorted(sid for sid, _ in entries),
        }
    return {"fields": fields, "rejected_claims": sorted(rejected)}


def run(task, model, tools, limits, emit):
    state = load_case(task, model, tools)
    data = state["case"]["input"]
    plan = prepare(data)
    result = perform(data, plan, state, tools)
    return finish(state, result)
