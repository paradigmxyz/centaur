from __future__ import annotations

from opentelemetry.trace import NonRecordingSpan, SpanContext, TraceFlags

from api.otel import current_traceparent, laminar_trace_metadata_attributes


def test_current_traceparent_masks_trace_flags_to_w3c_sampled_bit() -> None:
    span = NonRecordingSpan(
        SpanContext(
            trace_id=int("00000000000040008000000000000123", 16),
            span_id=int("1111111122223333", 16),
            is_remote=False,
            trace_flags=TraceFlags(0x03),
            trace_state=[],
        )
    )

    assert (
        current_traceparent(span)
        == "00-00000000000040008000000000000123-1111111122223333-01"
    )


def test_laminar_trace_metadata_attributes_use_wire_format() -> None:
    assert laminar_trace_metadata_attributes(
        {
            "environment": "local",
            "execution_id": "exe_123",
            "payload": {"a": 1},
            "none": None,
        }
    ) == {
        "lmnr.association.properties.metadata.environment": "local",
        "lmnr.association.properties.metadata.execution_id": "exe_123",
        "lmnr.association.properties.metadata.payload": '{"a":1}',
    }
