from __future__ import annotations

import asyncio
import json
import logging
import os
import signal
import sys
import time
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager, suppress
from pathlib import Path

import structlog
import structlog.contextvars
from fastapi import FastAPI
from fastapi.middleware.cors import CORSMiddleware

from api.api_keys import bootstrap_service_api_keys
from api.config import settings
from api.db import close_pool, create_pool
from api.laminar_tracing import (
    initialize_laminar,
    install_laminar_compat,
    set_span_attributes,
    set_trace_context,
    start_span,
)
from api.logging_config import configure_structlog
from api.retention import start_retention_sweeper, stop_retention_sweeper
from api.trace_context import get_or_create_thread_trace_id, traceparent_from_trace_id
from api.vm_metrics import (
    HTTP_REQUESTS_IN_PROGRESS,
    observe_http_request,
    start_push_loop,
    stop_push_loop,
)
from api.routers import (
    admin,
    attachments as attachments_mod,
    deprecated,
    health,
)
from api.routers import agent as agent_router_mod
from api.routers import workflows as workflow_router_mod
from api.tool_manager import ToolManager, load_plugins_config
from api.agent import reconcile_tick
from api.runtime_control import (
    recover_interrupted_executions_on_startup,
    start_execution_worker,
    stop_execution_worker,
)
from api.workflow_engine import (
    get_workflow_dirs,
    discover_workflow_handlers,
    start_workflow_worker,
    stop_workflow_worker,
    sync_registered_workflow_schedules,
)
from api.warm_pool import start_replenish_loop, stop_replenish_loop

configure_structlog()
install_laminar_compat()
initialize_laminar(service="api")

log = structlog.get_logger().bind(service="api")

# ---------------------------------------------------------------------------
# Graceful shutdown state
# ---------------------------------------------------------------------------
_shutting_down = False

SHUTDOWN_DRAIN_TIMEOUT_S = float(os.getenv("SHUTDOWN_DRAIN_TIMEOUT_S", "25"))


def is_shutting_down() -> bool:
    return _shutting_down


def _get_in_flight_count() -> float:
    """Read the current value of the in-flight request gauge."""
    key: tuple[tuple[str, str], ...] = ()
    with HTTP_REQUESTS_IN_PROGRESS._lock:
        return HTTP_REQUESTS_IN_PROGRESS._values.get(key, 0)


async def _drain_in_flight_requests() -> None:
    """Wait for in-flight HTTP requests to complete, up to the drain timeout."""
    deadline = time.monotonic() + SHUTDOWN_DRAIN_TIMEOUT_S
    while time.monotonic() < deadline:
        in_flight = _get_in_flight_count()
        if in_flight <= 0:
            log.info("graceful_drain_complete")
            return
        remaining = max(deadline - time.monotonic(), 0)
        log.info(
            "graceful_drain_waiting",
            in_flight=in_flight,
            remaining_s=round(remaining, 1),
        )
        await asyncio.sleep(min(0.5, remaining))
    log.warning(
        "graceful_drain_timeout",
        in_flight=_get_in_flight_count(),
        timeout_s=SHUTDOWN_DRAIN_TIMEOUT_S,
    )


# Suppress noisy uvicorn access logs; container-level logs already capture requests.
for _uvi_name in ("uvicorn.access",):
    logging.getLogger(_uvi_name).propagate = False


async def _watch_tools(pm: ToolManager) -> None:
    """Watch all plugin directories and auto-reload when files change."""
    from starlette.concurrency import run_in_threadpool
    from watchfiles import awatch

    watch_dirs = [d for d in pm.tools_dirs if d.exists()]
    log.info("tool_watcher_started", paths=[str(d) for d in watch_dirs])
    async for changes in awatch(*watch_dirs):
        changed_files = [str(p) for _, p in changes]
        log.info("tool_files_changed", files=changed_files)
        try:
            result = await run_in_threadpool(pm.reload)
            log.info("tools_auto_reloaded", **result)
        except Exception as e:
            log.error("tool_auto_reload_failed", error=str(e))


async def _watch_workflows() -> None:
    """Watch external workflow directories and auto-reload when files change."""
    from watchfiles import awatch

    watch_dirs = [d for d in get_workflow_dirs() if d.exists()]
    if not watch_dirs:
        return
    log.info("workflow_watcher_started", paths=[str(d) for d in watch_dirs])
    async for changes in awatch(*watch_dirs):
        changed_files = [str(p) for _, p in changes]
        log.info("workflow_files_changed", files=changed_files)
        try:
            result = discover_workflow_handlers()
            log.info(
                "workflows_auto_reloaded",
                workflows=list(result.keys()),
                count=len(result),
            )
        except Exception as e:
            log.error("workflow_auto_reload_failed", error=str(e))


async def _reconcile_loop() -> None:
    """Periodically reconcile sessions, enforce TTL, clean orphans."""
    while True:
        await asyncio.sleep(60)
        try:
            await reconcile_tick()
        except Exception:
            log.warning("reconcile_tick_failed", exc_info=True)


