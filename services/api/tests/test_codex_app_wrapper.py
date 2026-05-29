from __future__ import annotations

import importlib.util
from pathlib import Path
import tomllib
from types import ModuleType
import uuid

from opentelemetry.proto.collector.trace.v1.trace_service_pb2 import (
    ExportTraceServiceRequest,
)


WRAPPER_PY = Path(__file__).resolve().parents[2] / "sandbox" / "codex-app-wrapper.py"


def _load_wrapper() -> ModuleType:
    spec = importlib.util.spec_from_file_location("codex_app_wrapper", WRAPPER_PY)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_configure_trace_context_for_startup_sets_w3c_trace_context(
    monkeypatch, tmp_path
) -> None:
    wrapper = _load_wrapper()
    codex_home = tmp_path / ".codex"
    monkeypatch.setenv("CODEX_HOME", str(codex_home))
    monkeypatch.setattr(
        wrapper.uuid,
        "uuid4",
        lambda: uuid.UUID("11111111-2222-3333-4444-555555555555"),
    )

    wrapper.CURRENT_TRACEPARENT = None
    wrapper.configure_trace_context_for_startup("00000000-0000-4000-8000-000000000123")

    assert (
        wrapper.CURRENT_TRACEPARENT
        == "00-00000000000040008000000000000123-1111111122223333-01"
    )
    assert (
        wrapper.os.environ["TRACEPARENT"]
        == "00-00000000000040008000000000000123-1111111122223333-01"
    )
    assert wrapper.CONFIGURED_TRACE_CONTEXT_ID == "00000000-0000-4000-8000-000000000123"
    assert wrapper.CONFIGURED_OTEL_TRACE_ID is None


def test_trace_context_startup_does_not_skip_codex_otel_config(
    monkeypatch, tmp_path
) -> None:
    wrapper = _load_wrapper()
    codex_home = tmp_path / ".codex"
    monkeypatch.setenv("CODEX_HOME", str(codex_home))
    monkeypatch.setenv("OTEL_EXPORTER_OTLP_ENDPOINT", "http://otlp-collector:4318")
    monkeypatch.setattr(
        wrapper,
        "start_codex_otel_prefix_proxy",
        lambda _endpoint, _span_prefix: "http://127.0.0.1:4319/v1/traces",
    )

    wrapper.CONFIGURED_TRACE_CONTEXT_ID = None
    wrapper.CONFIGURED_OTEL_TRACE_ID = None
    wrapper.configure_trace_context_for_startup("00000000-0000-4000-8000-000000000123")
    wrapper.configure_codex_otel_for_startup(
        "00000000-0000-4000-8000-000000000123",
        "slack:C123:1700000000.000100",
    )

    parsed = tomllib.loads((codex_home / "config.toml").read_text())
    assert (
        parsed["otel"]["trace_exporter"]["otlp-http"]["endpoint"]
        == "http://127.0.0.1:4319/v1/traces"
    )


def test_configure_trace_context_ignores_invalid_trace_id(monkeypatch) -> None:
    wrapper = _load_wrapper()
    monkeypatch.delenv("TRACEPARENT", raising=False)

    wrapper.CURRENT_TRACEPARENT = None
    wrapper.configure_trace_context("not-a-trace")

    assert wrapper.CURRENT_TRACEPARENT is None
    assert "TRACEPARENT" not in wrapper.os.environ


def test_configure_traceparent_uses_exact_parent_context(monkeypatch) -> None:
    wrapper = _load_wrapper()
    traceparent = "00-00000000000040008000000000000123-1111111122223333-01"
    monkeypatch.delenv("TRACEPARENT", raising=False)

    wrapper.CURRENT_TRACEPARENT = None
    wrapper.configure_traceparent(traceparent)

    assert wrapper.CURRENT_TRACEPARENT == traceparent
    assert wrapper.os.environ["TRACEPARENT"] == traceparent


