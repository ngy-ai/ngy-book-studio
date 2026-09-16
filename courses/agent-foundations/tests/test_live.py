"""Transport and credential boundaries; no real provider or OS credential mutations."""

import ctypes
import json
import threading
import time
import traceback
import urllib.error
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest

from ngy_lab import credentials
from ngy_lab.contracts import BudgetExceeded, LabError, ProtocolError
from ngy_lab.live import (
    MAX_ARGUMENT_BYTES,
    MAX_CONTENT_BYTES,
    MAX_RESPONSE_BYTES,
    LiveConfig,
    OpenAICompatibleModel,
    normalize_base_url,
)

TEST_KEY = "test-only-secret-no-account"
SCHEMAS = [
    {
        "type": "function",
        "function": {
            "name": "lookup",
            "parameters": {"type": "object", "properties": {}},
        },
    }
]


def tool_call(call_id="call_1", arguments='{"query":"test"}'):
    return {
        "id": call_id,
        "type": "function",
        "function": {
            "name": "lookup",
            "arguments": arguments,
        },
    }


def completion(content='{"answer":"ok"}', calls=None, finish_reason=None, usage=None):
    message = {"role": "assistant", "content": content}
    if calls is not None:
        message["tool_calls"] = calls
    result = {
        "choices": [
            {
                "index": 0,
                "message": message,
                "finish_reason": finish_reason or ("tool_calls" if calls else "stop"),
            }
        ]
    }
    if usage is not None:
        result["usage"] = usage
    return result


@contextmanager
def mock_endpoint(responses):
    """Each response is (status, body, headers); body may be bytes or a JSON value."""
    requests = []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            size = int(self.headers.get("Content-Length", "0"))
            data = self.rfile.read(size)
            requests.append(
                {"path": self.path, "headers": dict(self.headers), "body": json.loads(data)}
            )
            status, body, headers = responses[len(requests) - 1]
            if not isinstance(body, bytes):
                body = json.dumps(body, ensure_ascii=False).encode("utf-8")
            self.send_response(status)
            combined = {"Content-Type": "application/json", **headers}
            if "Content-Length" not in combined and "Transfer-Encoding" not in combined:
                combined["Content-Length"] = str(len(body))
            for name, value in combined.items():
                self.send_header(name, value)
            self.end_headers()
            try:
                self.wfile.write(body)
            except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=lambda: server.serve_forever(poll_interval=0.01), daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}/v1", requests
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)


def model_for(endpoint, **kwargs):
    return OpenAICompatibleModel(
        LiveConfig(endpoint, "test-model", **kwargs),
        credential_resolver=lambda _: TEST_KEY,
    )


@pytest.mark.parametrize("token_limit_field", ["max_completion_tokens", "max_tokens"])
@pytest.mark.parametrize("json_mode", [False, True])
def test_tool_result_round_trip_and_cumulative_usage(token_limit_field, json_mode):
    usage = {"prompt_tokens": 10, "completion_tokens": 4, "total_tokens": 14}
    with mock_endpoint(
        [
            (200, completion(None, [tool_call()], usage=usage), {}),
            (200, completion(usage=usage), {}),
        ]
    ) as (endpoint, requests):
        model = model_for(
            endpoint,
            max_output_tokens=300,
            token_limit_field=token_limit_field,
            json_mode=json_mode,
            timeout_seconds=60,
        )
        messages = [{"role": "user", "content": "请查询并返回 JSON"}]
        call = model.complete(messages, SCHEMAS)
        assert call["tool_calls"] == [tool_call()]
        tool_result = {"role": "tool", "tool_call_id": "call_1", "content": '{"found":true}'}
        final = model.complete(messages + [call, tool_result], SCHEMAS)
        assert json.loads(final["content"]) == {"answer": "ok"}
        assert requests[1]["body"]["messages"][-2:] == [call, tool_result]
        assert requests[0]["path"] == "/v1/chat/completions"
        assert requests[0]["body"]["stream"] is False
        for request in requests:
            body = request["body"]
            assert body[token_limit_field] == 300
            assert {"max_tokens", "max_completion_tokens"} & body.keys() == {token_limit_field}
            if json_mode:
                assert body["response_format"] == {"type": "json_object"}
            else:
                assert "response_format" not in body
        assert requests[0]["body"]["tools"] == SCHEMAS
        assert requests[0]["headers"]["Authorization"] == f"Bearer {TEST_KEY}"
        assert model.metadata["usage"] == {
            "prompt_tokens": 20,
            "completion_tokens": 8,
            "total_tokens": 28,
        }
        assert model.metadata["completed_requests"] == 2
        assert model.metadata["token_limit_field"] == token_limit_field
        assert model.metadata["json_mode"] is json_mode
        assert model.metadata["timeout_seconds"] == 60
        assert TEST_KEY not in repr(model.metadata)
        assert "credential_target" not in model.metadata
        model.metadata["usage"]["total_tokens"] = -1
        assert model.metadata["usage"]["total_tokens"] == 28


