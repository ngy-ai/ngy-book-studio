"""Exercise both reference implementations through real loopback HTTP fixtures.

These deterministic HTTP responses test transport and host integration, not real-model ability.
"""

import json
import threading
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest

from moye_lab import live, runner
from moye_lab.live import LiveConfig
from moye_lab.scenarios import get_scenario

_DEFAULT_FINAL = object()


@contextmanager
def fixture_provider(scenario, final_content=_DEFAULT_FINAL, failure_on_turn=None):
    requests = []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.append({"path": self.path, "body": payload, "headers": dict(self.headers)})
            if failure_on_turn and len(requests) == failure_on_turn[0]:
                _, status, body = failure_on_turn
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                return
            messages = payload["messages"]
            previous_result = (
                json.loads(messages[-1]["content"]) if messages[-1]["role"] == "tool" else None
            )
            error = previous_result.get("error", {}) if previous_result else {}
            if previous_result is None or error.get("code") in {
                "UNKNOWN_TOOL",
                "INVALID_ARGUMENTS",
            }:
                actions = [("search_documents", {"query": scenario.project})]
            elif "documents" in previous_result.get("data", {}):
                actions = [
                    ("read_document", {"document_id": document["document_id"]})
                    for document in previous_result["data"]["documents"]
                ]
            else:
                actions = []
            if actions:
                message = {
                    "role": "assistant",
                    "content": None,
                    "tool_calls": [
                        {
                            "id": f"http_{len(requests)}_{index}",
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": json.dumps(arguments, ensure_ascii=False),
                            },
                        }
                        for index, (name, arguments) in enumerate(actions)
                    ],
                }
            else:
                # A known test answer; this server intentionally is not an LLM.
                message = {
                    "role": "assistant",
                    "content": (
                        json.dumps(scenario.expected_answer, ensure_ascii=False)
                        if final_content is _DEFAULT_FINAL
                        else final_content
                    ),
                }
            response = {
                "choices": [
                    {
                        "index": 0,
                        "finish_reason": "tool_calls" if actions else "stop",
                        "message": message,
                    }
                ],
                "usage": {"prompt_tokens": 7, "completion_tokens": 5, "total_tokens": 12},
            }
            encoded = json.dumps(response, ensure_ascii=False).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(encoded)))
            self.end_headers()
            self.wfile.write(encoded)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=lambda: server.serve_forever(poll_interval=0.01), daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}/v1", requests
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)


