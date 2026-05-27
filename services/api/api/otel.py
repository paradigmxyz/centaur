from __future__ import annotations

import json
import os
from collections.abc import Mapping
from contextlib import contextmanager
from typing import Any

import structlog
from opentelemetry import propagate, trace
from opentelemetry.exporter.otlp.proto.http.trace_exporter import OTLPSpanExporter
from opentelemetry.sdk.resources import Resource
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import BatchSpanProcessor, ConsoleSpanExporter
from opentelemetry.trace import (
    NonRecordingSpan,
    Span,
    SpanContext,
    SpanKind,
    Status,
    StatusCode,
    TraceFlags,
    TraceState,
)

log = structlog.get_logger()

_TRACER_NAME = "centaur.api"
_LAMINAR_METADATA_PREFIX = "lmnr.association.properties.metadata."
_configured = False
_instrumented = False


def configure_otel() -> None:
    """Configure first-party OpenTelemetry tracing for the API process.

    Export is opt-in: set OTEL_EXPORTER_OTLP_ENDPOINT or
    OTEL_EXPORTER_OTLP_TRACES_ENDPOINT for OTLP/HTTP, or set
    OTEL_TRACES_EXPORTER=console for local debugging.
    """
    global _configured, _instrumented
    if _configured:
        return
    _configured = True

    exporter = (os.getenv("OTEL_TRACES_EXPORTER") or "").strip().lower()
    endpoint = (
        os.getenv("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
        or os.getenv("OTEL_EXPORTER_OTLP_ENDPOINT")
        or ""
    ).strip()
    if exporter in {"none", "false", "0", "off"}:
        log.info("otel_tracing_disabled", reason="OTEL_TRACES_EXPORTER")
        return
    if not endpoint and exporter != "console":
        log.info("otel_tracing_disabled", reason="missing_otlp_endpoint")
        return

    resource = Resource.create(
        {
            "service.name": os.getenv("OTEL_SERVICE_NAME", "centaur-api"),
            "service.namespace": "centaur",
            "deployment.environment": (
                os.getenv("CENTAUR_ENVIRONMENT")
                or os.getenv("DEPLOY_ENV")
                or os.getenv("ENVIRONMENT")
                or "local"
            ),
        }
    )
    provider = TracerProvider(resource=resource)
    if exporter == "console":
        provider.add_span_processor(BatchSpanProcessor(ConsoleSpanExporter()))
    else:
        provider.add_span_processor(BatchSpanProcessor(OTLPSpanExporter()))
    trace.set_tracer_provider(provider)
    log.info(
        "otel_tracing_configured",
        exporter=exporter or "otlp",
        endpoint=endpoint or None,
    )

    if not _instrumented:
        _instrumented = True
        try:
            from opentelemetry.instrumentation.httpx import HTTPXClientInstrumentor

            if _db_auto_instrumentation_enabled():
                from opentelemetry.instrumentation.asyncpg import AsyncPGInstrumentor

                AsyncPGInstrumentor().instrument()
            HTTPXClientInstrumentor().instrument()
        except Exception:
            log.warning("otel_auto_instrumentation_failed", exc_info=True)


def _db_auto_instrumentation_enabled() -> bool:
    return (os.getenv("OTEL_INSTRUMENT_ASYNCPG") or "").strip().lower() in {
        "1",
        "true",
        "yes",
        "on",
    }


def tracer():
    return trace.get_tracer(_TRACER_NAME)


def _clean_attr_value(value: Any) -> Any:
    if value is None:
        return None
    if isinstance(value, (str, bool, int, float)):
        if isinstance(value, str) and len(value) > 512:
            return value[:509] + "..."
        return value
    if isinstance(value, (list, tuple)):
        cleaned = [_clean_attr_value(item) for item in value]
        return [item for item in cleaned if isinstance(item, (str, bool, int, float))]
    return str(value)[:512]


def clean_attributes(attributes: Mapping[str, Any] | None) -> dict[str, Any]:
    cleaned: dict[str, Any] = {}
    for key, value in (attributes or {}).items():
        cleaned_value = _clean_attr_value(value)
        if cleaned_value is not None:
            cleaned[str(key)] = cleaned_value
    return cleaned


def _laminar_metadata_value(value: Any) -> Any:
    if value is None:
        return None
    if isinstance(value, (str, bool, int, float)):
        return value
    if isinstance(value, (list, tuple)) and all(
        isinstance(item, (str, bool, int, float)) for item in value
    ):
        return list(value)
    try:
        return json.dumps(value, sort_keys=True, separators=(",", ":"), default=str)
    except TypeError:
        return str(value)


def laminar_trace_metadata_attributes(
    metadata: Mapping[str, Any] | None,
) -> dict[str, Any]:
    attributes: dict[str, Any] = {}
    for key, value in (metadata or {}).items():
        metadata_key = str(key).strip()
        if not metadata_key:
            continue
        cleaned_value = _laminar_metadata_value(value)
        if cleaned_value is not None:
            attributes[f"{_LAMINAR_METADATA_PREFIX}{metadata_key}"] = cleaned_value
    return attributes


def set_laminar_trace_metadata(span: Span, metadata: Mapping[str, Any] | None) -> None:
    set_span_attributes(span, laminar_trace_metadata_attributes(metadata))


@contextmanager
def start_span(
    name: str,
    *,
    attributes: Mapping[str, Any] | None = None,
    parent_context=None,
    kind: SpanKind = SpanKind.INTERNAL,
):
    if parent_context is not None:
        current_ctx = trace.get_current_span().get_span_context()
        parent_ctx = trace.get_current_span(parent_context).get_span_context()
        if (
            current_ctx.is_valid
            and parent_ctx.is_valid
            and current_ctx.trace_id == parent_ctx.trace_id
        ):
            parent_context = None

    with tracer().start_as_current_span(
        name,
        context=parent_context,
        kind=kind,
        attributes=clean_attributes(attributes),
    ) as span:
        yield span


def set_span_attributes(span: Span, attributes: Mapping[str, Any] | None) -> None:
    for key, value in clean_attributes(attributes).items():
        span.set_attribute(key, value)


def add_span_event(
    name: str,
    attributes: Mapping[str, Any] | None = None,
    *,
    span: Span | None = None,
) -> None:
    target = span or trace.get_current_span()
    if not target:
        return
    target.add_event(name, clean_attributes(attributes))


def record_exception(span: Span, exc: BaseException) -> None:
    span.record_exception(exc)
    span.set_status(Status(StatusCode.ERROR, str(exc)[:512]))


def mark_error(span: Span, message: str) -> None:
    span.set_status(Status(StatusCode.ERROR, message[:512]))


def span_context_to_dict(span: Span | None = None) -> dict[str, Any] | None:
    target = span or trace.get_current_span()
    span_context = target.get_span_context() if target else None
    if not span_context or not span_context.is_valid:
        return None
    return {
        "trace_id": f"{span_context.trace_id:032x}",
        "span_id": f"{span_context.span_id:016x}",
        "trace_flags": int(span_context.trace_flags),
        "trace_state": list(span_context.trace_state.items()),
    }


def context_from_serialized(value: Any):
    if not isinstance(value, dict):
        return None
    try:
        trace_id = int(str(value.get("trace_id") or ""), 16)
        span_id = int(str(value.get("span_id") or ""), 16)
    except ValueError:
        return None
    if trace_id <= 0 or span_id <= 0:
        return None
    trace_state_entries = value.get("trace_state")
    trace_state = TraceState(
        trace_state_entries if isinstance(trace_state_entries, list) else []
    )
    span_context = SpanContext(
        trace_id=trace_id,
        span_id=span_id,
        is_remote=True,
        trace_flags=TraceFlags(int(value.get("trace_flags") or TraceFlags.SAMPLED)),
        trace_state=trace_state,
    )
    if not span_context.is_valid:
        return None
    return trace.set_span_in_context(NonRecordingSpan(span_context))


def context_from_headers(headers: Mapping[str, str]):
    return propagate.extract(headers)


def current_traceparent(span: Span | None = None) -> str | None:
    span_context = (span or trace.get_current_span()).get_span_context()
    if not span_context or not span_context.is_valid:
        return None
    return (
        f"00-{span_context.trace_id:032x}-"
        f"{span_context.span_id:016x}-{int(span_context.trace_flags) & 0x01:02x}"
    )