def test_request_attaches_traceparent(monkeypatch) -> None:
    wrapper = _load_wrapper()
    sent: list[dict] = []
    monkeypatch.setattr(wrapper, "_next_id", lambda: 1)

    def fake_send_raw(payload: dict) -> None:
        sent.append(payload)
        wrapper.RESPONSES[1].put({"id": 1, "result": {"ok": True}})

    monkeypatch.setattr(wrapper, "send_raw", fake_send_raw)

    wrapper.CURRENT_TRACEPARENT = (
        "00-00000000000040008000000000000123-1111111122223333-01"
    )
    result = wrapper.request("thread/start", {"cwd": "/tmp"}, timeout=0.1)

    assert result == {"ok": True}
    assert sent == [
        {
            "id": 1,
            "method": "thread/start",
            "params": {"cwd": "/tmp"},
            "trace": {
                "traceparent": "00-00000000000040008000000000000123-1111111122223333-01"
            },
        }
    ]


def test_configure_codex_otel_writes_startup_config(monkeypatch, tmp_path) -> None:
    wrapper = _load_wrapper()
    proxy_calls: list[tuple[str, str]] = []
    codex_home = tmp_path / ".codex"
    codex_home.mkdir()
    (codex_home / "config.toml").write_text(
        "\n".join(
            [
                'model = "gpt-5.5"',
                "",
                "[otel]",
                'environment = "old"',
                "",
                "[otel.trace_exporter.otlp-http]",
                'endpoint = "http://old/v1/traces"',
                "",
            ]
        )
    )
    monkeypatch.setenv("CODEX_HOME", str(codex_home))
    monkeypatch.setenv("OTEL_EXPORTER_OTLP_ENDPOINT", "http://otlp-collector:4318")
    monkeypatch.setenv("OTEL_EXPORTER_OTLP_HEADERS", "authorization=Bearer%20otlp-key")
    monkeypatch.setenv("OTEL_RESOURCE_ATTRIBUTES", "deployment.environment=staging")
    monkeypatch.setattr(
        wrapper,
        "start_codex_otel_prefix_proxy",
        lambda endpoint, span_prefix: (
            proxy_calls.append((endpoint, span_prefix))
            or "http://127.0.0.1:4319/v1/traces"
        ),
    )

    wrapper.CONFIGURED_OTEL_TRACE_ID = None
    wrapper.configure_codex_otel_for_startup(
        "00000000-0000-4000-8000-000000000123",
        "slack:C123:1700000000.000100",
    )

    config = (codex_home / "config.toml").read_text()
    parsed = tomllib.loads(config)
    assert parsed["model"] == "gpt-5.5"
    assert parsed["otel"]["environment"] == "staging"
    assert parsed["otel"]["log_user_prompt"] is True
    assert parsed["otel"]["span_attributes"] == {
        "service.name": "codex",
        "centaur.span_prefix": "codex.",
    }
    assert (
        parsed["otel"]["trace_exporter"]["otlp-http"]["endpoint"]
        == "http://127.0.0.1:4319/v1/traces"
    )
    assert parsed["otel"]["trace_exporter"]["otlp-http"]["protocol"] == "binary"
    assert parsed["otel"]["trace_exporter"]["otlp-http"]["headers"] == {
        "x-trace-id": "00000000-0000-4000-8000-000000000123",
        "x-centaur-thread-key": "slack:C123:1700000000.000100",
        "authorization": "Bearer otlp-key",
    }
    assert config.count("[otel]") == 1
    assert proxy_calls == [("http://otlp-collector:4318/v1/traces", "codex.")]


def test_configure_codex_otel_ignores_unrelated_collector_endpoint(
    monkeypatch, tmp_path
) -> None:
    wrapper = _load_wrapper()
    codex_home = tmp_path / ".codex"
    codex_home.mkdir()
    monkeypatch.setenv("CODEX_HOME", str(codex_home))
    monkeypatch.setenv("OTLP_BASE_URL", "http://otlp-collector:4318")
    monkeypatch.setenv("OTEL_EXPORTER_OTLP_HEADERS", "authorization=Bearer%20otlp-key")
    monkeypatch.setattr(
        wrapper,
        "start_codex_otel_prefix_proxy",
        lambda *_args: (_ for _ in ()).throw(
            AssertionError("otel should stay disabled")
        ),
    )

    wrapper.CONFIGURED_OTEL_TRACE_ID = None
    wrapper.configure_codex_otel_for_startup(
        "00000000-0000-4000-8000-000000000123",
        "slack:C123:1700000000.000100",
    )

    assert not (codex_home / "config.toml").exists()