@pytest.mark.parametrize("implementation", ["manual", "langgraph"])
@pytest.mark.parametrize("json_mode", [False, True])
@pytest.mark.parametrize(
    "scenario_name,executions",
    [
        ("normal", 3),
        ("transient", 4),
        ("missing", 3),
        ("transfer", 4),
        ("invalid_call", 3),
        ("invalid_arguments", 3),
    ],
)
def test_complete_live_runner_paths(
    monkeypatch, implementation, scenario_name, executions, json_mode
):
    monkeypatch.setattr(live, "read_api_key", lambda _: None)
    scenario = get_scenario(scenario_name)
    with fixture_provider(scenario) as (endpoint, requests):
        report = runner.execute_run(
            implementation,
            scenario,
            mode="live",
            live_config=LiveConfig(
                endpoint,
                "fixture-http-model",
                token_limit_field="max_tokens" if json_mode else "max_completion_tokens",
                json_mode=json_mode,
            ),
        )
    assert report["passed"], (report["error"], report["checks"])
    assert report["outcome"]["answer"] == scenario.expected_answer
    assert report["learning"]["mastery"] is None
    decisions = 4 if scenario_name in {"invalid_call", "invalid_arguments"} else 3
    assert len(requests) == report["metrics"]["model_decisions"] == decisions
    assert report["metrics"]["actual_tool_executions"] == executions
    assert report["metrics"]["provider_usage"] == {
        "prompt_tokens": 7 * decisions,
        "completion_tokens": 5 * decisions,
        "total_tokens": 12 * decisions,
    }
    assert report["metrics"]["provider_usage_complete"] is True
    assert report["request"]["model_mode"] == "live"
    assert report["request"]["model"]["mode"] == "live"
    assert report["request"]["model"]["model"] == "fixture-http-model"
    assert report["request"]["model"]["json_mode"] is json_mode
    token_field = "max_tokens" if json_mode else "max_completion_tokens"
    assert report["request"]["model"]["token_limit_field"] == token_field
    assert all(request["path"] == "/v1/chat/completions" for request in requests)
    assert all(request["body"]["stream"] is False for request in requests)
    assert all(request["body"][token_field] == 2048 for request in requests)
    assert all(
        (request["body"].get("response_format") == {"type": "json_object"}) is json_mode
        for request in requests
    )
    assert all("Authorization" not in request["headers"] for request in requests)
    wire_calls = [
        message["tool_calls"][0]["id"]
        for message in requests[-1]["body"]["messages"]
        if message.get("tool_calls")
    ]
    assert wire_calls and all(call_id.startswith("http_") for call_id in wire_calls)
    checks = {item["id"]: item["passed"] for item in report["checks"]}
    if scenario_name in {"invalid_call", "invalid_arguments"}:
        assert checks["invalid_recovery"]
        injections = report["observations"]["fault_injections"]
        assert len(injections) == 1
        provider_function = injections[0]["provider_response"]["tool_calls"][0]["function"]
        assert provider_function["name"] == "search_documents"
        assert json.loads(provider_function["arguments"]) == {"query": scenario.project}
        exposed_function = injections[0]["exposed_response"]["tool_calls"][0]["function"]
        assert exposed_function != provider_function
        returned_error = json.loads(requests[1]["body"]["messages"][-1]["content"])["error"]
        assert returned_error["code"] == (
            "UNKNOWN_TOOL" if scenario_name == "invalid_call" else "INVALID_ARGUMENTS"
        )
        assert report["observations"]["tool_calls"][0]["actual_execution"] is False
    else:
        assert report["observations"]["fault_injections"] == []
    if scenario_name in {"transient", "transfer"}:
        assert checks["transient_recovery"]
    for missing_id in scenario.missing_ids:
        assert checks[f"missing.{missing_id}"]


def test_live_and_scripted_reports_do_not_conflate_provider_evidence(monkeypatch):
    monkeypatch.setattr(live, "read_api_key", lambda _: None)
    scenario = get_scenario("normal")
    with fixture_provider(scenario) as (endpoint, requests):
        scripted = runner.execute_run("manual", scenario, mode="scripted")
        assert requests == []
        connected = runner.execute_run(
            "manual",
            scenario,
            mode="live",
            live_config=LiveConfig(endpoint, "http-fixture"),
        )
        assert len(requests) == 3
    assert scripted["passed"] and connected["passed"]
    assert scripted["outcome"]["answer"] == connected["outcome"]["answer"]
    assert scripted["request"]["model"]["network"] is False
    assert scripted["request"]["model_mode"] == "scripted"
    assert scripted["metrics"]["provider_usage"] is None
    assert scripted["metrics"]["provider_usage_complete"] is False
    assert connected["metrics"]["provider_usage"]["total_tokens"] == 36
    assert connected["metrics"]["provider_usage_complete"] is True
    assert scripted["observations"]["model_calls"] != connected["observations"]["model_calls"]


