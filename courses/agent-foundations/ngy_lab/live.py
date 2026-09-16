"""Bounded, non-streaming Chat Completions adapter using only the Python stdlib.

The tool schema and tool-result message format follow the official function-calling
guide: https://developers.openai.com/api/docs/guides/function-calling
Remote content transfer is explicit. Redirects and inherited proxies are disabled.
"""

import http.client
import ipaddress
import json
import math
import re
import socket
import time
import urllib.error
import urllib.parse
import urllib.request
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

from .contracts import BudgetExceeded, LabError, Message, ProtocolError
from .credentials import read_api_key, target_for_endpoint, validate_api_key, validate_target

MAX_RESPONSE_BYTES = 1024 * 1024
MAX_REQUEST_BYTES = 512 * 1024
MAX_CONTENT_BYTES = 64 * 1024
MAX_ARGUMENT_BYTES = 4096
MAX_TOOL_CALLS = 4
_NAME_PATTERN = re.compile(r"[A-Za-z0-9_-]{1,64}\Z")
_CALL_ID_PATTERN = re.compile(r"[A-Za-z0-9_-]{1,128}\Z")


def normalize_base_url(base_url: str) -> str:
    """Return an unambiguous API base URL without granting network permission."""
    if (
        not isinstance(base_url, str)
        or not base_url
        or len(base_url) > 2048
        or any(ord(c) <= 32 or ord(c) >= 127 for c in base_url)
        or "\\" in base_url
        or "?" in base_url
        or "#" in base_url
    ):
        raise LabError("模型端点必须为不含空白、凭据、查询或片段的 HTTP(S) URL")
    try:
        parsed = urllib.parse.urlsplit(base_url)
        port = parsed.port
        hostname = parsed.hostname
    except ValueError:
        raise LabError("模型端点 URL 无效") from None
    if (
        parsed.scheme not in ("http", "https")
        or not hostname
        or parsed.username is not None
        or parsed.password is not None
        or "%" in hostname
        or (port is not None and not 1 <= port <= 65535)
    ):
        raise LabError("模型端点必须为不含凭据的 HTTP(S) URL")
    hostname = hostname.lower().rstrip(".")
    try:
        address = ipaddress.ip_address(hostname)
        hostname = address.compressed
        authority = f"[{hostname}]" if address.version == 6 else hostname
    except ValueError:
        if not re.fullmatch(r"[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?", hostname):
            raise LabError("模型端点主机名无效") from None
        authority = hostname
    path = parsed.path.rstrip("/")
    if "%" in path or any(part in (".", "..") for part in path.split("/")):
        raise LabError("模型端点路径不允许转义或相对路径片段")
    if port is not None and port != (443 if parsed.scheme == "https" else 80):
        authority += f":{port}"
    return urllib.parse.urlunsplit((parsed.scheme, authority, path, "", ""))


def _is_loopback(base_url: str) -> bool:
    hostname = urllib.parse.urlsplit(base_url).hostname
    if hostname == "localhost":
        return True
    try:
        address = ipaddress.ip_address(hostname or "")
        if isinstance(address, ipaddress.IPv6Address) and address.ipv4_mapped:
            return address.ipv4_mapped.is_loopback
        return address.is_loopback
    except ValueError:
        return False


@dataclass(frozen=True)
class LiveConfig:
    base_url: str
    model: str
    allow_remote: bool = False
    allow_insecure: bool = False
    timeout_seconds: float = 30.0
    credential_target: str | None = None
    max_output_tokens: int = 2048
    token_limit_field: str = "max_completion_tokens"
    json_mode: bool = False

    def __post_init__(self):
        endpoint = normalize_base_url(self.base_url)
        object.__setattr__(self, "base_url", endpoint)
        if (
            not isinstance(self.model, str)
            or not self.model
            or len(self.model) > 256
            or any(ord(c) <= 32 or ord(c) >= 127 for c in self.model)
        ):
            raise LabError("必须显式指定不含空白的模型名称")
        if type(self.allow_remote) is not bool or type(self.allow_insecure) is not bool:
            raise LabError("远程端点授权必须为布尔值")
        if not _is_loopback(endpoint):
            if not self.allow_remote:
                raise LabError("远程模型会接收本次材料，请显式允许远程端点")
            if endpoint.startswith("http:") and not self.allow_insecure:
                raise LabError("远程 HTTP 会明文传输材料和密钥，请另行显式允许不安全连接")
        if (
            isinstance(self.timeout_seconds, bool)
            or not isinstance(self.timeout_seconds, (float, int))
            or not math.isfinite(self.timeout_seconds)
            or not 0 < self.timeout_seconds <= 120
        ):
            raise LabError("单次模型超时必须在 (0, 120] 秒内")
        if type(self.max_output_tokens) is not int or not 1 <= self.max_output_tokens <= 4096:
            raise LabError("模型输出预算必须为 1..4096 token")
        if type(self.token_limit_field) is not str or self.token_limit_field not in (
            "max_completion_tokens",
            "max_tokens",
        ):
            raise LabError("模型 token 限制字段必须为 max_completion_tokens 或 max_tokens")
        if type(self.json_mode) is not bool:
            raise LabError("模型 JSON 模式必须为布尔值")
        if self.credential_target is not None:
            validate_target(self.credential_target)
            if self.credential_target != target_for_endpoint(endpoint):
                raise LabError("课程 API 密钥必须属于当前模型端点")


