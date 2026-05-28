"""Tool-server entrypoint — the ``/tools/*`` surface, served as a separate process.

Same image as the API (``services/api``); a different uvicorn target. The
sandbox-side sidecar runs ``uvicorn api.tool_server_app:app`` on loopback
inside each sandbox Pod; the API's own image is also reused for the
shared tool-server Deployment if one is configured.

Reuses ``api.tool_manager`` in process (the API and tool-server share the
package); only the FastAPI mount differs.
"""

from __future__ import annotations

import asyncio
import os
import sys
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager, suppress
from pathlib import Path

import structlog
from fastapi import FastAPI

from api.config import settings
from api.db import close_pool, create_pool
from api.logging_config import configure_structlog
from api.tool_manager import ToolManager, load_plugins_config

configure_structlog()
log = structlog.get_logger().bind(service="tool-server")


def _plugin_watcher_enabled() -> bool:
    return os.getenv("PLUGIN_WATCHER_ENABLED", "0").strip().lower() not in {
        "0",
        "false",
        "no",
    }


def _skip_db() -> bool:
    """Run without a Postgres pool (sandbox-sidecar mode).

    As a per-sandbox sidecar the tool-server only ever authenticates HMAC
    sandbox tokens, which need no DB, and the sandbox NetworkPolicy denies it
    Postgres egress anyway. The shared tool-server Deployment leaves this unset
    and keeps its pool for aiv2_* key auth.
    """
    return (os.getenv("TOOL_SERVER_SKIP_DB") or "0").strip().lower() not in {
        "0",
        "false",
        "no",
    }


async def _watch_tools(pm: ToolManager) -> None:
    if not _plugin_watcher_enabled():
        log.info("tool_watcher_disabled")
        return
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


def _resolve_tool_dirs() -> list[Path]:
    """Resolution order matches ``api.app``: TOOL_DIRS → tools.toml → PLUGINS_DIR."""
    app_root = Path(__file__).resolve().parent.parent.parent.parent

    tool_dirs_env = os.environ.get("TOOL_DIRS", "")
    if tool_dirs_env:
        return [Path(d.strip()) for d in tool_dirs_env.split(":") if d.strip()]

    plugins_config = app_root / "tools.toml"
    plugin_dirs = load_plugins_config(plugins_config)
    if plugin_dirs:
        return plugin_dirs

    return [Path(os.environ.get("PLUGINS_DIR", app_root / "tools"))]


@asynccontextmanager
async def lifespan(app: FastAPI) -> AsyncIterator[None]:
    if _skip_db():
        app.state.db_pool = None
        log.info("tool_server_db_skipped")
    else:
        app.state.db_pool = await create_pool(settings.database_url)
    watcher_task = asyncio.create_task(_watch_tools(app.state.tool_manager))
    try:
        yield
    finally:
        watcher_task.cancel()
        with suppress(asyncio.CancelledError):
            await watcher_task
        if app.state.db_pool is not None:
            await close_pool(app.state.db_pool)


app = FastAPI(
    title="Centaur tool-server",
    version="0.1.0",
    lifespan=lifespan,
    redirect_slashes=False,
)


@app.get("/healthz")
async def healthz() -> dict:
    return {"status": "ok"}


# ── Tool discovery (mirrors api.app:382-427) ─────────────────────────────────
_tools_dirs = _resolve_tool_dirs()
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
