#!/usr/bin/env python3
"""codex-app-wrapper — Centaur NDJSON bridge for `codex app-server`.

The API speaks a small Anthropic-shaped stdin protocol. This adapter keeps a
single Codex app-server process alive, translates each user turn into JSON-RPC
`turn/start` (or `turn/steer` while a turn is active), opts into experimental
APIs for thread goals, and emits Codex-shaped NDJSON events for Centaur.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import queue
import signal
import subprocess
import sys
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any
from urllib import request as urllib_request
from urllib.error import HTTPError, URLError
from urllib.parse import unquote

from opentelemetry.proto.common.v1.common_pb2 import KeyValue
from opentelemetry.proto.collector.trace.v1.trace_service_pb2 import (
    ExportTraceServiceRequest,
)

APP: subprocess.Popen[str] | None = None
WRITE_LOCK = threading.Lock()
NEXT_ID = 1
RESPONSES: dict[int, queue.Queue[dict[str, Any]]] = {}
EVENTS: queue.Queue[dict[str, Any] | None] = queue.Queue()
INPUTS: queue.Queue[dict[str, Any] | None] = queue.Queue()
THREAD_ID: str | None = None
ACTIVE_TURN_ID: str | None = None
SHUTTING_DOWN = False
CONFIGURED_OTEL_TRACE_ID: str | None = None
CONFIGURED_TRACE_CONTEXT_ID: str | None = None
APP_INITIALIZED = False
CURRENT_TRACEPARENT: str | None = None
OTEL_PROXY: ThreadingHTTPServer | None = None
OTEL_PROXY_TARGET_ENDPOINT: str | None = None
OTEL_PROXY_SPAN_PREFIX = "codex."
CURRENT_LLM_INPUT_TEXT = ""
CURRENT_LLM_OUTPUT_TEXT = ""
LLM_INPUTS_BY_TURN_ID: dict[str, str] = {}
LLM_OUTPUTS_BY_TURN_ID: dict[str, str] = {}
CURRENT_TRACE_METADATA: dict[str, Any] = {}
TRACE_METADATA_BY_TURN_ID: dict[str, dict[str, Any]] = {}

LAMINAR_METADATA_PREFIX = "lmnr.association.properties.metadata."


def emit(payload: dict[str, Any]) -> None:
    sys.stdout.write(
        json.dumps(payload, separators=(",", ":"), ensure_ascii=False) + "\n"
    )
    sys.stdout.flush()


def _next_id() -> int:
    global NEXT_ID
    with WRITE_LOCK:
        value = NEXT_ID
        NEXT_ID += 1
    return value


def send_raw(payload: dict[str, Any]) -> None:
    assert APP is not None and APP.stdin is not None
    with WRITE_LOCK:
        APP.stdin.write(
            json.dumps(payload, separators=(",", ":"), ensure_ascii=False) + "\n"
        )
        APP.stdin.flush()


def request(
    method: str, params: dict[str, Any] | None = None, timeout: float = 30.0
) -> dict[str, Any]:
    msg_id = _next_id()
    q: queue.Queue[dict[str, Any]] = queue.Queue(maxsize=1)
    RESPONSES[msg_id] = q
    payload: dict[str, Any] = {"id": msg_id, "method": method, "params": params or {}}
    if CURRENT_TRACEPARENT:
        payload["trace"] = {"traceparent": CURRENT_TRACEPARENT}
    send_raw(payload)
    try:
        response = q.get(timeout=timeout)
    finally:
        RESPONSES.pop(msg_id, None)
    if "error" in response:
        raise RuntimeError(response["error"].get("message") or str(response["error"]))
    return response.get("result") or {}


def notify(method: str, params: dict[str, Any] | None = None) -> None:
    send_raw({"method": method, "params": params or {}})


def start_app_server() -> None:
    global APP, APP_INITIALIZED
    if APP is not None and APP.poll() is None and APP_INITIALIZED:
        return
    if APP is not None and APP.poll() is not None:
        APP = None
        APP_INITIALIZED = False

    APP = subprocess.Popen(
        [
            "codex",
            "app-server",
            "--listen",
            "stdio://",
        ],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=sys.stderr,
        text=True,
        bufsize=1,
        cwd=os.getcwd(),
    )
    threading.Thread(target=app_stdout_reader, daemon=True).start()
    request(
        "initialize",
        {
            "clientInfo": {"name": "centaur", "title": "Centaur", "version": "0.1.0"},
            "capabilities": {"experimentalApi": True},
        },
        timeout=30,
    )
    notify("initialized")
    APP_INITIALIZED = True
    emit(
        {
            "type": "system",
            "subtype": "wrapper_heartbeat",
            "phase": "app_server_started",
        }
    )


def app_stdout_reader() -> None:
    assert APP is not None and APP.stdout is not None
    for raw in APP.stdout:
        line = raw.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if "id" in msg:
            q = RESPONSES.get(msg["id"])
            if q:
                q.put(msg)
        elif "method" in msg:
            EVENTS.put(msg)
    EVENTS.put(None)


def api_stdin_reader() -> None:
    for raw in sys.stdin:
        line = raw.strip()
        if not line:
            continue
        try:
            INPUTS.put(json.loads(line))
        except json.JSONDecodeError:
            emit({"type": "error", "message": "invalid stdin JSON"})
    INPUTS.put(None)


def text_from_blocks(blocks: list[dict[str, Any]]) -> str:
    parts: list[str] = []
    for block in blocks:
        btype = block.get("type")
        if btype == "text":
            parts.append(str(block.get("text") or ""))
        elif btype == "image":
            parts.append(
                "[User sent an image attachment; if needed, ask them to upload it as a file reference.]"
            )
        else:
            parts.append(json.dumps(block, ensure_ascii=False))
    return "\n".join(p for p in parts if p).strip()


def input_items(turn_input: dict[str, Any]) -> list[dict[str, Any]]:
    blocks = turn_input.get("message", {}).get("content") or []
    if not isinstance(blocks, list):
        blocks = []
    text = text_from_blocks(blocks)
    return [{"type": "text", "text": text or "continue"}]


def trace_metadata_from_input(turn_input: dict[str, Any]) -> dict[str, Any]:
    metadata = turn_input.get("trace_metadata")
    return dict(metadata) if isinstance(metadata, dict) else {}


def split_goal(items: list[dict[str, Any]]) -> tuple[str | None, list[dict[str, Any]]]:
    if len(items) != 1 or items[0].get("type") != "text":
        return None, items
    text = str(items[0].get("text") or "").strip()
    if not text.startswith("/goal"):
        return None, items
    goal = text[len("/goal") :].strip()
    return goal or None, []


def _codex_otel_endpoint() -> str:
    endpoint = (os.environ.get("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT") or "").strip()
    if endpoint:
        return endpoint
    base = (os.environ.get("OTEL_EXPORTER_OTLP_ENDPOINT") or "").strip()
    if not base:
        return ""
    base = base.rstrip("/")
    if base.endswith("/v1/traces"):
        return base
    return f"{base}/v1/traces"


def _toml_string(value: str) -> str:
    return json.dumps(value, ensure_ascii=False)


def _strip_otel_toml_sections(contents: str) -> str:
    kept: list[str] = []
    skipping = False
    for line in contents.splitlines():
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            section = stripped.strip("[]").strip()
            skipping = section == "otel" or section.startswith("otel.")
        if not skipping:
            kept.append(line)
    return "\n".join(kept).rstrip()


def _write_codex_otel_config(
    *, endpoint: str, trace_id: str, thread_key: str, api_key: str, environment: str
) -> None:
    config_path = (
        Path(os.environ.get("CODEX_HOME") or Path.home() / ".codex") / "config.toml"
    )
    config_path.parent.mkdir(parents=True, exist_ok=True)
    base = _strip_otel_toml_sections(
        config_path.read_text() if config_path.exists() else ""
    )
    headers = [
        f"x-trace-id = {_toml_string(trace_id)}",
    ]
    if thread_key:
        headers.append(f"x-centaur-thread-key = {_toml_string(thread_key)}")
    if api_key:
        headers.append(f"authorization = {_toml_string(f'Bearer {api_key}')}")

    span_attributes = [
        f'"service.name" = {_toml_string("codex")}',
        f'"centaur.span_prefix" = {_toml_string("codex.")}',
    ]

    trace_exporter = (
        f"trace_exporter = {{ otlp-http = {{ endpoint = {_toml_string(endpoint)}, "
        f'protocol = "binary", headers = {{ {", ".join(headers)} }} }} }}'
    )
    otel_block = "\n".join(
        [
            "[otel]",
            f"environment = {_toml_string(environment)}",
            "log_user_prompt = true",
            f"span_attributes = {{ {', '.join(span_attributes)} }}",
            trace_exporter,
        ]
    )
    next_contents = f"{base}\n\n{otel_block}\n" if base else f"{otel_block}\n"
    config_path.write_text(next_contents)


def _span_prefix() -> str:
    return "codex."


def _otel_headers() -> dict[str, str]:
    headers: dict[str, str] = {}
    raw = (os.environ.get("OTEL_EXPORTER_OTLP_HEADERS") or "").strip()
    if not raw:
        return headers
    for item in raw.split(","):
        if "=" not in item:
            continue
        key, value = item.split("=", 1)
        key = key.strip().lower()
        if key:
            headers[key] = unquote(value.strip())
    return headers


def _otel_authorization_token() -> str:
    authorization = _otel_headers().get("authorization", "").strip()
    if authorization.lower().startswith("bearer "):
        return authorization[7:].strip()
    return authorization


def _otel_environment() -> str:
    raw = os.environ.get("OTEL_RESOURCE_ATTRIBUTES") or ""
    for item in raw.split(","):
        if "=" not in item:
            continue
        key, value = item.split("=", 1)
        if key.strip() == "deployment.environment":
            environment = value.strip()
            if environment:
                return environment
    return (
        os.environ.get("DEPLOY_ENV") or os.environ.get("ENVIRONMENT") or "dev"
    ).strip() or "dev"


def _attribute(span: Any, key: str) -> KeyValue | None:
    return next(
        (attribute for attribute in span.attributes if attribute.key == key), None
    )


def _attribute_string(span: Any, key: str) -> str:
    attribute = _attribute(span, key)
    if attribute is None:
        return ""
    value_type = attribute.value.WhichOneof("value")
    if value_type == "string_value":
        return attribute.value.string_value
    if value_type == "int_value":
        return str(attribute.value.int_value)
    if value_type == "double_value":
        return str(attribute.value.double_value)
    if value_type == "bool_value":
        return "true" if attribute.value.bool_value else "false"
    return ""


def _attribute_int(span: Any, key: str) -> int | None:
    attribute = _attribute(span, key)
    if attribute is None:
        return None
    value_type = attribute.value.WhichOneof("value")
    if value_type == "int_value":
        return int(attribute.value.int_value)
    if value_type == "double_value":
        return int(attribute.value.double_value)
    if value_type == "string_value":
        try:
            return int(attribute.value.string_value)
        except ValueError:
            return None
    return None


def _set_attribute_string(span: Any, key: str, value: str) -> None:
    if value == "":
        return
    attribute = _attribute(span, key)
    if attribute is None:
        attribute = span.attributes.add()
        attribute.key = key
    attribute.value.string_value = value


def _set_attribute_value(span: Any, key: str, value: Any) -> None:
    if value is None:
        return
    attribute = _attribute(span, key)
    if attribute is None:
        attribute = span.attributes.add()
        attribute.key = key
    if isinstance(value, bool):
        attribute.value.bool_value = value
    elif isinstance(value, int) and not isinstance(value, bool):
        attribute.value.int_value = value
    elif isinstance(value, float):
        attribute.value.double_value = value
    elif isinstance(value, str):
        attribute.value.string_value = value
    elif isinstance(value, (list, tuple)) and all(
        isinstance(item, (str, bool, int, float)) for item in value
    ):
        attribute.value.string_value = json.dumps(
            list(value), ensure_ascii=False, separators=(",", ":")
        )
    else:
        attribute.value.string_value = json.dumps(
            value,
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
            default=str,
        )


def _set_laminar_trace_metadata(span: Any, metadata: dict[str, Any]) -> None:
    for key, value in metadata.items():
        metadata_key = str(key).strip()
        if metadata_key:
            _set_attribute_value(
                span, f"{LAMINAR_METADATA_PREFIX}{metadata_key}", value
            )


def _append_current_llm_output(text: Any) -> None:
    global CURRENT_LLM_OUTPUT_TEXT
    if isinstance(text, str) and text:
        CURRENT_LLM_OUTPUT_TEXT += text


def _append_llm_output_for_turn(turn_id: Any, text: Any) -> None:
    if (
        not isinstance(turn_id, str)
        or not turn_id
        or not isinstance(text, str)
        or not text
    ):
        return
    LLM_OUTPUTS_BY_TURN_ID[turn_id] = LLM_OUTPUTS_BY_TURN_ID.get(turn_id, "") + text


def _set_attribute_int(span: Any, key: str, value: int | None) -> None:
    if value is None:
        return
    attribute = _attribute(span, key)
    if attribute is None:
        attribute = span.attributes.add()
        attribute.key = key
    attribute.value.int_value = value


def _chat_messages_attribute(role: str, text: str) -> str:
    return json.dumps(
        [{"role": role, "content": text}],
        ensure_ascii=False,
        separators=(",", ":"),
    )


def _normalize_codex_llm_span(span: Any, prefix: str) -> None:
    if span.name != f"{prefix}session_task.turn":
        return

    model = _attribute_string(span, "model")
    turn_id = _attribute_string(span, "turn.id")
    input_text = LLM_INPUTS_BY_TURN_ID.get(turn_id, CURRENT_LLM_INPUT_TEXT)
    output_text = LLM_OUTPUTS_BY_TURN_ID.get(turn_id, CURRENT_LLM_OUTPUT_TEXT)
    trace_metadata = TRACE_METADATA_BY_TURN_ID.get(turn_id, CURRENT_TRACE_METADATA)
    _set_laminar_trace_metadata(span, trace_metadata)
    _set_attribute_string(span, "gen_ai.operation.name", "chat")
    _set_attribute_string(span, "gen_ai.system", "openai")
    _set_attribute_string(span, "gen_ai.request.model", model)
    _set_attribute_string(span, "gen_ai.response.model", model)
    _set_attribute_string(span, "input.value", input_text)
    _set_attribute_string(span, "output.value", output_text)
    _set_attribute_string(
        span, "gen_ai.input.messages", _chat_messages_attribute("user", input_text)
    )
    _set_attribute_string(
        span,
        "gen_ai.output.messages",
        _chat_messages_attribute("assistant", output_text),
    )
    _set_attribute_int(
        span,
        "gen_ai.usage.input_tokens",
        _attribute_int(span, "codex.turn.token_usage.input_tokens"),
    )
    _set_attribute_int(
        span,
        "gen_ai.usage.output_tokens",
        _attribute_int(span, "codex.turn.token_usage.output_tokens"),
    )
    _set_attribute_int(
        span,
        "gen_ai.usage.cache_read_input_tokens",
        _attribute_int(span, "codex.turn.token_usage.cached_input_tokens"),
    )
    _set_attribute_int(
        span,
        "gen_ai.usage.reasoning_tokens",
        _attribute_int(span, "codex.turn.token_usage.reasoning_output_tokens"),
    )


def _prefix_otlp_span_names(payload: bytes, prefix: str) -> bytes:
    request = ExportTraceServiceRequest()
    request.ParseFromString(payload)
    for resource_span in request.resource_spans:
        for scope_span in resource_span.scope_spans:
            for span in scope_span.spans:
                if span.name and not span.name.startswith(prefix):
                    span.name = f"{prefix}{span.name}"
                _normalize_codex_llm_span(span, prefix)
    return request.SerializeToString()


class CodexOtelPrefixProxyHandler(BaseHTTPRequestHandler):
    server_version = "CodexOtelPrefixProxy/1.0"

    def do_POST(self) -> None:
        if self.path != "/v1/traces":
            self.send_error(404)
            return
        endpoint = OTEL_PROXY_TARGET_ENDPOINT
        if not endpoint:
            self.send_error(503, "OTLP target endpoint not configured")
            return
        try:
            length = int(self.headers.get("content-length") or "0")
            body = self.rfile.read(length)
            rewritten = _prefix_otlp_span_names(body, OTEL_PROXY_SPAN_PREFIX)
            headers = {
                key: value
                for key, value in self.headers.items()
                if key.lower()
                not in {
                    "host",
                    "content-length",
                    "content-encoding",
                    "accept-encoding",
                    "connection",
                }
            }
            headers["content-type"] = "application/x-protobuf"
            request = urllib_request.Request(
                endpoint,
                data=rewritten,
                headers=headers,
                method="POST",
            )
            with urllib_request.urlopen(request, timeout=10) as response:
                response_body = response.read()
                self.send_response(response.status)
                for key, value in response.headers.items():
                    if key.lower() not in {"content-length", "connection"}:
                        self.send_header(key, value)
                self.send_header("content-length", str(len(response_body)))
                self.end_headers()
                if response_body:
                    self.wfile.write(response_body)
        except HTTPError as exc:
            body = exc.read()
            self.send_response(exc.code)
            for key, value in exc.headers.items():
                if key.lower() not in {"content-length", "connection"}:
                    self.send_header(key, value)
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            if body:
                self.wfile.write(body)
        except (OSError, URLError, ValueError) as exc:
            self.send_error(502, str(exc))

    def log_message(self, _format: str, *_args: object) -> None:
        return


def start_codex_otel_prefix_proxy(endpoint: str, span_prefix: str) -> str:
    global OTEL_PROXY, OTEL_PROXY_SPAN_PREFIX, OTEL_PROXY_TARGET_ENDPOINT
    OTEL_PROXY_TARGET_ENDPOINT = endpoint
    OTEL_PROXY_SPAN_PREFIX = span_prefix
    if OTEL_PROXY is None:
        OTEL_PROXY = ThreadingHTTPServer(("127.0.0.1", 0), CodexOtelPrefixProxyHandler)
        threading.Thread(target=OTEL_PROXY.serve_forever, daemon=True).start()
    host, port = OTEL_PROXY.server_address
    return f"http://{host}:{port}/v1/traces"


def configure_codex_otel_for_startup(
    trace_id: str | None, thread_key: str | None
) -> None:
    global CONFIGURED_OTEL_TRACE_ID
    trace_id = (trace_id or os.environ.get("CENTAUR_TRACE_ID") or "").strip()
    endpoint = _codex_otel_endpoint()
    if not trace_id or not endpoint or CONFIGURED_OTEL_TRACE_ID == trace_id:
        return
    span_prefix = _span_prefix()
    export_endpoint = start_codex_otel_prefix_proxy(endpoint, span_prefix)

    api_key = _otel_authorization_token()
    environment = _otel_environment()
    _write_codex_otel_config(
        endpoint=export_endpoint,
        trace_id=trace_id,
        thread_key=(thread_key or os.environ.get("CENTAUR_THREAD_KEY") or "").strip(),
        api_key=api_key,
        environment=environment,
    )
    CONFIGURED_OTEL_TRACE_ID = trace_id


def _trace_id_to_w3c_hex(trace_id: str | None) -> str | None:
    raw = (trace_id or "").strip()
    if not raw:
        return None
    try:
        return uuid.UUID(raw).hex
    except ValueError:
        compact = raw.replace("-", "").lower()
        if len(compact) == 32 and all(char in "0123456789abcdef" for char in compact):
            return compact
    return None


def _new_parent_span_id() -> str:
    span_id = uuid.uuid4().hex[:16]
    return span_id if span_id != "0" * 16 else "1".rjust(16, "0")


def _traceparent_from_trace_id(trace_id: str | None) -> str | None:
    trace_hex = _trace_id_to_w3c_hex(trace_id)
    if not trace_hex or trace_hex == "0" * 32:
        return None
    return f"00-{trace_hex}-{_new_parent_span_id()}-01"


def configure_trace_context(trace_id: str | None) -> None:
    global CURRENT_TRACEPARENT
    traceparent = _traceparent_from_trace_id(trace_id)
    if not traceparent:
        return
    CURRENT_TRACEPARENT = traceparent
    os.environ["TRACEPARENT"] = traceparent


def configure_traceparent(traceparent: str | None) -> None:
    global CURRENT_TRACEPARENT
    value = (traceparent or "").strip()
    parts = value.split("-")
    if (
        len(parts) == 4
        and parts[0] == "00"
        and len(parts[1]) == 32
        and len(parts[2]) == 16
        and all(char in "0123456789abcdef" for char in parts[1].lower())
        and all(char in "0123456789abcdef" for char in parts[2].lower())
    ):
        CURRENT_TRACEPARENT = value
        os.environ["TRACEPARENT"] = value


def _trace_id_from_traceparent(traceparent: str | None) -> str | None:
    parts = (traceparent or "").strip().split("-")
    if len(parts) != 4 or len(parts[1]) != 32:
        return None
    try:
        return str(uuid.UUID(hex=parts[1]))
    except ValueError:
        return None


def configure_trace_context_for_startup(trace_id: str | None) -> None:
    global CONFIGURED_TRACE_CONTEXT_ID
    trace_id = (trace_id or os.environ.get("CENTAUR_TRACE_ID") or "").strip()
    configure_trace_context(trace_id)
    if not trace_id or CONFIGURED_TRACE_CONTEXT_ID == trace_id:
        return
    CONFIGURED_TRACE_CONTEXT_ID = trace_id


def start_or_resume_thread() -> str:
    global THREAD_ID
    if THREAD_ID:
        return THREAD_ID
    resume = (
        os.environ.get("CODEX_CONTINUE_THREAD_ID")
        or os.environ.get("AMP_CONTINUE_THREAD_ID")
        or ""
    ).strip()
    if resume:
        try:
            result = request(
                "thread/resume", {"threadId": resume, "cwd": os.getcwd()}, timeout=60
            )
        except Exception as exc:
            # The rollout file lives only on the sandbox filesystem; after a pod
            # replacement it is gone and resume can never succeed. Fall back to a
            # fresh thread so the turn proceeds instead of bricking the session.
            emit(
                {
                    "type": "system",
                    "subtype": "thread_resume_failed",
                    "message": str(exc),
                    "resume_thread_id": resume,
                }
            )
            resume = ""
            result = request("thread/start", {"cwd": os.getcwd()}, timeout=60)
    else:
        result = request("thread/start", {"cwd": os.getcwd()}, timeout=60)
    thread = result.get("thread") or {}
    THREAD_ID = str(thread.get("id") or resume or uuid.uuid4())
    emit({"type": "thread.started", "thread_id": THREAD_ID})
    return THREAD_ID


def emit_notification(msg: dict[str, Any]) -> bool:
    global THREAD_ID, ACTIVE_TURN_ID
    method = str(msg.get("method") or "")
    params = msg.get("params") if isinstance(msg.get("params"), dict) else {}

    if method == "thread/started":
        thread = params.get("thread") or {}
        tid = thread.get("id") or params.get("threadId")
        if tid:
            THREAD_ID = str(tid)
            emit({"type": "thread.started", "thread_id": THREAD_ID})
        return False

    if method == "turn/started":
        turn = params.get("turn") or {}
        ACTIVE_TURN_ID = (
            str(turn.get("id") or params.get("turnId") or "") or ACTIVE_TURN_ID
        )
        emit({"type": "turn.started", "turn_id": ACTIVE_TURN_ID or ""})
        return False

    if method in {
        "item/commandExecution/outputDelta",
        "item/fileChange/outputDelta",
        "item/plan/delta",
        "item/reasoning/summaryTextDelta",
        "item/reasoning/summaryPartAdded",
        "item/reasoning/textDelta",
    }:
        emit({"type": method.replace("/", "."), **params})
        return False

    if method == "turn/plan/updated":
        emit({"type": method.replace("/", "."), **params})
        return False

    if method == "item/agentMessage/delta":
        _append_current_llm_output(params.get("delta"))
        _append_llm_output_for_turn(params.get("turnId"), params.get("delta"))
        payload = {"type": method.replace("/", "."), **params}
        if THREAD_ID and "session_id" not in payload and "thread_id" not in payload:
            payload["session_id"] = THREAD_ID
        emit(payload)
        return False

    if method == "item/completed":
        item = params.get("item") if isinstance(params.get("item"), dict) else {}
        if item.get("type") == "agentMessage" and not CURRENT_LLM_OUTPUT_TEXT:
            _append_current_llm_output(item.get("text"))
        emit({"type": method.replace("/", "."), "item": item or params})
        return False

    if method in {"item/started", "item/updated"}:
        emit({"type": method.replace("/", "."), "item": params.get("item") or params})
        return False

    if method == "turn/completed":
        turn = params.get("turn") or {}
        emit(
            {
                "type": "turn.completed",
                "turn": turn,
                "usage": params.get("usage") or turn.get("usage"),
            }
        )
        ACTIVE_TURN_ID = None
        return True

    if method in {"turn/failed", "error"}:
        emit({"type": "turn.failed", "error": params.get("error") or params})
        ACTIVE_TURN_ID = None
        return True

    if method in {"thread/goal/updated", "thread/goal/cleared"}:
        emit({"type": method.replace("/", "."), **params})
        return False

    return False


def drain_until_turn_done() -> None:
    while True:
        try:
            msg = EVENTS.get(timeout=0.1)
        except queue.Empty:
            try:
                incoming = INPUTS.get_nowait()
            except queue.Empty:
                continue
            if incoming is None:
                return
            handle_input(incoming)
            continue
        if msg is None:
            return
        if emit_notification(msg):
            return


def handle_input(turn_input: dict[str, Any]) -> None:
    global ACTIVE_TURN_ID, CURRENT_LLM_INPUT_TEXT, CURRENT_LLM_OUTPUT_TEXT
    global CURRENT_TRACE_METADATA
    if turn_input.get("type") == "interrupt":
        interrupt_active_turn()
        return
    if turn_input.get("type") != "user":
        return

    configure_trace_context_for_startup(turn_input.get("trace_id"))
    configure_traceparent(turn_input.get("traceparent"))
    configure_codex_otel_for_startup(
        _trace_id_from_traceparent(turn_input.get("traceparent"))
        or turn_input.get("trace_id"),
        turn_input.get("thread_key"),
    )
    CURRENT_TRACE_METADATA = trace_metadata_from_input(turn_input)
    start_app_server()
    thread_id = start_or_resume_thread()
    items = input_items(turn_input)
    CURRENT_LLM_INPUT_TEXT = "\n".join(
        str(item.get("text") or "") for item in items if item.get("type") == "text"
    ).strip()
    CURRENT_LLM_OUTPUT_TEXT = ""
    goal, items = split_goal(items)
    if goal is not None:
        request(
            "thread/goal/set", {"threadId": thread_id, "objective": goal}, timeout=30
        )
        emit(
            {
                "type": "assistant",
                "session_id": thread_id,
                "message": {"content": [{"type": "text", "text": "Goal set."}]},
            }
        )
        emit({"type": "turn.completed"})
        return

    params = {"threadId": thread_id, "input": items}
    if ACTIVE_TURN_ID or turn_input.get("steer"):
        try:
            steer_params = {**params, "expectedTurnId": ACTIVE_TURN_ID or ""}
            result = request("turn/steer", steer_params, timeout=10)
            ACTIVE_TURN_ID = (
                str(
                    result.get("turnId")
                    or result.get("turn_id")
                    or ACTIVE_TURN_ID
                    or ""
                )
                or None
            )
            if ACTIVE_TURN_ID and CURRENT_TRACE_METADATA:
                TRACE_METADATA_BY_TURN_ID[ACTIVE_TURN_ID] = dict(CURRENT_TRACE_METADATA)
            return
        except Exception:
            interrupt_active_turn()
    result = request("turn/start", params, timeout=60)
    turn = result.get("turn") or {}
    ACTIVE_TURN_ID = str(turn.get("id") or result.get("turnId") or "") or None
    if ACTIVE_TURN_ID and CURRENT_LLM_INPUT_TEXT:
        LLM_INPUTS_BY_TURN_ID[ACTIVE_TURN_ID] = CURRENT_LLM_INPUT_TEXT
    if ACTIVE_TURN_ID and CURRENT_TRACE_METADATA:
        TRACE_METADATA_BY_TURN_ID[ACTIVE_TURN_ID] = dict(CURRENT_TRACE_METADATA)
    drain_until_turn_done()


def interrupt_active_turn(*_args: object) -> None:
    global ACTIVE_TURN_ID
    if THREAD_ID and ACTIVE_TURN_ID:
        try:
            request(
                "turn/interrupt",
                {"threadId": THREAD_ID, "turnId": ACTIVE_TURN_ID},
                timeout=5,
            )
        except Exception as exc:
            emit({"type": "error", "message": f"interrupt failed: {exc}"})
    ACTIVE_TURN_ID = None


def exit_wrapper(*_args: object) -> None:
    global SHUTTING_DOWN
    SHUTTING_DOWN = True
    if APP and APP.poll() is None:
        APP.terminate()


def main() -> None:
    signal.signal(signal.SIGTERM, exit_wrapper)
    signal.signal(signal.SIGINT, exit_wrapper)
    signal.signal(signal.SIGUSR1, interrupt_active_turn)

    threading.Thread(target=api_stdin_reader, daemon=True).start()
    emit({"type": "system", "subtype": "wrapper_heartbeat", "phase": "startup"})

    while not SHUTTING_DOWN:
        item = INPUTS.get()
        if item is None:
            break
        try:
            handle_input(item)
        except Exception as exc:
            emit({"type": "error", "message": str(exc)})
            emit({"type": "turn.failed", "error": {"message": str(exc)}})
        time.sleep(0.01)

    exit_wrapper()
    if APP:
        APP.wait(timeout=10)


if __name__ == "__main__":
    main()