@pytest.mark.parametrize("implementation", ["manual", "langgraph"])
@pytest.mark.parametrize(
    "final_content",
    [
        "完成：试运行日期是 2026-10-12。",
        '```json\n{"start_date": "2026-10-12"}\n```',
        '{"start_date":',
        "[]",
        "",
        None,
    ],
)
def test_invalid_final_answer_is_failed_but_preserves_observation_and_usage(
    monkeypatch,
    implementation,
    final_content,
):
    monkeypatch.setattr(live, "read_api_key", lambda _: None)
    scenario = get_scenario("normal")
    with fixture_provider(scenario, final_content=final_content) as (endpoint, requests):
        report = runner.execute_run(
            implementation,
            scenario,
            mode="live",
            live_config=LiveConfig(
                endpoint,
                "http-fixture-invalid-answer",
                token_limit_field="max_tokens",
                json_mode=True,
            ),
        )
    assert report["passed"] is False
    assert report["outcome"]["status"] == "error"
    assert report["outcome"]["answer"] is None
    assert report["error"]["type"] == "ProtocolError"
    assert len(requests) == report["metrics"]["model_decisions"] == 3
    assert report["observations"]["model_calls"][-1]["response"] == {
        "role": "assistant",
        "content": final_content,
    }
    assert report["metrics"]["provider_usage"] == {
        "prompt_tokens": 21,
        "completion_tokens": 15,
        "total_tokens": 36,
    }
    assert report["metrics"]["provider_usage_complete"] is True
    assert report["request"]["model"]["completed_requests"] == 3
    assert report["request"]["model"]["requests_with_usage"] == 3
    assert report["observations"]["successful_reads"].keys() == {"D1", "D2"}
    assert report["learning"]["mastery"] is None


@pytest.mark.parametrize("implementation", ["manual", "langgraph"])
@pytest.mark.parametrize("incorrect_field", ["value", "source_ids"])
def test_json_mode_does_not_bypass_fact_and_source_grading(
    monkeypatch, implementation, incorrect_field
):
    monkeypatch.setattr(live, "read_api_key", lambda _: None)
    scenario = get_scenario("normal")
    answer = scenario.expected_answer
    answer["start_date"][incorrect_field] = "1999-01-01" if incorrect_field == "value" else ["D2"]
    with fixture_provider(scenario, json.dumps(answer)) as (endpoint, requests):
        report = runner.execute_run(
            implementation,
            scenario,
            mode="live",
            live_config=LiveConfig(
                endpoint,
                "http-fixture-wrong-answer",
                token_limit_field="max_tokens",
                json_mode=True,
            ),
        )
    checks = {item["id"]: item["passed"] for item in report["checks"]}
    assert report["passed"] is False
    assert report["outcome"]["status"] == "completed"
    assert checks["observed_final"] and checks["answer_shape"]
    assert (
        checks["start_date.value" if incorrect_field == "value" else "start_date.sources"] is False
    )
    assert report["metrics"]["provider_usage"]["total_tokens"] == 36
    assert report["metrics"]["provider_usage_complete"] is True
    assert report["learning"]["mastery"] is None
    assert all(
        request["body"]["response_format"] == {"type": "json_object"} for request in requests
    )


@pytest.mark.parametrize("implementation", ["manual", "langgraph"])
@pytest.mark.parametrize(
    "status,body",
    [(500, b"fixture-private-http-error"), (200, b'{"choices": invalid-envelope')],
)
def test_transport_failure_marks_accumulated_usage_incomplete(
    monkeypatch, implementation, status, body
):
    monkeypatch.setattr(live, "read_api_key", lambda _: None)
    scenario = get_scenario("normal")
    with fixture_provider(scenario, failure_on_turn=(3, status, body)) as (endpoint, requests):
        report = runner.execute_run(
            implementation,
            scenario,
            mode="live",
            live_config=LiveConfig(endpoint, "http-fixture-error"),
        )
    assert report["passed"] is False
    assert report["outcome"]["status"] == "error"
    assert len(requests) == report["metrics"]["model_decisions"] == 3
    assert report["metrics"]["provider_usage"] == {
        "prompt_tokens": 14,
        "completion_tokens": 10,
        "total_tokens": 24,
    }
    assert report["metrics"]["provider_usage_complete"] is False
    assert report["request"]["model"]["requests_with_usage"] == 2
    assert body.decode() not in json.dumps(report)
