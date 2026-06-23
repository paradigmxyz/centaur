#!/usr/bin/env python3
"""Python workflow host for api-rs Absurd workflows.

The host speaks newline-delimited JSON over stdin/stdout. It imports existing
Centaur workflow modules, runs ``handler(input, ctx)``, and delegates durable
context operations back to api-rs.
"""

from __future__ import annotations

import asyncio
import ast
import dataclasses
import importlib.util
import inspect
import json
import os
import shutil
import subprocess
import sys
import traceback
import types
import typing
import urllib.request
from pathlib import Path
from typing import Any


class ProtocolError(RuntimeError):
    pass


class WorkflowContext:
    def __init__(
        self,
        rpc: "RpcClient",
        *,
        run_id: str,
        task_id: str,
        workflow_name: str,
        pool: Any = None,
    ) -> None:
        self._rpc = rpc
        self.run_id = run_id
        self.task_id = task_id
        self.workflow_name = workflow_name
        self._pool = pool

    def log(self, event: str, **fields: Any) -> None:
        self._rpc.notify(
            {
                "type": "ctx.log",
                "message": event,
                "fields": fields,
            }
        )

    async def step(self, name: str, fn: Any, *, retry: Any = None, timeout: Any = None) -> Any:
        del retry, timeout
        started = await self._rpc.request({"type": "ctx.step.get", "step": name})
        if started.get("done"):
            return started.get("value")

        value = fn()
        if inspect.isawaitable(value):
            value = await value
        await self._rpc.request(
            {
                "type": "ctx.step.put",
                "checkpoint_name": started["checkpoint_name"],
                "value": value,
            }
        )
        return value

    async def agent_turn(self, text: str | None = None, **kwargs: Any) -> Any:
        args = dict(kwargs)
        if text is not None:
            args.setdefault("text", text)
        return await self._rpc.request({"type": "ctx.agent_turn", "args": args})

    async def run_agent(self, text: str | None = None, **kwargs: Any) -> Any:
        return await self.agent_turn(text, **kwargs)

    async def call_tool(self, tool: str, method: str, args: dict[str, Any] | None = None) -> Any:
        tool_shim = resolve_tool_shim()
        if tool_shim is not None:
            # Sandboxed workflow hosts cannot rely on api-rs having a /tools
            # backend. Use the generated catalog's method bridge for durable
            # workflow ctx.call_tool(...); interactive agents use tool CLIs.
            return await call_tool_shim(tool_shim, tool, method, args or {})
        return await self._rpc.request(
            {
                "type": "ctx.call_tool",
                "tool": tool,
                "method": method,
                "args": args or {},
            }
        )

    async def post_to_slack(self, channel: str, text: str, **kwargs: Any) -> Any:
        return await self._rpc.request(
            {
                "type": "ctx.post_to_slack",
                "channel": channel,
                "text": text,
                "args": kwargs,
            }
        )


class RpcClient:
    def __init__(self) -> None:
        self._next_request_id = 1
        self._pending: dict[str, asyncio.Future[Any]] = {}
        self._notifications: set[asyncio.Task[None]] = set()
        self._write_lock = asyncio.Lock()

    async def write(self, payload: dict[str, Any]) -> None:
        async with self._write_lock:
            sys.stdout.write(json.dumps(payload, separators=(",", ":"), default=str) + "\n")
            sys.stdout.flush()

    def notify(self, payload: dict[str, Any]) -> None:
        task = asyncio.create_task(self.write(payload))
        self._notifications.add(task)
        task.add_done_callback(self._notifications.discard)

    async def drain_notifications(self) -> None:
        if self._notifications:
            await asyncio.gather(*list(self._notifications), return_exceptions=True)

    async def request(self, payload: dict[str, Any]) -> Any:
        request_id = str(self._next_request_id)
        self._next_request_id += 1
        payload = dict(payload)
        payload["request_id"] = request_id
        loop = asyncio.get_running_loop()
        fut: asyncio.Future[Any] = loop.create_future()
        self._pending[request_id] = fut
        await self.write(payload)
        return await fut

    def resolve(self, response: dict[str, Any]) -> None:
        request_id = str(response.get("request_id") or "")
        fut = self._pending.pop(request_id, None)
        if fut is None:
            raise ProtocolError(f"response for unknown request_id {request_id!r}")
        if response.get("ok"):
            fut.set_result(response.get("value"))
        else:
            fut.set_exception(RuntimeError(str(response.get("error") or "context RPC failed")))


