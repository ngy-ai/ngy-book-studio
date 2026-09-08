"""The reference agent loop written directly in Python."""

from moye_lab.contracts import AgentOutcome, BudgetExceeded, Emit, Model, RunLimits, Tools
from moye_lab.implementations.common import Session, parse_final


def run(task: str, model: Model, tools: Tools, limits: RunLimits, emit: Emit) -> AgentOutcome:
    session = Session(task, model, tools, limits, emit)
    try:
        while True:
            assistant = session.model_step()
            if not assistant.get("tool_calls"):
                return session.completed(parse_final(assistant))
            session.tool_step()
    except BudgetExceeded as error:
        return session.exhausted(error)