def test_missing_usage_is_unknown_and_no_key_is_allowed():
    with mock_endpoint([(200, completion(), {})]) as (endpoint, requests):
        model = OpenAICompatibleModel(
            LiveConfig(endpoint, "test"), credential_resolver=lambda _: None
        )
        model.complete([{"role": "user", "content": "JSON"}], [])
        assert "Authorization" not in requests[0]["headers"]
        assert model.metadata["usage"] is None
        assert model.metadata["requests_with_usage"] == 0
        assert model.metadata["token_limit_field"] == "max_completion_tokens"
        assert model.metadata["json_mode"] is False
        assert requests[0]["body"]["max_completion_tokens"] == 2048
        assert "max_tokens" not in requests[0]["body"]
        assert "response_format" not in requests[0]["body"]


@pytest.mark.parametrize("token_limit_field", ["max_completion_tokens", "max_tokens"])
def test_rejected_parameter_does_not_retry_or_switch_token_field(token_limit_field):
    with mock_endpoint([(400, {"error": {"message": TEST_KEY}}, {}), (200, completion(), {})]) as (
        endpoint,
        requests,
    ):
        model = model_for(endpoint, token_limit_field=token_limit_field, json_mode=True)
        with pytest.raises(LabError) as error:
            model.complete([{"role": "user", "content": "JSON"}], [])
        assert TEST_KEY not in str(error.value)
        assert len(requests) == 1
        assert requests[0]["body"][token_limit_field] == 2048
        assert model.metadata["completed_requests"] == 0


@pytest.mark.parametrize(
    "url",
    [
        "http://127.0.0.1:11434/v1",
        "http://localhost/v1",
        "http://[::1]/v1",
        "https://127.0.0.1/v1",
        "http://[::ffff:127.0.0.1]/v1",
    ],
)
def test_loopback_does_not_require_remote_confirmation(url):
    LiveConfig(url, "model")


@pytest.mark.parametrize(
    "url", ["https://provider.example/v1", "http://provider.example/v1", "http://192.168.1.2/v1"]
)
def test_remote_requires_separate_explicit_authorizations(url):
    with pytest.raises(LabError, match="远程"):
        LiveConfig(url, "model")
    if url.startswith("http:"):
        with pytest.raises(LabError, match="明文"):
            LiveConfig(url, "model", allow_remote=True)
    LiveConfig(url, "model", allow_remote=True, allow_insecure=True)


@pytest.mark.parametrize(
    "url",
    [
        "http://user:secret@example.com/v1",
        "http://@127.0.0.1/v1",
        "http://127.0.0.1/v1?key=secret",
        "http://127.0.0.1/v1#secret",
        "http://127.0.0.1/v1?",
        "http://127.0.0.1/v1#",
        "file:///tmp/model",
        "http://127.0.0.1:99999/v1",
        "http://127.0.0.1:0/v1",
        "http://[::1/v1",
        " http://127.0.0.1/v1",
        "http://127.0.0.1/\nsecret",
        "http://127.0.0.1\\@evil.example",
        "http://%31%32%37.0.0.1/v1",
        "http://127.0.0.1/../v1",
        "http://127.0.0.1/%2e%2e/v1",
    ],
)
def test_ambiguous_or_credential_bearing_urls_are_rejected(url):
    with pytest.raises(LabError) as error:
        LiveConfig(url, "model", allow_remote=True, allow_insecure=True)
    assert url not in str(error.value)
    assert "secret" not in str(error.value)


def test_endpoint_identity_is_canonical_and_scoped():
    assert normalize_base_url("HTTP://LOCALHOST:80/v1/") == "http://localhost/v1"
    target = credentials.target_for_endpoint("http://LOCALHOST:80/v1/")
    assert target == credentials.target_for_endpoint("http://localhost/v1")
    assert target.startswith(credentials.NAMESPACE)
    assert "localhost" not in target
    assert target != credentials.target_for_endpoint("http://localhost/other")
    with pytest.raises(LabError, match="当前模型端点"):
        LiveConfig("http://localhost/other", "model", credential_target=target)


