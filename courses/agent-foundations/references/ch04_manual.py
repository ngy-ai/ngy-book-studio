"""第 4 章 manual 参考示范；阅读或运行示范不代表独立完成。"""

from ngy_lab.chapter_support import call_tool, finish, load_case


def prepare(data):
    eligible = []
    for record in data["records"]:
        if (
            record["user_id"] == data["user_id"]
            and record["project_id"] == data["project_id"]
            and record["expires_at"] > data["now"]
            and record["verified"] is True
            and record["status"] == "active"
        ):
            eligible.append(record)
    return sorted(eligible, key=lambda record: record["id"])


def perform(data, plan, state, tools):
    selected = []
    for record in plan:
        reply = call_tool(state, tools, "read_memory", {"memory_id": record["id"]})
        selected.append(
            {"id": record["id"], "field": record["field"], "value": reply["data"]["value"]}
        )
    return {"selected": selected}


def run(task, model, tools, limits, emit):
    state = load_case(task, model, tools)
    data = state["case"]["input"]
    plan = prepare(data)
    result = perform(data, plan, state, tools)
    return finish(state, result)