_METRIC_RPC: RpcClient | None = None


def resolve_tool_shim() -> str | None:
    if tool_shim := shutil.which("centaur-tools"):
        return tool_shim
    fallback = Path("/home/agent/.local/bin/centaur-tools")
    if fallback.exists():
        return str(fallback)
    installer = Path("/usr/local/bin/install-tool-shims")
    if installer.exists():
        subprocess.run(
            [str(installer)],
            check=False,
            stdout=sys.stderr,
            stderr=sys.stderr,
        )
        if tool_shim := shutil.which("centaur-tools"):
            return tool_shim
        if fallback.exists():
            return str(fallback)
    return None


async def call_tool_shim(
    tool_shim: str, tool: str, method: str, args: dict[str, Any]
) -> Any:
    proc = await asyncio.create_subprocess_exec(
        tool_shim,
        "call",
        tool,
        method,
        json.dumps(args, separators=(",", ":"), default=str),
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    stdout, stderr = await proc.communicate()
    text = stdout.decode(errors="replace").strip()
    err = stderr.decode(errors="replace").strip()
    if proc.returncode != 0:
        detail = err or text or f"exit code {proc.returncode}"
        raise RuntimeError(f"centaur-tools call {tool}.{method} failed: {detail}")
    if not text:
        return None
    return json.loads(text)


@dataclasses.dataclass
class RegisteredWorkflow:
    workflow_name: str
    source_path: str
    handler: Any
    input_cls: type | None
    webhooks: Any
    schedule: Any


def install_api_compat_module() -> None:
    api_mod = sys.modules.get("api")
    if api_mod is None:
        try:
            import api as imported_api  # type: ignore

            api_mod = imported_api
        except ImportError:
            api_mod = types.ModuleType("api")
            api_mod.__path__ = []  # Mark as package so compat submodules can import.
            sys.modules["api"] = api_mod

    workflow_engine = types.ModuleType("api.workflow_engine")
    workflow_engine.WorkflowContext = WorkflowContext
    sys.modules["api.workflow_engine"] = workflow_engine
    setattr(api_mod, "workflow_engine", workflow_engine)

    runtime_control = types.ModuleType("api.runtime_control")
    runtime_control.canonical_json = canonical_json
    runtime_control.decode_jsonb = decode_jsonb
    sys.modules.setdefault("api.runtime_control", runtime_control)
    setattr(api_mod, "runtime_control", runtime_control)

    install_vm_metrics_compat_module(api_mod)

    install_centaur_sdk_compat_module()


def canonical_json(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), default=str)


def decode_jsonb(value: Any, fallback: Any) -> Any:
    if value is None:
        return fallback
    if isinstance(value, str):
        try:
            return json.loads(value)
        except json.JSONDecodeError:
            return fallback
    return value


def install_centaur_sdk_compat_module() -> None:
    if "centaur_sdk" in sys.modules:
        return
    try:
        __import__("centaur_sdk")
        return
    except ImportError:
        pass
    centaur_sdk = types.ModuleType("centaur_sdk")

    def secret(name: str, default: str | None = None) -> str:
        return os.getenv(name, default or "")

    centaur_sdk.secret = secret
    sys.modules["centaur_sdk"] = centaur_sdk


_METRIC_COUNTERS: dict[tuple[str, tuple[tuple[str, str], ...]], float] = {}
_METRIC_GAUGES: dict[tuple[str, tuple[tuple[str, str], ...]], float] = {}
_METRIC_HISTOGRAMS: dict[tuple[str, tuple[tuple[str, str], ...]], dict[str, Any]] = {}
_COMPANY_CONTEXT_DOCUMENT_SIZE_BUCKETS = [
    100,
    500,
    1_000,
    5_000,
    10_000,
    25_000,
    50_000,
    100_000,
    250_000,
    500_000,
]