@pytest.mark.parametrize(
    "override",
    [
        {"model": ""},
        {"model": "model\nsecret"},
        {"timeout_seconds": 0},
        {"timeout_seconds": float("nan")},
        {"timeout_seconds": float("inf")},
        {"timeout_seconds": 121},
        {"max_output_tokens": 4097},
        {"max_output_tokens": True},
        {"token_limit_field": "max_output_tokens"},
        {"token_limit_field": "MAX_TOKENS"},
        {"token_limit_field": "max_tokens "},
        {"token_limit_field": None},
        {"token_limit_field": ["max_tokens"]},
        {"token_limit_field": True},
        {"json_mode": "true"},
        {"json_mode": 1},
        {"json_mode": None},
        {"allow_remote": "yes"},
        {"allow_insecure": 1},
    ],
)
def test_invalid_configuration_is_rejected(override):
    with pytest.raises(LabError):
        LiveConfig(**{"base_url": "http://localhost/v1", "model": "test", **override})


def test_no_environment_proxy_is_used(monkeypatch):
    monkeypatch.setenv("HTTP_PROXY", "http://127.0.0.1:1")
    monkeypatch.setenv("http_proxy", "http://127.0.0.1:1")
    monkeypatch.setenv("NO_PROXY", "")
    monkeypatch.setenv("no_proxy", "")
    with mock_endpoint([(200, completion(), {})]) as (endpoint, requests):
        model_for(endpoint).complete([{"role": "user", "content": "JSON"}], [])
        assert len(requests) == 1


@pytest.mark.parametrize("status", [301, 302, 303, 307, 308])
def test_redirects_never_reach_destination(status):
    with mock_endpoint([(200, completion(), {})]) as (destination, destination_requests):
        with mock_endpoint([(status, b"", {"Location": destination + "/chat/completions"})]) as (
            endpoint,
            _,
        ):
            with pytest.raises(LabError):
                model_for(endpoint).complete([{"role": "user", "content": "private fixture"}], [])
        assert destination_requests == []


@pytest.mark.parametrize(
    "status,body,headers",
    [
        (401, ("provider echoed " + TEST_KEY).encode(), {}),
        (500, {"error": {"message": TEST_KEY}}, {}),
        (200, b'{"choices": secret invalid JSON}', {}),
        (200, {"error": {"message": TEST_KEY}}, {}),
        (200, completion(), {"Content-Type": "text/html"}),
        (200, completion(), {"Content-Encoding": "gzip"}),
        (200, completion(), {"Content-Length": str(MAX_RESPONSE_BYTES + 1)}),
        (200, completion(), {"Content-Length": "invalid-secret"}),
    ],
)
def test_http_and_protocol_errors_do_not_echo_secrets(status, body, headers):
    with mock_endpoint([(status, body, headers)]) as (endpoint, _):
        try:
            model_for(endpoint).complete([{"role": "user", "content": "JSON"}], [])
        except LabError as error:
            assert TEST_KEY not in str(error)
            assert TEST_KEY not in traceback.format_exc()
            assert "invalid-secret" not in str(error)
        else:
            pytest.fail("expected a sanitized error")


