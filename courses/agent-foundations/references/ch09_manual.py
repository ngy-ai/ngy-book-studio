"""第 9 章 manual 参考示范；阅读或运行示范不代表独立完成。"""

from moye_lab.chapter_support import call_tool, finish, load_case


def prepare(data):
    # This is an in-process protocol client exercise: no socket or MCP server starts.
    return {
        "version": data["protocol_version"],
        "document_id": data["document_id"],
        "preflight": data["preflight"],
    }


def perform(data, plan, state, tools):
    def exchange(method, params, request_id=None):
        message = {"jsonrpc": "2.0", "method": method, "params": params}
        if request_id is not None:
            message["id"] = request_id
        return call_tool(state, tools, "exchange", {"message": message})["data"]

    preflight_error = None
    if plan["preflight"]:
        preflight_error = exchange("tools/list", {}, 0)["error"]["message"]
    response = exchange(
        "initialize",
        {
            "protocolVersion": plan["version"],
            "capabilities": {},
            "clientInfo": {"name": "moye-practice", "version": "1.0.0"},
        },
        1,
    )
    version = response["result"]["protocolVersion"]
    result = {
        "status": "unsupported_protocol_version",
        "version": version,
        "tools": [],
        "facts": None,
        "preflight_error": preflight_error,
    }
    if version != plan["version"]:
        return result
    exchange("notifications/initialized", {})
    listed = exchange("tools/list", {}, 2)
    response = exchange(
        "tools/call",
        {"name": "read_document", "arguments": {"document_id": plan["document_id"]}},
        3,
    )
    result.update(
        status="ready",
        tools=[tool["name"] for tool in listed["result"]["tools"]],
        facts=response["result"]["facts"],
    )
    return result


def run(task, model, tools, limits, emit):
    state = load_case(task, model, tools)
    data = state["case"]["input"]
    plan = prepare(data)
    result = perform(data, plan, state, tools)
    return finish(state, result)
