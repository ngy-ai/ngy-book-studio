"""An in-process MCP message subset, NOT a server or interoperability test."""

from __future__ import annotations

import json
from typing import TypedDict

from langgraph.graph import END, START, StateGraph
from langsmith import tracing_context

PROTOCOL = "2025-11-25"
DOCUMENTS = {
    "D1": {"start_date": "2026-10-12"},
    "D2": {"audience": "内部员工", "allowed_operations": "资料检索与阅读"},
}
TOOL = {
    "name": "read_document",
    "description": "读取本次会话授权的虚构资料",
    "inputSchema": {
        "type": "object",
        "properties": {"document_id": {"type": "string"}},
        "required": ["document_id"],
        "additionalProperties": False,
    },
}


class LocalPeer:
    def __init__(self, allowed_ids: set[str]) -> None:
        self.allowed_ids = frozenset(allowed_ids)
        self.phase = "new"

    def receive(self, message: dict) -> dict | None:
        request_id = message.get("id")
        valid_id = not isinstance(request_id, bool) and isinstance(request_id, (str, int))
        if not valid_id:
            request_id = None

        def error(code: int, text: str) -> dict:
            return {"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": text}}

        def result(value: dict) -> dict:
            return {"jsonrpc": "2.0", "id": request_id, "result": value}

        if message.get("jsonrpc") != "2.0" or not isinstance(message.get("method"), str):
            return error(-32600, "Invalid Request")
        method = message["method"]
        if "id" not in message:
            if method == "notifications/initialized" and self.phase == "initializing":
                self.phase = "ready"
            return None
        if not valid_id:
            return error(-32600, "Invalid request id")
        params = message.get("params", {})
        if not isinstance(params, dict):
            return error(-32602, "params must be an object")
        if method == "initialize":
            if self.phase != "new":
                return error(-32600, "Already initialized")
            if (
                not isinstance(params.get("protocolVersion"), str)
                or not isinstance(params.get("capabilities"), dict)
                or not isinstance(params.get("clientInfo"), dict)
            ):
                return error(-32602, "Missing initialization fields")
            self.phase = "initializing"
            return result(
                {
                    "protocolVersion": PROTOCOL,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "songguo-message-demo", "version": "1.0.0"},
                }
            )
        if self.phase != "ready":
            return error(-32600, "Session is not initialized")
        if method == "tools/list":
            return result({"tools": [TOOL]})
        if method != "tools/call":
            return error(-32601, "Method not found")
        if params.get("name") != TOOL["name"]:
            return error(-32602, "Unknown tool")
        args = params.get("arguments", {})
        if not isinstance(args, dict):
            return error(-32602, "arguments must be an object")
        if set(args) != {"document_id"} or not isinstance(args.get("document_id"), str):
            return result(
                {"content": [{"type": "text", "text": "invalid_arguments"}], "isError": True}
            )
        sid = args["document_id"]
        if sid not in self.allowed_ids:
            return result({"content": [{"type": "text", "text": "scope_denied"}], "isError": True})
        if sid not in DOCUMENTS:
            return result({"content": [{"type": "text", "text": "not_found"}], "isError": True})
        value = {"source_id": sid, "facts": DOCUMENTS[sid]}
        return result(
            {
                "content": [{"type": "text", "text": json.dumps(value, ensure_ascii=False)}],
                "structuredContent": value,
                "isError": False,
            }
        )


class LocalClient:
    def __init__(self, supported: tuple[str, ...] = (PROTOCOL,)) -> None:
        self.peer = LocalPeer({"D1", "D2"})
        self.supported = supported
        self.next_id = 1
        self.methods: list[str] = []

    def exchange(self, method: str, params: dict | None = None, *, notification: bool = False):
        message = {"jsonrpc": "2.0", "method": method}
        if not notification:
            message["id"] = self.next_id
            self.next_id += 1
        if params is not None:
            message["params"] = params
        encoded = json.dumps(message, ensure_ascii=False)
        if len(encoded.encode("utf-8")) > 4096:
            raise ValueError("request_too_large")
        self.methods.append(method)
        # JSON round trips simulate a message boundary; no pipe/socket is opened.
        response = self.peer.receive(json.loads(encoded))
        if response is None:
            return None
        wire_response = json.dumps(response, ensure_ascii=False)
        if len(wire_response.encode("utf-8")) > 4096:
            raise ValueError("response_too_large")
        response = json.loads(wire_response)
        if response.get("jsonrpc") != "2.0" or response.get("id") != message.get("id"):
            raise ValueError("response_correlation_failed")
        if ("result" in response) == ("error" in response):
            raise ValueError("invalid_response_envelope")
        return response

    def initialize(self) -> str:
        response = self.exchange(
            "initialize",
            {
                "protocolVersion": self.supported[0],
                "capabilities": {},
                "clientInfo": {"name": "tutorial-client", "version": "1.0.0"},
            },
        )
        version = response["result"]["protocolVersion"]
        if version not in self.supported:
            raise ValueError("unsupported_protocol_version")
        self.exchange("notifications/initialized", notification=True)
        return version


class ProtocolState(TypedDict):
    protocol_version: str
    tool_names: list[str]
    read_result: dict
    denied_result: dict


def run(use_framework: bool) -> dict:
    client = LocalClient()

    def initialize(_: dict) -> dict:
        return {"protocol_version": client.initialize()}

    def discover(_: dict) -> dict:
        response = client.exchange("tools/list")
        return {"tool_names": [tool["name"] for tool in response["result"]["tools"]]}

    def read(_: dict) -> dict:
        ok = client.exchange(
            "tools/call", {"name": "read_document", "arguments": {"document_id": "D1"}}
        )
        denied = client.exchange(
            "tools/call", {"name": "read_document", "arguments": {"document_id": "OTHER_PROJECT"}}
        )
        return {"read_result": ok["result"], "denied_result": denied["result"]}

    state: ProtocolState = {
        "protocol_version": "",
        "tool_names": [],
        "read_result": {},
        "denied_result": {},
    }
    if use_framework:
        graph = StateGraph(ProtocolState)
        for name, node in (("initialize", initialize), ("discover", discover), ("read", read)):
            graph.add_node(name, node)
        graph.add_edge(START, "initialize")
        graph.add_edge("initialize", "discover")
        graph.add_edge("discover", "read")
        graph.add_edge("read", END)
        with tracing_context(enabled=False):
            state = graph.compile().invoke(state)
    else:
        for node in (initialize, discover, read):
            state.update(node(state))
    early = LocalClient().exchange("tools/list")
    try:
        LocalClient(("2099-01-01",)).initialize()
    except ValueError as error:
        incompatible = str(error)
    else:
        raise AssertionError("an unsupported protocol must not be accepted")
    return {
        **state,
        "methods": client.methods,
        "before_initialization_error": early["error"]["code"],
        "incompatible_version": incompatible,
        "boundary": "仅进程内消息子集；无传输、认证、分页或 MCP 互操作验证",
    }


def run_manual() -> dict:
    return run(False)


def run_framework() -> dict:
    return run(True)


if __name__ == "__main__":
    manual, framework = run_manual(), run_framework()
    print(
        json.dumps(
            {"manual": manual, "framework": framework, "equivalent": manual == framework},
            ensure_ascii=False,
            indent=2,
        )
    )