def test_prefix_otlp_span_names_prefixes_codex_spans() -> None:
    wrapper = _load_wrapper()
    request = ExportTraceServiceRequest()
    scope_spans = request.resource_spans.add().scope_spans.add()
    scope_spans.spans.add(name="session_task.turn")
    scope_spans.spans.add(name="codex.initialize")

    rewritten = wrapper._prefix_otlp_span_names(request.SerializeToString(), "codex.")
    parsed = ExportTraceServiceRequest()
    parsed.ParseFromString(rewritten)

    names = [
        span.name
        for resource_span in parsed.resource_spans
        for scope_span in resource_span.scope_spans
        for span in scope_span.spans
    ]
    assert names == ["codex.session_task.turn", "codex.initialize"]


def test_prefix_otlp_span_names_normalizes_codex_turn_as_gen_ai_span() -> None:
    wrapper = _load_wrapper()
    request = ExportTraceServiceRequest()
    span = (
        request.resource_spans.add()
        .scope_spans.add()
        .spans.add(name="session_task.turn")
    )

    def set_string(key: str, value: str) -> None:
        attribute = span.attributes.add()
        attribute.key = key
        attribute.value.string_value = value

    def set_int(key: str, value: int) -> None:
        attribute = span.attributes.add()
        attribute.key = key
        attribute.value.int_value = value

    set_string("model", "gpt-5.5")
    set_string("turn.id", "turn-123")
    set_int("codex.turn.token_usage.input_tokens", 20448)
    set_int("codex.turn.token_usage.output_tokens", 11)
    set_int("codex.turn.token_usage.cached_input_tokens", 20352)
    set_int("codex.turn.token_usage.reasoning_output_tokens", 0)
    wrapper.CURRENT_LLM_INPUT_TEXT = "stale input"
    wrapper.CURRENT_LLM_OUTPUT_TEXT = "stale output"
    wrapper.LLM_INPUTS_BY_TURN_ID = {
        "turn-123": "Reply with exactly PONG and nothing else."
    }
    wrapper.LLM_OUTPUTS_BY_TURN_ID = {"turn-123": "PONG"}
    wrapper.CURRENT_TRACE_METADATA = {"environment": "stale"}
    wrapper.TRACE_METADATA_BY_TURN_ID = {
        "turn-123": {
            "environment": "local",
            "thread_key": "slack:C123:1700000000.000100",
            "execution_id": "exe_123",
        }
    }

    rewritten = wrapper._prefix_otlp_span_names(request.SerializeToString(), "codex.")
    parsed = ExportTraceServiceRequest()
    parsed.ParseFromString(rewritten)
    rewritten_span = parsed.resource_spans[0].scope_spans[0].spans[0]
    attributes = {
        attribute.key: attribute.value for attribute in rewritten_span.attributes
    }

    assert rewritten_span.name == "codex.session_task.turn"
    assert attributes["gen_ai.operation.name"].string_value == "chat"
    assert attributes["gen_ai.system"].string_value == "openai"
    assert attributes["gen_ai.request.model"].string_value == "gpt-5.5"
    assert attributes["gen_ai.response.model"].string_value == "gpt-5.5"
    assert attributes["input.value"].string_value == (
        "Reply with exactly PONG and nothing else."
    )
    assert attributes["output.value"].string_value == "PONG"
    assert attributes["gen_ai.input.messages"].string_value == (
        '[{"role":"user","content":"Reply with exactly PONG and nothing else."}]'
    )
    assert attributes["gen_ai.output.messages"].string_value == (
        '[{"role":"assistant","content":"PONG"}]'
    )
    assert attributes["gen_ai.usage.input_tokens"].int_value == 20448
    assert attributes["gen_ai.usage.output_tokens"].int_value == 11
    assert attributes["gen_ai.usage.cache_read_input_tokens"].int_value == 20352
    assert attributes["gen_ai.usage.reasoning_tokens"].int_value == 0
    assert (
        attributes["lmnr.association.properties.metadata.environment"].string_value
        == "local"
    )
    assert (
        attributes["lmnr.association.properties.metadata.thread_key"].string_value
        == "slack:C123:1700000000.000100"
    )
    assert (
        attributes["lmnr.association.properties.metadata.execution_id"].string_value
        == "exe_123"
    )