class _RejectRedirects(urllib.request.HTTPRedirectHandler):
    def http_error_302(self, request, response, code, message, headers):
        response.close()
        raise LabError("模型端点返回重定向；请直接配置目标 API 端点")

    http_error_301 = http_error_302
    http_error_303 = http_error_302
    http_error_307 = http_error_302
    http_error_308 = http_error_302


def _byte_length(value: str) -> int:
    try:
        return len(value.encode("utf-8"))
    except UnicodeError:
        raise ProtocolError("模型消息含有无效 Unicode") from None


def _strict_json(value: str | bytes) -> Any:
    def object_pairs(pairs):
        output = {}
        for key, item in pairs:
            if key in output:
                raise ValueError("duplicate JSON key")
            output[key] = item
        return output

    def reject_constant(_):
        raise ValueError("non-finite JSON value")

    def finite_float(number):
        parsed = float(number)
        if not math.isfinite(parsed):
            raise ValueError("non-finite JSON number")
        return parsed

    try:
        if isinstance(value, bytes):
            value = value.decode("utf-8")
        parsed = json.loads(
            value,
            object_pairs_hook=object_pairs,
            parse_constant=reject_constant,
            parse_float=finite_float,
        )
        pending = [parsed]
        while pending:
            item = pending.pop()
            if isinstance(item, str):
                item.encode("utf-8")
            elif isinstance(item, dict):
                pending.extend(item.keys())
                pending.extend(item.values())
            elif isinstance(item, list):
                pending.extend(item)
        return parsed
    except (ValueError, UnicodeError, RecursionError):
        raise ProtocolError("模型返回的 JSON 无效") from None


def _validate_calls(calls: Any) -> list[dict[str, Any]]:
    if not isinstance(calls, list) or not 1 <= len(calls) <= MAX_TOOL_CALLS:
        raise ProtocolError("每轮模型必须返回 1..4 个工具调用")
    result = []
    seen = set()
    for call in calls:
        if not isinstance(call, dict) or call.get("type") != "function":
            raise ProtocolError("模型工具调用类型无效")
        call_id = call.get("id")
        function = call.get("function")
        if (
            not isinstance(call_id, str)
            or not _CALL_ID_PATTERN.fullmatch(call_id)
            or call_id in seen
            or not isinstance(function, dict)
        ):
            raise ProtocolError("模型工具调用 ID 或 function 结构无效")
        name, arguments = function.get("name"), function.get("arguments")
        if not isinstance(name, str) or not _NAME_PATTERN.fullmatch(name):
            raise ProtocolError("模型工具名称无效")
        if not isinstance(arguments, str) or _byte_length(arguments) > MAX_ARGUMENT_BYTES:
            raise ProtocolError("模型工具参数必须为不超过 4096 字节的字符串")
        # Tools.execute owns JSON/schema validation and recoverable error feedback.
        result.append(
            {
                "id": call_id,
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": arguments,
                },
            }
        )
        seen.add(call_id)
    return result