def install_vm_metrics_compat_module(api_mod: types.ModuleType) -> None:
    if "api.vm_metrics" in sys.modules:
        setattr(api_mod, "vm_metrics", sys.modules["api.vm_metrics"])
        return
    try:
        import api.vm_metrics as vm_metrics  # type: ignore

        setattr(api_mod, "vm_metrics", vm_metrics)
        return
    except ImportError:
        pass

    vm_metrics = types.ModuleType("api.vm_metrics")
    vm_metrics.record_etl_items_deleted = record_etl_items_deleted
    vm_metrics.record_etl_items_enqueued = record_etl_items_enqueued
    vm_metrics.record_etl_items_failed = record_etl_items_failed
    vm_metrics.record_etl_items_seen = record_etl_items_seen
    vm_metrics.record_etl_items_upserted = record_etl_items_upserted
    vm_metrics.record_slack_etl_rate_limit = record_slack_etl_rate_limit
    vm_metrics.set_etl_active_scopes = set_etl_active_scopes
    vm_metrics.set_etl_backfill_job_age_seconds = set_etl_backfill_job_age_seconds
    vm_metrics.set_etl_backfill_jobs = set_etl_backfill_jobs
    vm_metrics.set_etl_failed_scopes = set_etl_failed_scopes
    vm_metrics.set_etl_scope_sync_freshness_seconds = (
        set_etl_scope_sync_freshness_seconds
    )
    vm_metrics.record_company_context_documents_changed = (
        record_company_context_documents_changed
    )
    vm_metrics.observe_company_context_document_size = (
        observe_company_context_document_size
    )
    vm_metrics.set_company_context_projection_lag = (
        set_company_context_projection_lag
    )
    sys.modules["api.vm_metrics"] = vm_metrics
    setattr(api_mod, "vm_metrics", vm_metrics)


def record_etl_items_seen(
    source: str, source_type: str, item_type: str, count: int
) -> None:
    increment_metric(
        "etl_items_seen_total",
        count,
        source=source,
        source_type=source_type,
        item_type=item_type,
    )


def record_etl_items_enqueued(
    source: str, source_type: str, item_type: str, count: int
) -> None:
    increment_metric(
        "etl_items_enqueued_total",
        count,
        source=source,
        source_type=source_type,
        item_type=item_type,
    )


def record_etl_items_upserted(
    source: str, source_type: str, item_type: str, count: int
) -> None:
    increment_metric(
        "etl_items_upserted_total",
        count,
        source=source,
        source_type=source_type,
        item_type=item_type,
    )


def record_etl_items_deleted(
    source: str, source_type: str, item_type: str, count: int
) -> None:
    increment_metric(
        "etl_items_deleted_total",
        count,
        source=source,
        source_type=source_type,
        item_type=item_type,
    )


def record_etl_items_failed(
    source: str,
    source_type: str,
    item_type: str,
    reason: str,
    count: int = 1,
) -> None:
    increment_metric(
        "etl_items_failed_total",
        count,
        source=source,
        source_type=source_type,
        item_type=item_type,
        reason=reason,
    )


def record_slack_etl_rate_limit(
    workflow: str,
    method: str,
    outcome: str,
    retry_after_seconds: int | float,
) -> None:
    retry_after = max(float(retry_after_seconds), 0.0)
    labels = {
        "workflow": workflow,
        "method": method,
        "outcome": outcome,
    }
    increment_metric("slack_etl_rate_limits_total", 1, **labels)
    increment_metric(
        "slack_etl_rate_limit_retry_after_seconds_total",
        retry_after,
        **labels,
    )


def set_etl_active_scopes(source: str, count: int) -> None:
    set_gauge(
        "etl_active_scopes",
        max(count, 0),
        source=source,
    )


def set_etl_failed_scopes(source: str, count: int) -> None:
    set_gauge(
        "etl_failed_scopes",
        max(count, 0),
        source=source,
    )


def set_etl_scope_sync_freshness_seconds(
    source: str,
    freshness_s: int | float,
) -> None:
    set_gauge(
        "etl_scope_sync_freshness_seconds",
        max(float(freshness_s), 0.0),
        source=source,
    )


def set_etl_backfill_jobs(
    source: str,
    job_type: str,
    status: str,
    count: int,
) -> None:
    set_gauge(
        "etl_backfill_jobs",
        max(count, 0),
        source=source,
        job_type=job_type,
        status=status,
    )


