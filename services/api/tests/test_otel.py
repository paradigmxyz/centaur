from __future__ import annotations

from opentelemetry.trace import NonRecordingSpan, SpanContext, TraceFlags

from api.otel import current_traceparent


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
