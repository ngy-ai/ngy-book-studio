"""Worker-safe transport helpers. No fixtures, policy decisions, or reference answers."""

import json

from moye_lab.contracts import AgentOutcome


def load_case(task, model, tools):
    """Read one fixed exercise input and keep the exact observed message history."""
    messages = [{"role": "user", "content": task}]
    reply = model.complete(messages, tools.schemas)
    messages.append(reply)
    return {"case": json.loads(reply["content"]), "messages": messages, "dispatch": 0}


def call_tool(state, tools, name, arguments):
    """Send one request; return the decoded ok/data or ok/error payload."""
    state["dispatch"] += 1
    call = {
        "id": f"{state['case']['case_id']}:{state['dispatch']}",
        "type": "function",
        "function": {"name": name, "arguments": json.dumps(arguments, ensure_ascii=False)},
    }
    state["messages"].append({"role": "assistant", "content": None, "tool_calls": [call]})
    reply = tools.execute(call)
    state["messages"].append(reply)
    return json.loads(reply["content"])


def finish(state, result):
    """Business rejection belongs in result; host independently grades the work."""
    return AgentOutcome(
        "completed",
        "final_answer",
        {"case_id": state["case"]["case_id"], "result": result},
        state["messages"],
    )