def set_etl_backfill_job_age_seconds(
    source: str,
    job_type: str,
    status: str,
    age_s: int | float,
) -> None:
    set_gauge(
        "etl_backfill_job_age_seconds",
        max(float(age_s), 0.0),
        source=source,
        job_type=job_type,
        status=status,
    )


def record_company_context_documents_changed(
    source: str,
    source_type: str,
    action: str,
    count: int = 1,
) -> None:
    increment_metric(
        "company_context_documents_changed_total",
        count,
        source=source,
        source_type=source_type,
        action=action,
    )


def observe_company_context_document_size(
    source: str, source_type: str, chars: int
) -> None:
    observe_histogram(
        "company_context_document_size_chars",
        max(chars, 0),
        _COMPANY_CONTEXT_DOCUMENT_SIZE_BUCKETS,
        source=source,
        source_type=source_type,
    )


def set_company_context_projection_lag(source: str, projection_lag_s: float) -> None:
    set_gauge(
        "company_context_projection_lag_seconds",
        max(projection_lag_s, 0.0),
        source=source,
    )


def increment_metric(metric: str, count: int, **labels: str) -> None:
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
        {
            "buckets": {bucket: 0 for bucket in buckets},
            "count": 0,
            "sum": 0.0,
        },
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
    lines.append(
        format_metric_line(
            f"{metric}_bucket",
            {**labels, "le": "+Inf"},
            histogram["count"],
        )
    )
    lines.append(format_metric_line(f"{metric}_count", labels, histogram["count"]))
    lines.append(format_metric_line(f"{metric}_sum", labels, histogram["sum"]))
    push_metric_lines(lines)


def metric_key(
    metric: str, labels: dict[str, str]
) -> tuple[str, tuple[tuple[str, str], ...]]:
    return (metric, tuple(sorted((key, str(value)) for key, value in labels.items())))


def emit_metric_event(
    kind: str, metric: str, value: int | float, labels: dict[str, str]
) -> None:
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
        if separator and key.strip() in {
            "deployment.environment",
            "deployment.environment.name",
        }:
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


def workflow_dirs() -> list[Path]:
    dirs = []
    raw = os.getenv("WORKFLOW_DIRS", "")
    for entry in raw.split(":"):
        entry = entry.strip()
        if entry:
            path = Path(entry).expanduser().resolve()
            if path.is_dir():
                dirs.append(path)
    return dirs


def workflow_allowed_names() -> set[str]:
    raw = os.getenv("WORKFLOW_ALLOWED_NAMES", "")
    return {
        name
        for name in (part.strip() for part in raw.replace(",", " ").split())
        if name
    }


def workflow_enabled(workflow_name: str) -> bool:
    mode = os.getenv("WORKFLOW_ENABLE_MODE", "").strip().lower()
    if not mode or mode == "all":
        return True
    if mode == "allowlist":
        return workflow_name.strip() in workflow_allowed_names()
    raise RuntimeError(f'WORKFLOW_ENABLE_MODE must be "all" or "allowlist", got {mode!r}')


def configure_workflow_import_paths(dirs: list[Path]) -> None:
    for directory in dirs:
        candidate_paths = [directory.parent, directory]
        if directory.name == "workflows":
            candidate_paths.append(directory.parent / "services" / "api")
        for path in candidate_paths:
            if path.is_dir() and str(path) not in sys.path:
                sys.path.insert(0, str(path))


def module_name_for(path: Path) -> str:
    return f"_centaur_workflow_host.{path.stem}_{abs(hash(str(path)))}"


def load_workflow_file(path: Path) -> RegisteredWorkflow | None:
    spec = importlib.util.spec_from_file_location(module_name_for(path), path)
    if spec is None or spec.loader is None:
        return None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)

    workflow_name = getattr(module, "WORKFLOW_NAME", None)
    handler = getattr(module, "handler", None)
    if not isinstance(workflow_name, str) or not callable(handler):
        return None
    return RegisteredWorkflow(
        workflow_name=workflow_name,
        source_path=str(path),
        handler=handler,
        input_cls=getattr(module, "Input", None),
        webhooks=getattr(module, "WEBHOOKS", None),
        schedule=getattr(module, "SCHEDULE", None),
    )


