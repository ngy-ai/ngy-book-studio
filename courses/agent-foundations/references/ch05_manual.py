"""第 5 章 manual 参考示范；阅读或运行示范不代表独立完成。"""

from moye_lab.chapter_support import call_tool, finish, load_case


def prepare(data):
    steps = {step["id"]: step for step in data["steps"]}
    order = []
    if len(steps) != len(data["steps"]):
        return {"status": "invalid_plan", "order": []}
    # Validate the entire graph before the first effect, including late cycles.
    while len(order) < len(steps):
        ready = sorted(
            sid
            for sid, step in steps.items()
            if sid not in order and all(dep in order for dep in step["depends_on"])
        )
        if not ready:
            return {"status": "invalid_plan", "order": []}
        order.append(ready[0])
    return {
        "status": "completed" if len(order) <= data["budget"] else "budget_exhausted",
        "order": order[: data["budget"]],
    }


def perform(data, plan, state, tools):
    for sid in plan["order"]:
        call_tool(state, tools, "execute_step", {"step_id": sid})
    return plan


def run(task, model, tools, limits, emit):
    state = load_case(task, model, tools)
    data = state["case"]["input"]
    plan = prepare(data)
    result = perform(data, plan, state, tools)
    return finish(state, result)