def _parse_response(payload: bytes) -> tuple[Message, dict[str, int] | None]:
    data = _strict_json(payload)
    if not isinstance(data, dict) or data.get("error") is not None:
        raise ProtocolError("模型响应外壳无效或包含服务错误")
    choices = data.get("choices")
    if not isinstance(choices, list) or len(choices) != 1 or not isinstance(choices[0], dict):
        raise ProtocolError("模型响应必须恰好包含一个 choice")
    choice = choices[0]
    reason = choice.get("finish_reason")
    if reason not in ("stop", "tool_calls"):
        raise ProtocolError("模型响应未正常完成，可能被截断、过滤或使用了不支持的协议")
    source = choice.get("message")
    if not isinstance(source, dict) or source.get("role") != "assistant":
        raise ProtocolError("模型响应必须包含 assistant 消息")
    if source.get("refusal") is not None or source.get("function_call") is not None:
        raise ProtocolError("模型拒绝请求或返回了不支持的旧工具协议")
    content = source.get("content")
    if content is not None and (
        not isinstance(content, str) or _byte_length(content) > MAX_CONTENT_BYTES
    ):
        raise ProtocolError("模型正文必须为不超过 64 KiB 的文本")
    calls = source.get("tool_calls")
    if calls:
        if reason != "tool_calls":
            raise ProtocolError("模型 finish_reason 与工具调用不一致")
        message = {"role": "assistant", "content": content, "tool_calls": _validate_calls(calls)}
    else:
        if calls is not None and not isinstance(calls, list):
            raise ProtocolError("模型 tool_calls 结构无效")
        if reason != "stop":
            raise ProtocolError("模型 finish_reason 与空工具调用不一致")
        # Preserve the actual final content before the host applies its answer
        # contract. Invalid answer JSON is learning evidence, not an HTTP error.
        message = {"role": "assistant", "content": content}
    usage = data.get("usage")
    if usage is not None:
        if not isinstance(usage, dict):
            raise ProtocolError("模型 usage 结构无效")
        names = ("prompt_tokens", "completion_tokens", "total_tokens")
        if any(type(usage.get(name)) is not int or not 0 <= usage[name] <= 10**9 for name in names):
            raise ProtocolError("模型 token 用量无效")
        if usage["total_tokens"] != usage["prompt_tokens"] + usage["completion_tokens"]:
            raise ProtocolError("模型 token 用量总数不一致")
        usage = {name: usage[name] for name in names}
    return message, usage