def has_workflow_name_assignment(path: Path) -> bool:
    try:
        tree = ast.parse(path.read_text())
    except Exception:
        return False
    for node in tree.body:
        if isinstance(node, ast.Assign):
            for target in node.targets:
                if isinstance(target, ast.Name) and target.id == "WORKFLOW_NAME":
                    return True
        elif isinstance(node, ast.AnnAssign):
            target = node.target
            if isinstance(target, ast.Name) and target.id == "WORKFLOW_NAME":
                return True
    return False


def discover_workflows() -> dict[str, RegisteredWorkflow]:
    dirs = workflow_dirs()
    configure_workflow_import_paths(dirs)
    install_api_compat_module()
    discovered: dict[str, RegisteredWorkflow] = {}
    for directory in dirs:
        for path in sorted(directory.rglob("*.py")):
            if path.name == "__init__.py" or path.name.startswith("_"):
                continue
            if not has_workflow_name_assignment(path):
                continue
            try:
                registered = load_workflow_file(path)
            except Exception as exc:
                print(f"workflow_load_error path={path} error={exc}", file=sys.stderr)
                continue
            if registered is None:
                continue
            if not workflow_enabled(registered.workflow_name):
                continue
            if registered.workflow_name in discovered:
                raise RuntimeError(f"duplicate workflow name {registered.workflow_name!r}")
            discovered[registered.workflow_name] = registered
    return discovered


def coerce_value(value: Any, target_type: type) -> Any:
    if not dataclasses.is_dataclass(target_type):
        return value
    if isinstance(value, target_type):
        return value
    if not isinstance(value, dict):
        return value
    fields = {field.name for field in dataclasses.fields(target_type)}
    try:
        hints = typing.get_type_hints(target_type)
    except Exception:
        hints = {}
    coerced = {}
    for key, raw in value.items():
        if key not in fields:
            continue
        hint = hints.get(key)
        if isinstance(hint, type) and dataclasses.is_dataclass(hint) and isinstance(raw, dict):
            coerced[key] = coerce_value(raw, hint)
        else:
            coerced[key] = raw
    return target_type(**coerced)


def coerce_input(raw: Any, input_cls: type | None) -> Any:
    if input_cls is None:
        return raw
    try:
        return coerce_value(raw, input_cls)
    except Exception:
        return raw


async def create_pool() -> Any:
    database_url = os.getenv("DATABASE_URL", "").strip()
    if not database_url:
        return None
    try:
        import asyncpg  # type: ignore
    except ImportError:
        return None
    return await asyncpg.create_pool(database_url)


def jsonable(value: Any) -> Any:
    if dataclasses.is_dataclass(value):
        return dataclasses.asdict(value)
    json.dumps(value, default=str)
    return value


def normalize_webhook_auth(auth: Any) -> dict[str, Any]:
    if auth is None or auth == "none":
        return {"type": "none"}
    if isinstance(auth, str):
        return {"type": auth}
    if isinstance(auth, dict):
        if "type" not in auth:
            return {"type": "none", **auth}
        return auth
    return {"type": "none"}


def normalize_webhook_spec(workflow: RegisteredWorkflow, raw: Any) -> dict[str, Any] | None:
    if not isinstance(raw, dict):
        return None
    spec = dict(raw)
    slug = spec.get("slug")
    if not isinstance(slug, str) or not slug.strip():
        return None
    spec["slug"] = slug.strip()
    spec["auth"] = normalize_webhook_auth(spec.get("auth"))
    return {
        "workflow_name": workflow.workflow_name,
        "source_path": workflow.source_path,
        "spec": spec,
    }


def normalize_webhooks(workflow: RegisteredWorkflow) -> list[dict[str, Any]]:
    raw = workflow.webhooks
    if raw is None:
        return []
    if isinstance(raw, dict):
        raw = [raw]
    if not isinstance(raw, list):
        return []
    return [
        spec
        for spec in (normalize_webhook_spec(workflow, item) for item in raw)
        if spec is not None
    ]


def normalize_schedule(workflow: RegisteredWorkflow) -> dict[str, Any] | None:
    raw = workflow.schedule
    if not isinstance(raw, dict):
        return None
    schedule = dict(raw)
    schedule.setdefault("schedule_id", workflow.workflow_name)
    schedule.setdefault("workflow_name", workflow.workflow_name)
    schedule.setdefault("source_path", workflow.source_path)
    return schedule


