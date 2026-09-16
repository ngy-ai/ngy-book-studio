"""第 10 章 manual 参考示范；阅读或运行示范不代表独立完成。"""

from ngy_lab.chapter_support import call_tool, finish, load_case


def prepare(data):
    return list(data["document_ids"])


def perform(data, plan, state, tools):
    observed = {}
    for sid in plan:
        response = call_tool(state, tools, "read_document", {"document_id": sid})
        if not response["ok"] and response["error"]["retryable"]:
            response = call_tool(state, tools, "read_document", {"document_id": sid})
        if response["ok"]:
            observed[sid] = response["data"]["facts"]
    candidate = call_tool(state, tools, "draft_candidate", {})["data"]
    answer = candidate["answer"]
    valid = True
    for field in data["fields"]:
        value, sources = answer[field]["value"], answer[field]["source_ids"]
        if value is None:
            valid = valid and not sources and not any(field in facts for facts in observed.values())
        else:
            valid = (
                valid
                and bool(sources)
                and len(set(sources)) == len(sources)
                and all(
                    sid in observed and field in observed[sid] and observed[sid][field] == value
                    for sid in sources
                )
            )
    status = (
        "review_failed" if not valid else "rejected" if not candidate["approved"] else "completed"
    )
    receipt = None
    if status == "completed":
        receipt = call_tool(state, tools, "save_draft", {"answer": answer})["data"]["receipt"]
    return {"status": status, "answer": answer, "receipt": receipt}


def run(task, model, tools, limits, emit):
    state = load_case(task, model, tools)
    data = state["case"]["input"]
    plan = prepare(data)
    result = perform(data, plan, state, tools)
    return finish(state, result)
