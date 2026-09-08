"""第 7 章 manual 参考示范；阅读或运行示范不代表独立完成。"""

import hashlib
import json

from moye_lab.chapter_support import call_tool, finish, load_case


def proposal_digest(proposal):
    encoded = json.dumps(proposal, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def prepare(data):
    approval = data["approval"]
    if data["cancelled"]:
        return "cancelled"
    if approval["decision"] != "approve":
        return "rejected"
    if approval["expires_at"] <= data["now"]:
        return "expired"
    if approval["digest"] != proposal_digest(data["proposal"]):
        return "proposal_changed"
    return "completed"


def perform(data, plan, state, tools):
    receipt = None
    # Repeated delivery of this one logical proposal must not repeat its effect.
    if plan == "completed":
        response = call_tool(
            state,
            tools,
            "save_draft",
            {"proposal": data["proposal"], "digest": proposal_digest(data["proposal"])},
        )
        receipt = response["data"]["receipt"]
    return {"status": plan, "writes": int(receipt is not None), "receipt": receipt}


def run(task, model, tools, limits, emit):
    state = load_case(task, model, tools)
    data = state["case"]["input"]
    plan = prepare(data)
    result = perform(data, plan, state, tools)
    return finish(state, result)