@asynccontextmanager
async def lifespan(app: FastAPI) -> AsyncIterator[None]:
    global _shutting_down

    app.state.db_pool = await create_pool(settings.database_url)
    await bootstrap_service_api_keys(app.state.db_pool)
    execution_worker_enabled = os.getenv(
        "EXECUTION_WORKER_ENABLED", "1"
    ).strip().lower() not in {
        "0",
        "false",
        "no",
    }
    workflow_worker_enabled = os.getenv(
        "WORKFLOW_WORKER_ENABLED", "1"
    ).strip().lower() not in {
        "0",
        "false",
        "no",
    }
    warm_pool_enabled = os.getenv("WARM_POOL_ENABLED", "1").strip().lower() not in {
        "0",
        "false",
        "no",
    }
    discover_workflow_handlers()
    await sync_registered_workflow_schedules(app.state.db_pool)
    # Bring up the API's own iron-proxy pod (replaces the previously
    # helm-managed deployment).
    try:
        from api.sandbox.kubernetes import KubernetesExecutorBackend
        from api.sandbox.registry import get_backend

        backend = get_backend()
        if isinstance(backend, KubernetesExecutorBackend):
            await backend.ensure_api_proxy_pod()
    except Exception as exc:
        log.warning("api_proxy_pod_bootstrap_failed", error=str(exc))
    if execution_worker_enabled:
        await recover_interrupted_executions_on_startup(app.state.db_pool)
        await start_execution_worker(app.state.db_pool)
    if workflow_worker_enabled:
        await start_workflow_worker(app.state.db_pool)
    start_push_loop(app.state.db_pool)
    await start_retention_sweeper(app.state.db_pool)
    watcher_task = asyncio.create_task(_watch_tools(tool_manager))
    wf_watcher_task = asyncio.create_task(_watch_workflows())
    reconcile_task = (
        asyncio.create_task(_reconcile_loop()) if execution_worker_enabled else None
    )
    if warm_pool_enabled:
        await start_replenish_loop()

    # Register signal handlers so we can mark ourselves as draining before
    # uvicorn triggers the lifespan teardown.  We re-raise to let uvicorn
    # proceed with its own shutdown sequence.
    _original_sigterm = signal.getsignal(signal.SIGTERM)
    _original_sigint = signal.getsignal(signal.SIGINT)

    def _on_shutdown_signal(signum: int, frame) -> None:
        global _shutting_down
        if not _shutting_down:
            _shutting_down = True
            log.info("graceful_shutdown_initiated", signal=signal.Signals(signum).name)
        # Re-install the original handler and re-raise so uvicorn proceeds
        # with its shutdown.
        signal.signal(
            signum, _original_sigterm if signum == signal.SIGTERM else _original_sigint
        )
        os.kill(os.getpid(), signum)

    signal.signal(signal.SIGTERM, _on_shutdown_signal)
    signal.signal(signal.SIGINT, _on_shutdown_signal)

    try:
        yield
    finally:
        _shutting_down = True
        log.info("graceful_shutdown_started")

        # Wait for in-flight HTTP requests to finish before tearing down
        # background workers and the DB pool.
        await _drain_in_flight_requests()

        await stop_retention_sweeper()
        await stop_push_loop()
        if warm_pool_enabled:
            await stop_replenish_loop()
        if workflow_worker_enabled:
            await stop_workflow_worker()
        if execution_worker_enabled:
            await stop_execution_worker()
        if reconcile_task is not None:
            reconcile_task.cancel()
        watcher_task.cancel()
        wf_watcher_task.cancel()
        with suppress(asyncio.CancelledError):
            await watcher_task
        with suppress(asyncio.CancelledError):
            await wf_watcher_task
        if reconcile_task is not None:
            with suppress(asyncio.CancelledError):
                await reconcile_task
        await close_pool(app.state.db_pool)
        log.info("graceful_shutdown_complete")


app = FastAPI(
    title="AI v2 API",
    version="0.1.0",
    lifespan=lifespan,
    redirect_slashes=False,
)

app.add_middleware(
    CORSMiddleware,
    allow_origins=settings.cors_origins,
    allow_credentials=False,
    allow_methods=["*"],
    allow_headers=["*"],
)


