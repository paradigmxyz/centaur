from __future__ import annotations

import os
import sys
import urllib.request
from pathlib import Path
from typing import Any


_METRIC_RPC: Any | None = None
_METRIC_COUNTERS: dict[tuple[str, tuple[tuple[str, str], ...]], float] = {}
_METRIC_GAUGES: dict[tuple[str, tuple[tuple[str, str], ...]], float] = {}
_METRIC_HISTOGRAMS: dict[tuple[str, tuple[tuple[str, str], ...]], dict[str, Any]] = {}


def set_metric_rpc(rpc: Any | None) -> None:
    global _METRIC_RPC
    _METRIC_RPC = rpc


def get_metric_rpc() -> Any | None:
    return _METRIC_RPC


def increment_metric(metric: str, count: int | float, **labels: str) -> None:
    if count < 0:
        return
    labels = metric_runtime_labels(labels)
    key = metric_key(metric, labels)
    _METRIC_COUNTERS[key] = _METRIC_COUNTERS.get(key, 0.0) + float(count)
    emit_metric_event("counter", metric, count, labels)
    push_metric_lines([format_metric_line(metric, labels, _METRIC_COUNTERS[key])])


def set_gauge(metric: str, value: float, **labels: str) -> None:
    labels = metric_runtime_labels(labels)
    key = metric_key(metric, labels)
    _METRIC_GAUGES[key] = float(value)
    emit_metric_event("gauge", metric, float(value), labels)
    push_metric_lines([format_metric_line(metric, labels, _METRIC_GAUGES[key])])


def observe_histogram(
    metric: str,
    value: int | float,
    buckets: list[int],
    **labels: str,
) -> None:
    labels = metric_runtime_labels(labels)
    key = metric_key(metric, labels)
    histogram = _METRIC_HISTOGRAMS.setdefault(
        key,
        {"buckets": {bucket: 0 for bucket in buckets}, "count": 0, "sum": 0.0},
    )
    numeric = float(value)
    histogram["count"] += 1
    histogram["sum"] += numeric
    emit_metric_event("histogram", metric, numeric, labels)
    for bucket in buckets:
        if numeric <= bucket:
            histogram["buckets"][bucket] += 1

    lines = []
    for bucket in buckets:
        lines.append(
            format_metric_line(
                f"{metric}_bucket",
                {**labels, "le": str(float(bucket))},
                histogram["buckets"][bucket],
            )
        )
    lines.append(format_metric_line(f"{metric}_bucket", {**labels, "le": "+Inf"}, histogram["count"]))
    lines.append(format_metric_line(f"{metric}_count", labels, histogram["count"]))
    lines.append(format_metric_line(f"{metric}_sum", labels, histogram["sum"]))
    push_metric_lines(lines)


def metric_key(metric: str, labels: dict[str, str]) -> tuple[str, tuple[tuple[str, str], ...]]:
    return (metric, tuple(sorted((key, str(value)) for key, value in labels.items())))


def emit_metric_event(kind: str, metric: str, value: int | float, labels: dict[str, str]) -> None:
    if _METRIC_RPC is None:
        return
    _METRIC_RPC.notify(
        {
            "type": "ctx.metric",
            "kind": kind,
            "name": metric,
            "value": value,
            "labels": labels,
        }
    )


def metric_runtime_labels(labels: dict[str, str]) -> dict[str, str]:
    runtime_labels = dict(labels)
    for key, value in default_metric_runtime_labels().items():
        runtime_labels.setdefault(key, value)
    return runtime_labels


def default_metric_runtime_labels() -> dict[str, str]:
    labels: dict[str, str] = {}
    namespace = runtime_namespace()
    if namespace:
        labels["namespace"] = namespace
    environment = runtime_environment()
    if environment:
        labels["environment"] = environment
    return labels


def runtime_namespace() -> str | None:
    for name in (
        "METRICS_NAMESPACE",
        "KUBERNETES_NAMESPACE",
        "POD_NAMESPACE",
        "SESSION_SANDBOX_K8S_NAMESPACE",
    ):
        value = clean_metric_label_value(os.environ.get(name))
        if value:
            return value

    try:
        namespace = Path("/var/run/secrets/kubernetes.io/serviceaccount/namespace")
        if namespace.exists():
            return clean_metric_label_value(namespace.read_text(encoding="utf-8"))
    except OSError:
        return None
    return None


def runtime_environment() -> str | None:
    for name in ("METRICS_ENVIRONMENT", "ENVIRONMENT", "DEPLOYMENT_ENVIRONMENT"):
        value = clean_metric_label_value(os.environ.get(name))
        if value:
            return value

    for attr in os.environ.get("OTEL_RESOURCE_ATTRIBUTES", "").split(","):
        key, separator, value = attr.partition("=")
        if separator and key.strip() in {"deployment.environment", "deployment.environment.name"}:
            cleaned = clean_metric_label_value(value)
            if cleaned:
                return cleaned

    namespace = runtime_namespace()
    if namespace == "centaur-system":
        return "production"
    if namespace and namespace.startswith("stg-"):
        return "staging"
    return None


def clean_metric_label_value(value: str | None) -> str | None:
    if value is None:
        return None
    value = value.strip()
    return value or None


def format_metric_line(metric: str, labels: dict[str, str], value: float) -> str:
    if labels:
        label_text = ",".join(
            f'{key}="{escape_label_value(str(label_value))}"'
            for key, label_value in sorted(labels.items())
        )
        return f"{metric}{{{label_text}}} {value}"
    return f"{metric} {value}"


def escape_label_value(value: str) -> str:
    return value.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")


def push_metric_lines(lines: list[str]) -> None:
    if not victoria_metrics_push_enabled():
        return
    payload = ("\n".join(lines) + "\n").encode("utf-8")
    request = urllib.request.Request(
        f"{victoria_metrics_url().rstrip('/')}/api/v1/import/prometheus",
        data=payload,
        headers={"Content-Type": "text/plain"},
        method="POST",
    )
    try:
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        opener.open(request, timeout=2).close()
    except Exception as exc:
        print(f"workflow_metric_push_error error={exc}", file=sys.stderr)


def victoria_metrics_url() -> str:
    return os.environ.get("VICTORIAMETRICS_URL", "http://victoriametrics:8428")


def victoria_metrics_push_enabled() -> bool:
    return os.environ.get("VICTORIAMETRICS_PUSH_ENABLED", "1").strip().lower() not in {
        "0",
        "false",
        "no",
        "off",
    }