async def run_workflow(message: dict[str, Any], rpc: RpcClient) -> dict[str, Any]:
    global _METRIC_RPC
    workflows = discover_workflows()
    workflow_name = str(message.get("workflow_name") or "")
    registered = workflows.get(workflow_name)
    if registered is None:
        raise RuntimeError(f"unknown workflow_name {workflow_name!r}")

    pool = await create_pool()
    ctx = WorkflowContext(
        rpc,
        run_id=str(message.get("run_id") or ""),
        task_id=str(message.get("task_id") or ""),
        workflow_name=workflow_name,
        pool=pool,
    )
    previous_metric_rpc = _METRIC_RPC
    _METRIC_RPC = rpc
    try:
        inp = coerce_input(message.get("input") or {}, registered.input_cls)
        result = registered.handler(inp, ctx)
        if inspect.isawaitable(result):
            result = await result
        return {
            "type": "workflow.result",
            "workflow_run_id": ctx.run_id,
            "run_id": ctx.run_id,
            "workflow_task_id": ctx.task_id,
            "task_id": ctx.task_id,
            "workflow_name": ctx.workflow_name,
            "result": jsonable(result),
        }
    finally:
        await rpc.drain_notifications()
        _METRIC_RPC = previous_metric_rpc
        if pool is not None:
            await pool.close()


def discovery_payload() -> dict[str, Any]:
    workflows = discover_workflows()
    return {
        "type": "workflow.discovery",
        "workflows": [
            {
                "workflow_name": workflow.workflow_name,
                "source_path": workflow.source_path,
                "webhooks": normalize_webhooks(workflow),
                "schedule": normalize_schedule(workflow),
            }
            for workflow in workflows.values()
        ],
    }


async def main() -> int:
    rpc = RpcClient()
    stdin_queue: asyncio.Queue[dict[str, Any] | None] = asyncio.Queue()
    completion_queue: asyncio.Queue[dict[str, Any]] = asyncio.Queue()
    active_workflow: asyncio.Task[dict[str, Any]] | None = None

    async def read_stdin() -> None:
        while True:
            line = await asyncio.to_thread(sys.stdin.readline)
            if line == "":
                await stdin_queue.put(None)
                return
            await stdin_queue.put(json.loads(line))

    asyncio.create_task(read_stdin())

    def watch_workflow(task: asyncio.Task[dict[str, Any]]) -> None:
        try:
            completion_queue.put_nowait(task.result())
        except Exception as exc:
            completion_queue.put_nowait(
                {
                    "type": "workflow.error",
                    "message": str(exc),
                    "traceback": traceback.format_exc(),
                }
            )

    while True:
        stdin_get = asyncio.create_task(stdin_queue.get())
        completion_get = asyncio.create_task(completion_queue.get())
        done, pending = await asyncio.wait(
            {stdin_get, completion_get},
            return_when=asyncio.FIRST_COMPLETED,
        )
        for task in pending:
            task.cancel()

        if completion_get in done:
            active_workflow = None
            await rpc.write(completion_get.result())
            return 0

        message = stdin_get.result()
        if message is None:
            if active_workflow is not None:
                continue
            await asyncio.sleep(0.1)
            asyncio.create_task(read_stdin())
            continue
        message_type = message.get("type")
        try:
            if message_type == "ctx.response":
                rpc.resolve(message)
                continue
            if message_type == "workflow.discover":
                await rpc.write(discovery_payload())
                return 0
            if message_type == "workflow.start":
                if active_workflow is not None:
                    await rpc.write(
                        {
                            "type": "workflow.error",
                            "message": "workflow host already has an active workflow",
                        }
                    )
                    continue
                active_workflow = asyncio.create_task(run_workflow(message, rpc))
                active_workflow.add_done_callback(watch_workflow)
                continue
            raise ProtocolError(f"unknown message type {message_type!r}")
        except Exception as exc:
            await rpc.write(
                {
                    "type": "host.error",
                    "message": str(exc),
                    "traceback": traceback.format_exc(),
                }
            )


if __name__ == "__main__":
    raise SystemExit(asyncio.run(main()))
