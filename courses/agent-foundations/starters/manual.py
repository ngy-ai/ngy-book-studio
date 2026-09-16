"""练习起点：请在此处亲手实现循环，不要导入参考实现。"""

from ngy_lab.contracts import AgentOutcome, Emit, Model, RunLimits, Tools


def run(task: str, model: Model, tools: Tools, limits: RunLimits, emit: Emit) -> AgentOutcome:
    # TODO 1: 建立 system/user 消息，写明答案格式、来源要求与 8/6 预算。
    # TODO 2: 调用 model.complete(messages, tools.schemas)，保留完整 assistant 消息。
    # TODO 3: 逐个 tools.execute(call)，明确 retryable=true 才额外重试一次。
    # TODO 4: 将最终 tool 消息回填，再进入下一次模型决策。
    # TODO 5: 校验最终 JSON；只捕获 BudgetExceeded 并记录准确停止原因。
    emit({"phase": "incomplete", "implementation": "manual"})
    return AgentOutcome("incomplete", "starter_todo", None)
