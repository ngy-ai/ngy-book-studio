"""第 2 章 manual 参考示范；阅读或运行示范不代表独立完成。"""

from moye_lab.chapter_support import call_tool, finish, load_case


def prepare(data):
    decisions = []
    for request in data["requests"]:
        args = request["arguments"]
        error = None
        if request["name"] != "read_document":
            error = "unknown_tool"
        elif not isinstance(args, dict) or set(args) != {"document_id"}:
            error = "invalid_arguments"
        elif not isinstance(args["document_id"], str) or not args["document_id"]:
            error = "invalid_arguments"
        elif args["document_id"] not in data["allowed_ids"]:
            error = "outside_scope"
        decisions.append(
            {
                "request_id": request["id"],
                "accepted": error is None,
                "error_code": error,
                "data": None,
            }
        )
    return decisions


def perform(data, plan, state, tools):
    for request, decision in zip(data["requests"], plan, strict=True):
        if decision["accepted"]:
            result = call_tool(state, tools, "read_document", request["arguments"])
            decision["data"] = result["data"]
    return {"decisions": plan}


def run(task, model, tools, limits, emit):
    state = load_case(task, model, tools)
    data = state["case"]["input"]
    plan = prepare(data)
    result = perform(data, plan, state, tools)
    return finish(state, result)