@app.middleware("http")
async def instrument_requests(request, call_next):
    if request.url.path == "/metrics":
        return await call_next(request)

    structlog.contextvars.clear_contextvars()

    trace_id = request.headers.get("x-trace-id")
    thread_key = request.headers.get("x-centaur-thread-key")

    if trace_id:
        structlog.contextvars.bind_contextvars(trace_id=trace_id)
    if thread_key:
        structlog.contextvars.bind_contextvars(thread_key=thread_key)

    if request.method == "POST" and request.url.path in (
        "/agent/execute",
        "/agent/spawn",
        "/agent/message",
        "/agent/messages",
        "/workflows/runs",
    ):
        try:
            body_bytes = await request.body()
            body_json = json.loads(body_bytes)
            body_tk = body_json.get("thread_key")
            if not body_tk and isinstance(body_json.get("input"), dict):
                body_tk = body_json["input"].get("thread_key")
            if body_tk:
                thread_key = body_tk
                structlog.contextvars.bind_contextvars(thread_key=thread_key)
        except Exception:
            pass

    if not trace_id and thread_key:
        try:
            trace_id = await get_or_create_thread_trace_id(request.app.state.db_pool, thread_key)
            if trace_id:
                structlog.contextvars.bind_contextvars(trace_id=trace_id)
        except Exception:
            log.debug("thread_trace_lookup_failed", thread_key=thread_key, exc_info=True)

    start = time.perf_counter()
    status_code = 500
    HTTP_REQUESTS_IN_PROGRESS.inc()
    try:
        route = request.scope.get("route")
        path = getattr(route, "path", None) or request.url.path
        with start_span(
            name="centaur.api.http_request",
            span_type="DEFAULT",
            metadata={
                "service": "api",
                "trace_id": trace_id,
                "thread_key": thread_key,
                "http_method": request.method,
                "http_path": path,
            },
            trace_id=trace_id,
        ):
            set_trace_context(
                session_id=trace_id or thread_key,
                metadata={
                    "service": "api",
                    "environment": os.getenv("CENTAUR_ENVIRONMENT", "local"),
                    "trace_id": trace_id,
                    "thread_key": thread_key,
                    "http_path": path,
                },
            )
            response = await call_next(request)
            status_code = response.status_code
            if trace_id:
                response.headers["X-Trace-Id"] = trace_id
                traceparent = traceparent_from_trace_id(trace_id)
                if traceparent:
                    response.headers["traceparent"] = traceparent
            set_span_attributes(
                {
                    "http.method": request.method,
                    "http.route": path,
                    "http.status_code": status_code,
                    **({"centaur.trace_id": trace_id} if trace_id else {}),
                    **({"centaur.thread_key": thread_key} if thread_key else {}),
                }
            )
            return response
    finally:
        HTTP_REQUESTS_IN_PROGRESS.dec()
        route = request.scope.get("route")
        path = getattr(route, "path", None) or request.url.path
        duration_ms = (time.perf_counter() - start) * 1000
        observe_http_request(
            method=request.method,
            path=path,
            status=status_code,
            duration_s=duration_ms / 1000,
        )
        if not path.startswith(("/health", "/metrics")):
            log.info(
                "http_request",
                method=request.method,
                path=path,
                status=status_code,
                duration_ms=round(duration_ms, 2),
                trace_id=trace_id,
                thread_key=thread_key,
                client_ip=request.client.host if request.client else None,
            )
        structlog.contextvars.clear_contextvars()


app.include_router(health.router)
app.include_router(agent_router_mod.router)
app.include_router(workflow_router_mod.router)
app.include_router(attachments_mod.router)
app.include_router(admin.router)
app.include_router(deprecated.router)


# Load tools
# Resolution order: TOOL_DIRS env var (colon-separated) → tools.toml → PLUGINS_DIR fallback
_app_root = Path(__file__).resolve().parent.parent.parent

_tool_dirs_env = os.environ.get("TOOL_DIRS", "")
if _tool_dirs_env:
    _tools_dirs = [Path(d.strip()) for d in _tool_dirs_env.split(":") if d.strip()]
else:
    _plugins_config = _app_root / "tools.toml"
    _plugin_dirs = load_plugins_config(_plugins_config)
    _tools_dirs = (
        _plugin_dirs
        if _plugin_dirs
        else [Path(os.environ.get("PLUGINS_DIR", _app_root / "tools"))]
    )

# Make all TOOL_DIRS entries visible through the `tools.*` namespace with
# overlay-last priority. Some overlay tools subclass or extend base tools;
# without refreshing both sys.path and the already-imported namespace path,
# Python can silently import the base module and drop overlay-only methods.
for _tools_dir in reversed(_tools_dirs):
    _parent = str(_tools_dir.resolve().parent)
    if _parent and _parent not in sys.path:
        sys.path.insert(0, _parent)
try:
    import tools as _tools_pkg

    _seen: set[str] = set()
    _new_path: list[str] = []
    for _tools_dir in reversed(_tools_dirs):
        s = str(_tools_dir.resolve())
        if s and s not in _seen and os.path.isdir(s):
            _seen.add(s)
            _new_path.append(s)
    for _existing in getattr(_tools_pkg, "__path__", []):
        if _existing not in _seen:
            _seen.add(_existing)
            _new_path.append(_existing)
    _tools_pkg.__path__ = _new_path  # type: ignore[assignment]
except ImportError:
    pass

tool_manager = ToolManager(_tools_dirs)
tool_manager.discover()
app.state.tool_manager = tool_manager
app.include_router(tool_manager.create_rest_router())


def get_tool_manager() -> ToolManager:
    return tool_manager