class OpenAICompatibleModel:
    def __init__(
        self,
        config: LiveConfig,
        *,
        credential_resolver: Callable[[str], str | None] | None = None,
    ):
        if not isinstance(config, LiveConfig):
            raise LabError("模型连接必须使用经过验证的 LiveConfig")
        self.config = config
        self._credential_resolver = credential_resolver or read_api_key
        self._opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({}), _RejectRedirects()
        )
        self._usage = {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}
        self._completed_requests = 0
        self._requests_with_usage = 0
        self._deadline: float | None = None

    def set_deadline(self, monotonic_deadline: float) -> None:
        """Restrict this model to the host's absolute monotonic deadline."""
        if (
            isinstance(monotonic_deadline, bool)
            or not isinstance(monotonic_deadline, (int, float))
            or not math.isfinite(monotonic_deadline)
        ):
            raise LabError("宿主截止时间必须为有效的 monotonic 时间")
        self._deadline = (
            monotonic_deadline
            if self._deadline is None
            else min(self._deadline, monotonic_deadline)
        )

    def _check_host_deadline(self) -> None:
        if self._deadline is not None and time.monotonic() >= self._deadline:
            raise BudgetExceeded("deadline")

    def _timeout_error(self) -> None:
        self._check_host_deadline()
        raise LabError("模型请求超时，请缩小输入或检查服务")

    @property
    def usage(self) -> dict[str, int] | None:
        """Only provider-reported usage, never an estimated token count."""
        return dict(self._usage) if self._requests_with_usage else None

    @property
    def metadata(self) -> dict[str, Any]:
        return {
            "mode": "live",
            "provider": "openai-compatible",
            "base_url": self.config.base_url,
            "model": self.config.model,
            "max_output_tokens": self.config.max_output_tokens,
            "token_limit_field": self.config.token_limit_field,
            "json_mode": self.config.json_mode,
            "timeout_seconds": self.config.timeout_seconds,
            "completed_requests": self._completed_requests,
            "requests_with_usage": self._requests_with_usage,
            "usage": self.usage,
        }

    def complete(self, messages: list[Message], schemas: list[dict[str, Any]]) -> Message:
        self._check_host_deadline()
        if not isinstance(messages, list) or not 1 <= len(messages) <= 128:
            raise ProtocolError("模型输入必须为 1..128 条消息")
        if not isinstance(schemas, list) or len(schemas) > 32:
            raise ProtocolError("模型工具定义必须为最多 32 项的数组")
        for schema in schemas:
            if (
                not isinstance(schema, dict)
                or schema.get("type") != "function"
                or not isinstance(schema.get("function"), dict)
            ):
                raise ProtocolError("模型工具定义必须符合 function schema 外壳")
            function = schema["function"]
            if (
                not isinstance(function.get("name"), str)
                or not _NAME_PATTERN.fullmatch(function["name"])
                or not isinstance(function.get("parameters"), dict)
                or function["parameters"].get("type") != "object"
            ):
                raise ProtocolError("模型工具定义必须包含有效名称和 object 参数 schema")
        for message in messages:
            if not isinstance(message, dict) or message.get("role") not in (
                "system",
                "user",
                "assistant",
                "tool",
            ):
                raise ProtocolError("模型输入消息角色无效")
            content = message.get("content")
            if content is not None and (
                not isinstance(content, str) or _byte_length(content) > MAX_CONTENT_BYTES
            ):
                raise ProtocolError("模型输入正文必须为不超过 64 KiB 的文本")
        body = {
            "model": self.config.model,
            "messages": messages,
            "stream": False,
            "n": 1,
            self.config.token_limit_field: self.config.max_output_tokens,
        }
        if self.config.json_mode:
            body["response_format"] = {"type": "json_object"}
        if schemas:
            body.update(tools=schemas, tool_choice="auto")
        try:
            encoded = json.dumps(
                body, ensure_ascii=False, allow_nan=False, separators=(",", ":")
            ).encode("utf-8")
        except (TypeError, ValueError, UnicodeError, RecursionError):
            raise ProtocolError("模型请求包含不可序列化的数据") from None
        if len(encoded) > MAX_REQUEST_BYTES:
            raise ProtocolError("模型请求超过 512 KiB 上限")
        try:
            key = self._credential_resolver(
                self.config.credential_target or target_for_endpoint(self.config.base_url)
            )
            if key is not None:
                validate_api_key(key)
        except Exception:
            raise LabError("无法读取课程 API 密钥，请检查课程凭据设置") from None
        headers = {
            "Content-Type": "application/json",
            "Accept": "application/json",
            "Accept-Encoding": "identity",
        }
        if key is not None:
            headers["Authorization"] = f"Bearer {key}"
        request = urllib.request.Request(
            self.config.base_url + "/chat/completions",
            data=encoded,
            headers=headers,
            method="POST",
        )
        started = time.monotonic()
        self._check_host_deadline()
        expires_at = started + self.config.timeout_seconds
        host_limited = self._deadline is not None and self._deadline <= expires_at
        if host_limited:
            expires_at = self._deadline
        try:
            with self._opener.open(request, timeout=expires_at - started) as response:
                if response.status != 200:
                    raise LabError("模型端点未返回 HTTP 200")
                if response.headers.get("Content-Encoding", "identity").lower() != "identity":
                    raise ProtocolError("模型响应使用了不支持的压缩编码")
                if response.headers.get_content_type() != "application/json":
                    raise ProtocolError("模型端点必须返回 application/json")
                declared_size = response.headers.get("Content-Length")
                if declared_size is not None:
                    if (
                        not declared_size.isascii()
                        or not declared_size.isdigit()
                        or int(declared_size) > MAX_RESPONSE_BYTES
                    ):
                        raise ProtocolError("模型响应长度无效或超过 1 MiB 上限")
                chunks = bytearray()
                while len(chunks) <= MAX_RESPONSE_BYTES:
                    remaining = expires_at - time.monotonic()
                    if remaining <= 0:
                        self._timeout_error()
                    if response.fp is None:
                        break
                    # urllib exposes HTTPResponse here; keep each body read inside
                    # the remaining request budget rather than renewing its timeout.
                    response.fp.raw._sock.settimeout(remaining)
                    chunk = response.read1(min(65536, MAX_RESPONSE_BYTES + 1 - len(chunks)))
                    if time.monotonic() >= expires_at:
                        self._timeout_error()
                    if not chunk:
                        break
                    chunks.extend(chunk)
                if len(chunks) > MAX_RESPONSE_BYTES:
                    raise ProtocolError("模型响应超过 1 MiB 上限")
                if declared_size is not None and len(chunks) != int(declared_size):
                    raise ProtocolError("模型响应长度不完整")
        except urllib.error.HTTPError as error:
            status = error.code
            error.close()
            raise LabError(
                f"模型 HTTP 请求失败（状态码 {status}），请检查端点、模型与课程凭据"
            ) from None
        except (
            urllib.error.URLError,
            socket.timeout,
            OSError,
            http.client.HTTPException,
            ValueError,
        ) as error:
            timed_out = isinstance(error, TimeoutError) or (
                isinstance(error, urllib.error.URLError) and isinstance(error.reason, TimeoutError)
            )
            # Socket timeouts can fire just before the next monotonic-clock sample
            # reaches the deadline. Classify by the bound used for this request.
            if timed_out and host_limited:
                raise BudgetExceeded("deadline") from None
            self._check_host_deadline()
            raise LabError("无法完成模型 HTTP 请求，请检查连接和超时设置") from None
        message, usage = _parse_response(bytes(chunks))
        self._check_host_deadline()
        if usage is not None and usage["completion_tokens"] > self.config.max_output_tokens:
            raise ProtocolError("模型报告的输出 token 超出本次请求预算")
        self._completed_requests += 1
        if usage is not None:
            self._requests_with_usage += 1
            for name, count in usage.items():
                self._usage[name] += count
        return message