def test_emit_notification_collects_agent_message_delta_output(monkeypatch) -> None:
    wrapper = _load_wrapper()
    emitted: list[dict] = []
    monkeypatch.setattr(wrapper, "emit", emitted.append)

    wrapper.CURRENT_LLM_OUTPUT_TEXT = ""
    wrapper.LLM_OUTPUTS_BY_TURN_ID = {}
    done = wrapper.emit_notification(
        {
            "method": "item/agentMessage/delta",
            "params": {"delta": "PO", "itemId": "msg-1", "turnId": "turn-123"},
        }
    )
    wrapper.emit_notification(
        {
            "method": "item/agentMessage/delta",
            "params": {"delta": "NG", "itemId": "msg-1", "turnId": "turn-123"},
        }
    )

    assert done is False
    assert wrapper.CURRENT_LLM_OUTPUT_TEXT == "PONG"
    assert wrapper.LLM_OUTPUTS_BY_TURN_ID == {"turn-123": "PONG"}
    assert emitted[0]["type"] == "item.agentMessage.delta"


def test_main_lazy_starts_app_server_after_input(monkeypatch) -> None:
    wrapper = _load_wrapper()
    requests: list[tuple[str, dict]] = []
    popen_args: list[str] = []
    emitted: list[dict] = []

    class FakeProcess:
        stdin = object()
        stdout = object()
        stderr = object()

        def poll(self) -> None:
            return None

        def terminate(self) -> None:
            return None

        def wait(self, timeout: float | None = None) -> int:
            return 0

    class FakeThread:
        def __init__(self, *args, **kwargs) -> None:
            self.target = kwargs.get("target")

        def start(self) -> None:
            if self.target == wrapper.api_stdin_reader:
                wrapper.INPUTS.put(
                    {
                        "type": "user",
                        "trace_id": "00000000-0000-0000-0000-000000000123",
                        "thread_key": "slack:C123:1700000000.000100",
                        "message": {
                            "content": [{"type": "text", "text": "/goal ship"}]
                        },
                    }
                )
                wrapper.INPUTS.put(None)

    def fake_request(method: str, params: dict, timeout: float = 30.0) -> dict:
        requests.append((method, params))
        if method == "initialize":
            return {"codexHome": "/tmp/.codex"}
        if method == "thread/start":
            return {"thread": {"id": "thread-123"}}
        return {}

    def fake_emit(msg: dict) -> None:
        emitted.append(msg)
        if msg.get("type") == "turn.completed":
            wrapper.SHUTTING_DOWN = True

    def fake_popen(args: list[str], *other_args, **kwargs) -> FakeProcess:
        popen_args.extend(args)
        return FakeProcess()

    monkeypatch.setattr(wrapper.subprocess, "Popen", fake_popen)
    monkeypatch.setattr(wrapper.threading, "Thread", FakeThread)
    monkeypatch.setattr(wrapper, "request", fake_request)
    monkeypatch.setattr(wrapper, "notify", lambda *_args, **_kwargs: None)
    monkeypatch.setattr(wrapper, "emit", fake_emit)
    monkeypatch.setattr(
        wrapper, "configure_trace_context_for_startup", lambda *_args, **_kwargs: None
    )
    wrapper.SHUTTING_DOWN = False
    wrapper.APP = None
    wrapper.APP_INITIALIZED = False
    wrapper.THREAD_ID = None
    while not wrapper.INPUTS.empty():
        wrapper.INPUTS.get_nowait()

    wrapper.main()

    assert popen_args == ["codex", "app-server", "--listen", "stdio://"]
    assert requests[0] == (
        "initialize",
        {
            "clientInfo": {
                "name": "centaur",
                "title": "Centaur",
                "version": "0.1.0",
            },
            "capabilities": {"experimentalApi": True},
        },
    )
    assert requests[1][0] == "thread/start"
    assert requests[2] == (
        "thread/goal/set",
        {"threadId": "thread-123", "objective": "ship"},
    )
    assert {"type": "thread.started", "thread_id": "thread-123"} in emitted
    assert {"type": "turn.completed"} in emitted
