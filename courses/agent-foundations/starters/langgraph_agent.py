"""练习起点：用真实 StateGraph 节点和边复现手搓版的行为。"""

from ngy_lab.contracts import AgentOutcome, Emit, Model, RunLimits, Tools


def run(task: str, model: Model, tools: Tools, limits: RunLimits, emit: Emit) -> AgentOutcome:
    # TODO 1: 定义图状态，保存完整消息与本轮是否存在工具调用。
    # TODO 2: 分别实现 model 节点与 tools 节点，不在单节点内塞入完整 while 循环。
    # TODO 3: START -> model；有调用则 tools -> model，无调用则 END。
    # TODO 4: 保持一次额外重试、严格 JSON、预算与停止原因和手搓版一致。
    # TODO 5: 在 tracing_context(enabled=False) 内编译和运行图。
    emit({"phase": "incomplete", "implementation": "langgraph"})
    return AgentOutcome("incomplete", "starter_todo", None)