@pytest.mark.parametrize(
    "payload",
    [
        [],
        {},
        {"choices": []},
        {"choices": [None]},
        {"choices": [1, 2]},
        completion(finish_reason="length"),
        completion(finish_reason="content_filter"),
        completion(finish_reason="function_call"),
        completion(content=[]),
        completion(None, [tool_call()], finish_reason="stop"),
        completion(calls=[], finish_reason="tool_calls"),
        completion(calls="bad"),
        completion(calls=False),
        completion(None, [tool_call("same"), tool_call("same")]),
        completion(None, [{"type": "custom", "id": "one", "function": {}}]),
        completion(
            None,
            [
                {
                    "id": "one",
                    "type": "function",
                    "function": {"name": "bad name", "arguments": "{}"},
                }
            ],
        ),
        completion(None, [tool_call(arguments={})]),
        completion(usage={"prompt_tokens": -1, "completion_tokens": 1, "total_tokens": 0}),
        completion(usage={"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 3}),
        completion(usage={"prompt_tokens": True, "completion_tokens": 1, "total_tokens": 2}),
    ],
)
def test_malformed_response_shapes_fail_closed(payload):
    with mock_endpoint([(200, payload, {})]) as (endpoint, _):
        with pytest.raises(ProtocolError):
            model_for(endpoint).complete([{"role": "user", "content": "JSON"}], [])


def test_outer_duplicate_json_keys_are_rejected():
    payload = b'{"choices": [], "choices": []}'
    with mock_endpoint([(200, payload, {})]) as (endpoint, _):
        with pytest.raises(ProtocolError):
            model_for(endpoint).complete([{"role": "user", "content": "JSON"}], [])


@pytest.mark.parametrize(
    "content",
    [
        "plain text instead of JSON",
        '```json\n{"answer":"ok"}\n```',
        "[]",
        "null",
        "NaN",
        '{"x":1e999}',
        '{"x":1,"x":2}',
        '{"x":"\\ud800"}',
        "",
        None,
    ],
)
def test_final_content_and_usage_reach_host_before_answer_validation(content):
    usage = {"prompt_tokens": 7, "completion_tokens": 5, "total_tokens": 12}
    with mock_endpoint([(200, completion(content, usage=usage), {})]) as (endpoint, _):
        model = model_for(endpoint)
        response = model.complete([{"role": "user", "content": "JSON"}], [])
    assert response == {"role": "assistant", "content": content}
    assert model.usage == usage
    assert model.metadata["completed_requests"] == 1


def test_tool_argument_json_is_left_to_tool_validator():
    with mock_endpoint([(200, completion(None, [tool_call(arguments="invalid-json")]), {})]) as (
        endpoint,
        _,
    ):
        result = model_for(endpoint).complete([{"role": "user", "content": "JSON"}], SCHEMAS)
        assert result["tool_calls"][0]["function"]["arguments"] == "invalid-json"


@pytest.mark.parametrize("count,accepted", [(4, True), (5, False)])
def test_tool_call_count_limit(count, accepted):
    payload = completion(None, [tool_call(f"call_{number}") for number in range(count)])
    with mock_endpoint([(200, payload, {})]) as (endpoint, _):
        if accepted:
            assert (
                len(
                    model_for(endpoint).complete([{"role": "user", "content": "JSON"}], SCHEMAS)[
                        "tool_calls"
                    ]
                )
                == 4
            )
        else:
            with pytest.raises(ProtocolError):
                model_for(endpoint).complete([{"role": "user", "content": "JSON"}], SCHEMAS)


@pytest.mark.parametrize("extra", [0, 1])
def test_tool_argument_byte_limit(extra):
    # Non-ASCII checks enforce bytes rather than Python character count.
    arguments = "界" * (MAX_ARGUMENT_BYTES // 3) + "x" * (MAX_ARGUMENT_BYTES % 3 + extra)
    with mock_endpoint([(200, completion(None, [tool_call(arguments=arguments)]), {})]) as (
        endpoint,
        _,
    ):
        if extra:
            with pytest.raises(ProtocolError):
                model_for(endpoint).complete([{"role": "user", "content": "JSON"}], SCHEMAS)
        else:
            assert model_for(endpoint).complete([{"role": "user", "content": "JSON"}], SCHEMAS)[
                "tool_calls"
            ]


@pytest.mark.parametrize("extra", [0, 1])
def test_content_byte_limit(extra):
    content = '{"x":"' + "x" * (MAX_CONTENT_BYTES - 8 + extra) + '"}'
    assert len(content) == MAX_CONTENT_BYTES + extra
    with mock_endpoint([(200, completion(content), {})]) as (endpoint, _):
        if extra:
            with pytest.raises(ProtocolError):
                model_for(endpoint).complete([{"role": "user", "content": "JSON"}], [])
        else:
            assert (
                model_for(endpoint).complete([{"role": "user", "content": "JSON"}], [])["content"]
                == content
            )


def test_response_total_limit_without_content_length():
    chunk = b"x" * (MAX_RESPONSE_BYTES + 1)
    body = f"{len(chunk):x}\r\n".encode() + chunk + b"\r\n0\r\n\r\n"
    with mock_endpoint([(200, body, {"Transfer-Encoding": "chunked"})]) as (endpoint, _):
        with pytest.raises(ProtocolError, match="1 MiB"):
            model_for(endpoint).complete([{"role": "user", "content": "JSON"}], [])


def test_declared_length_must_match_body():
    with mock_endpoint([(200, completion(), {"Content-Length": "2000"})]) as (endpoint, _):
        with pytest.raises(ProtocolError, match="不完整"):
            model_for(endpoint).complete([{"role": "user", "content": "JSON"}], [])


@pytest.mark.parametrize(
    "messages,schemas",
    [
        ([], []),
        ([{"role": "invalid", "content": "x"}], []),
        ([{"role": "user", "content": ["not text"]}], []),
        ([{"role": "user", "content": "x" * (MAX_CONTENT_BYTES + 1)}], []),
        ([{"role": "user", "content": "JSON"}], [{"type": "function", "function": {}}]),
        ([{"role": "user", "content": "JSON"}], [{"type": "custom"}]),
        ([{"role": "user", "content": "x" * MAX_CONTENT_BYTES}] * 9, []),
    ],
)
def test_invalid_input_fails_before_network_or_credential_lookup(messages, schemas):
    def no_credentials(_):
        pytest.fail("invalid input should not look up credentials")

    model = OpenAICompatibleModel(
        LiveConfig("http://127.0.0.1:1/v1", "test"), credential_resolver=no_credentials
    )
    with pytest.raises(ProtocolError):
        model.complete(messages, schemas)


def test_resolver_failure_and_header_injection_are_sanitized():
    def broken_resolver(_):
        raise RuntimeError(TEST_KEY)

    for resolver in (broken_resolver, lambda _: TEST_KEY + "\r\nX-Evil: 1"):
        model = OpenAICompatibleModel(
            LiveConfig("http://127.0.0.1:1/v1", "test"), credential_resolver=resolver
        )
        with pytest.raises(LabError) as error:
            model.complete([{"role": "user", "content": "JSON"}], [])
        assert TEST_KEY not in str(error.value)


def test_credential_namespace_cannot_access_reader_or_arbitrary_targets(monkeypatch):
    monkeypatch.setattr(
        credentials, "_native_api", lambda: pytest.fail("must reject before native access")
    )
    for target in (
        "dev.ngy.book-studio.openai-compatible",
        "other-app",
        "",
        credentials.NAMESPACE,
    ):
        for operation in (
            lambda: credentials.read_api_key(target),
            lambda: credentials.write_api_key(target, TEST_KEY),
            lambda: credentials.delete_api_key(target),
        ):
            with pytest.raises(LabError):
                operation()


def test_credential_native_lifecycle_with_injected_fake(monkeypatch):
    class NativeFake:
        def __init__(self):
            self.saved = None
            self.freed = 0

        def CredWriteW(self, pointer, flags):
            entry = ctypes.cast(pointer, ctypes.POINTER(credentials._Credential)).contents
            assert entry.Type == 1 and entry.Persist == 2 and flags == 0
            self.saved = ctypes.string_at(entry.CredentialBlob, entry.CredentialBlobSize)
            self.target = entry.TargetName
            return True

        def CredReadW(self, target, kind, flags, output):
            assert target == self.target and kind == 1 and flags == 0
            self.blob = (ctypes.c_ubyte * len(self.saved)).from_buffer_copy(self.saved)
            self.entry = credentials._Credential(
                CredentialBlobSize=len(self.saved),
                CredentialBlob=ctypes.cast(self.blob, ctypes.POINTER(ctypes.c_ubyte)),
            )
            ctypes.cast(output, ctypes.POINTER(ctypes.POINTER(credentials._Credential)))[0] = (
                ctypes.pointer(self.entry)
            )
            return True

        def CredFree(self, _):
            self.freed += 1

        def CredDeleteW(self, target, kind, flags):
            assert target == self.target and kind == 1 and flags == 0
            self.saved = None
            return True

    api = NativeFake()
    monkeypatch.setattr(credentials, "_native_api", lambda: api)
    target = credentials.target_for_endpoint("http://localhost/v1")
    credentials.write_api_key(target, TEST_KEY)
    assert credentials.read_api_key(target) == TEST_KEY
    assert api.freed == 1
    api.saved = b"bad\r\nkey"
    with pytest.raises(LabError):
        credentials.read_api_key(target)
    assert api.freed == 2
    credentials.delete_api_key(target)
    assert api.saved is None


def test_missing_credentials_and_idempotent_delete_with_fake(monkeypatch):
    class Missing:
        def CredReadW(self, *_):
            return False

        def CredDeleteW(self, *_):
            return False

    monkeypatch.setattr(credentials, "_native_api", lambda: Missing())
    monkeypatch.setattr(ctypes, "get_last_error", lambda: 1168, raising=False)
    target = credentials.target_for_endpoint("http://localhost/v1")
    assert credentials.read_api_key(target) is None
    credentials.delete_api_key(target)
    monkeypatch.setattr(ctypes, "get_last_error", lambda: 5)
    with pytest.raises(LabError):
        credentials.read_api_key(target)
    with pytest.raises(LabError):
        credentials.delete_api_key(target)


@pytest.mark.parametrize("host_deadline", [False, True])
def test_slow_response_times_out(host_deadline):
    class SlowHandler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            self.rfile.read(int(self.headers["Content-Length"]))
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", "100")
            self.end_headers()
            time.sleep(0.3)
            try:
                self.wfile.write(b"{}")
            except OSError:
                pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), SlowHandler)
    thread = threading.Thread(target=lambda: server.serve_forever(poll_interval=0.01), daemon=True)
    thread.start()
    try:
        model = model_for(
            f"http://127.0.0.1:{server.server_port}/v1",
            timeout_seconds=1 if host_deadline else 0.05,
        )
        if host_deadline:
            model.set_deadline(time.monotonic() + 0.05)
        start = time.monotonic()
        with pytest.raises(BudgetExceeded if host_deadline else LabError) as error:
            model.complete([{"role": "user", "content": "JSON"}], [])
        if host_deadline:
            assert error.value.reason == "deadline"
        assert time.monotonic() - start < 0.25
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)


