"""第 9 章 manual 独立练习骨架。"""

from ngy_lab.chapter_support import finish, load_case


def prepare(data):
    # 在这里实现本章的验证、筛选或计划；输入形状见本章桌面练习说明。
    raise NotImplementedError("先实现 prepare：不要把参考输出或旧资料 ID 写成常量")


def perform(data, plan, state, tools):
    # 需要调用工具时：from ngy_lab.chapter_support import call_tool
    # call_tool(state, tools, 名称, 参数) 返回 ok/data 或 ok/error。
    # 返回本章 result 字典；宿主会独立核验真实动作与本次数据。
    raise NotImplementedError("再实现 perform：保留实际工具响应并形成候选结果")


def run(task, model, tools, limits, emit):
    state = load_case(task, model, tools)
    data = state["case"]["input"]
    plan = prepare(data)
    result = perform(data, plan, state, tools)
    return finish(state, result)