def test_expired_deadline_prevents_credential_lookup_and_cannot_be_extended():
    def no_lookup(_):
        pytest.fail("expired requests must not access credentials")

    model = OpenAICompatibleModel(
        LiveConfig("http://127.0.0.1:1/v1", "test"),
        credential_resolver=no_lookup,
    )
    model.set_deadline(time.monotonic() - 1)
    model.set_deadline(time.monotonic() + 10)
    with pytest.raises(BudgetExceeded) as error:
        model.complete([{"role": "user", "content": "JSON"}], [])
    assert error.value.reason == "deadline"


@pytest.mark.parametrize("wrapped", [False, True])
@pytest.mark.parametrize("host_limited", [False, True])
def test_early_socket_timeout_is_classified_by_effective_limit(monkeypatch, wrapped, host_limited):
    model = model_for("http://127.0.0.1:1/v1", timeout_seconds=60)
    deadline = time.monotonic() + (30 if host_limited else 120)
    model.set_deadline(deadline)

    def early_timeout(request, *, timeout):
        assert time.monotonic() < deadline
        assert 0 < timeout <= (30 if host_limited else 60)
        error = TimeoutError("test-only-early-timeout")
        raise urllib.error.URLError(error) if wrapped else error

    monkeypatch.setattr(model._opener, "open", early_timeout)
    with pytest.raises(BudgetExceeded if host_limited else LabError) as error:
        model.complete([{"role": "user", "content": "JSON"}], [])
    if host_limited:
        assert error.value.reason == "deadline"
    else:
        assert not isinstance(error.value, BudgetExceeded)
    assert "test-only-early-timeout" not in str(error.value)
    assert model.metadata["completed_requests"] == 0


@pytest.mark.parametrize("wrapped", [False, True])
def test_connection_refusal_before_host_deadline_stays_transport_error(monkeypatch, wrapped):
    model = model_for("http://127.0.0.1:1/v1", timeout_seconds=60)
    model.set_deadline(time.monotonic() + 30)

    def refuse_connection(request, *, timeout):
        error = ConnectionRefusedError("test-only-refused")
        raise urllib.error.URLError(error) if wrapped else error

    monkeypatch.setattr(model._opener, "open", refuse_connection)
    with pytest.raises(LabError) as error:
        model.complete([{"role": "user", "content": "JSON"}], [])
    assert not isinstance(error.value, BudgetExceeded)
    assert "test-only-refused" not in str(error.value)


@pytest.mark.parametrize("deadline", [None, "soon", True, float("nan"), float("inf")])
def test_invalid_host_deadline_is_rejected(deadline):
    with pytest.raises(LabError):
        model_for("http://127.0.0.1:1/v1").set_deadline(deadline)


def test_provider_cannot_report_more_completion_tokens_than_requested():
    usage = {"prompt_tokens": 1, "completion_tokens": 11, "total_tokens": 12}
    with mock_endpoint([(200, completion(usage=usage), {})]) as (endpoint, _):
        with pytest.raises(ProtocolError, match="token"):
            model_for(endpoint, max_output_tokens=10).complete(
                [{"role": "user", "content": "JSON"}],
                [],
            )
