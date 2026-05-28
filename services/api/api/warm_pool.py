"""Pre-warmed sandbox pool — keeps N sandboxes ready for instant claiming.

Eliminates sandbox startup latency (~15s) by maintaining a pool of idle
sandboxes that can be instantly claimed when a new thread arrives.

Fully async — no threads or asyncio.to_thread wrappers.
"""

from __future__ import annotations

import asyncio
import contextlib
import os
import time
from dataclasses import dataclass, field
from pathlib import Path

import structlog

from api.agent import _get_runtime
from api.deps import mint_sandbox_token
from api.sandbox.base import SandboxSession
from api.sandbox.prompt_assembly import assemble_prompt
from api.sandbox.registry import get_backend

log = structlog.get_logger()

# Pool configuration
POOL_SIZE = int(os.getenv("WARM_POOL_SIZE", "5"))
POOL_HARNESS = os.getenv("WARM_POOL_HARNESS", "codex")
POOL_REPLENISH_INTERVAL = float(os.getenv("WARM_POOL_REPLENISH_INTERVAL", "5.0"))
POOL_BACKEND_TIMEOUT = float(os.getenv("WARM_POOL_BACKEND_TIMEOUT", "30.0"))
# Recycle existing warm pods on startup instead of adopting them. After an image
# or overlay bump the API restarts but pre-existing warm pods still run the old
# refs; evicting them here guarantees the first claim post-deploy uses the
# just-deployed code. Set to "0"/"false" to keep the legacy adopt-on-restart
# behavior.
POOL_EVICT_ON_STARTUP = os.getenv("WARM_POOL_EVICT_ON_STARTUP", "1").lower() not in (
    "0",
    "false",
    "no",
)


@dataclass
class WarmContainer:
    """A pre-warmed sandbox not yet bound to any thread."""

    sandbox_id: str
    harness: str
    engine: str
    created_at: float = field(default_factory=time.time)


# Async-safe pool (single-threaded asyncio — no lock needed for non-awaiting ops,
# but we use asyncio.Lock around sections with awaits to prevent interleaving)
_pool_lock = asyncio.Lock()
_pool: list[WarmContainer] = []
_replenish_task: asyncio.Task | None = None


def _repo_root() -> Path:
    for candidate in Path(__file__).resolve().parents:
        if (candidate / "services" / "sandbox" / "SYSTEM_PROMPT.md").is_file():
            return candidate
    raise FileNotFoundError("could not locate services/sandbox/SYSTEM_PROMPT.md")


def _overlay_root() -> Path | None:
    value = (os.getenv("CENTAUR_OVERLAY_DIR") or "").strip()
    if not value:
        return None
    path = Path(value)
    return path if path.exists() else None


def pool_status() -> dict:
    """Return pool diagnostics (sync-safe: no awaits, reads only)."""
    containers = [
        {"sandbox_id": w.sandbox_id[:12], "age_s": round(time.time() - w.created_at, 1)}
        for w in _pool
    ]
    return {
        "target_size": POOL_SIZE,
        "current_size": len(containers),
        "harness": POOL_HARNESS,
        "containers": containers,
    }


async def _spawn_warm_container() -> WarmContainer | None:
    """Create one warm sandbox. Returns None on failure."""
    backend = get_backend()
    if not backend.supports_warm_pool:
        return None
    engine = (
        POOL_HARNESS
        if POOL_HARNESS in {"amp", "claude-code", "codex", "openrouter"}
        else "codex"
    )

    placeholder_key = f"warm-{int(time.time() * 1000)}-{id(asyncio.current_task())}"
    try:
        session = await asyncio.wait_for(
            backend.create(placeholder_key, POOL_HARNESS, engine, warm=True),
            timeout=POOL_BACKEND_TIMEOUT,
        )
        warm = WarmContainer(
            sandbox_id=session.sandbox_id,
            harness=POOL_HARNESS,
            engine=engine,
        )
        log.info("warm_container_created", sandbox=session.sandbox_id[:12])
        return warm
    except Exception as exc:
        log.warning("warm_container_spawn_failed", error=str(exc))
        return None


async def replenish() -> int:
    """Spawn sandboxes until the pool reaches target size. Returns count spawned."""
    # Health-check existing pool entries
    backend = get_backend()
    async with _pool_lock:
        alive = []
        for warm in _pool:
            try:
                st = await asyncio.wait_for(
                    backend.status_by_id(warm.sandbox_id),
                    timeout=POOL_BACKEND_TIMEOUT,
                )
                if st == "running" and (time.time() - warm.created_at) < 3600:
                    alive.append(warm)
                else:
                    log.info(
                        "warm_pool_evicted",
                        sandbox=warm.sandbox_id[:12],
                        status=st,
                        age_s=round(time.time() - warm.created_at),
                    )
                    with contextlib.suppress(Exception):
                        await asyncio.wait_for(
                            backend.stop_by_id(warm.sandbox_id),
                            timeout=POOL_BACKEND_TIMEOUT,
                        )
            except Exception:
                log.info("warm_pool_evicted_error", sandbox=warm.sandbox_id[:12])
        if len(alive) != len(_pool):
            _pool[:] = alive

    spawned = 0
    while True:
        async with _pool_lock:
            deficit = POOL_SIZE - len(_pool)
        if deficit <= 0:
            break
        warm = await _spawn_warm_container()
        if warm is None:
            break
        async with _pool_lock:
            _pool.append(warm)
        spawned += 1
    return spawned


async def _inject_persona(
    sandbox_id: str,
    persona: str | None,
    repo: str | None,
) -> None:
    """Inject persona/repo config into a running warm container via exec."""
    backend = get_backend()

    # 1. Clone repo into workspace
    if repo:
        await backend.exec_run(
            sandbox_id,
            [
                "sh",
                "-c",
                f"""
REPO_PATH="/home/agent/github/{repo}"
WORKSPACE="/home/agent/workspace"
if git -C "$REPO_PATH" rev-parse --git-dir >/dev/null 2>&1; then
    rm -rf "$WORKSPACE"
    git clone --quiet --shared "$REPO_PATH" "$WORKSPACE" || git clone --quiet "$REPO_PATH" "$WORKSPACE"
    BRANCH="agent-$(date +%s)-$RANDOM-$RANDOM"
    git -C "$WORKSPACE" checkout -q -b "$BRANCH" || true
fi
""",
            ],
            user="agent",
        )
        # Copy project skills
        await backend.exec_run(
            sandbox_id,
            [
                "sh",
                "-c",
                """
MOUNTED_CENTAUR_SKILLS="/home/agent/centaur-skills"
MOUNTED_ORG_SKILLS="/home/agent/centaur-overlay-skills"
CENTAUR_SKILLS=""
if [ -d /home/agent/github ]; then
    CENTAUR_SKILLS="$(find /home/agent/github -path '*/centaur/.agents/skills' -type d -print -quit 2>/dev/null || true)"
fi
WS_SKILLS="/home/agent/workspace/.agents/skills"
for SKILLS_SRC in "$MOUNTED_CENTAUR_SKILLS" "$CENTAUR_SKILLS" "$MOUNTED_ORG_SKILLS"; do
    if [ -d "$SKILLS_SRC" ]; then
        mkdir -p "$WS_SKILLS"
        cp -r "$SKILLS_SRC"/. "$WS_SKILLS"/
    fi
done
""",
            ],
            user="agent",
        )
        # Re-copy base system prompt
        await backend.exec_run(
            sandbox_id,
            [
                "sh",
                "-c",
                """
if [ -f /home/agent/AGENTS_BASE.md ]; then
    cp /home/agent/AGENTS_BASE.md /home/agent/workspace/AGENTS.md
elif [ -f /home/agent/AGENTS.md ]; then
    cp /home/agent/AGENTS.md /home/agent/workspace/AGENTS.md
fi
if [ -f /home/agent/AGENTS_OVERLAY.md ] && [ -f /home/agent/workspace/AGENTS.md ]; then
    printf '\n\n---\n\n' >> /home/agent/workspace/AGENTS.md
    cat /home/agent/AGENTS_OVERLAY.md >> /home/agent/workspace/AGENTS.md
fi
""",
            ],
            user="agent",
        )

    # 2. Rebuild the effective prompt from the same helper used for cold spawns.
    persona_info = None
    if persona:
        from api.app import get_tool_manager

        persona_info = get_tool_manager().get_persona(persona)
    overlay_root = _overlay_root()
    overlay_prompt = (
        overlay_root / "services" / "sandbox" / "SYSTEM_PROMPT.md"
        if overlay_root is not None
        else None
    )
    prompt_content = assemble_prompt(
        persona,
        base_prompt=(
            _repo_root() / "services" / "sandbox" / "SYSTEM_PROMPT.md"
        ).read_text(),
        overlay_prompt_path=overlay_prompt,
        persona_info=persona_info,
        api_overlay_dir=overlay_root,
        sandbox_overlay_dir="/home/agent/overlay/org"
        if (os.getenv("CENTAUR_OVERLAY_IMAGE") or "").strip()
        else None,
    )
    await backend.exec_run(
        sandbox_id,
        [
            "sh",
            "-c",
            (
                'mkdir -p /home/agent/workspace && printf "%s" "$_CONTENT" > /home/agent/workspace/AGENTS.md && '
                "if [ -f /home/agent/AGENTS_OVERLAY.md ]; then "
                'printf "\\n\\n---\\n\\n" >> /home/agent/workspace/AGENTS.md && '
                "cat /home/agent/AGENTS_OVERLAY.md >> /home/agent/workspace/AGENTS.md; "
                "fi"
            ),
        ],
        environment={"_CONTENT": prompt_content},
        user="agent",
    )

    # 3. Write env overrides
    env_lines: list[str] = []
    if persona:
        env_lines.append(f"export AGENT_PERSONA={persona}")
    if repo:
        env_lines.append(f"export AGENT_REPO={repo}")
    if env_lines:
        env_content = "\n".join(env_lines) + "\n"
        await backend.exec_run(
            sandbox_id,
            ["sh", "-c", 'printf "%s" "$_CONTENT" > /home/agent/.env_override'],
            environment={"_CONTENT": env_content},
            user="agent",
        )

    log.info(
        "warm_container_persona_injected",
        sandbox=sandbox_id[:12],
        persona=persona,
        repo=repo,
    )


async def claim_container(
    thread_key: str,
    harness: str = "codex",
    *,
    persona: str | None = None,
    repo: str | None = None,
    trace_id: str | None = None,
) -> SandboxSession | None:
    """Try to claim a warm sandbox from the pool. Returns SandboxSession or None."""
    if harness != POOL_HARNESS:
        return None
    backend = get_backend()
    if not backend.supports_warm_pool:
        return None
    if backend.name == "kubernetes" and (persona or repo):
        return None

    warm: WarmContainer | None = None
    async with _pool_lock:
        if _pool:
            warm = _pool.pop(0)

    if warm is None:
        return None
    try:
        st = await asyncio.wait_for(
            backend.status_by_id(warm.sandbox_id),
            timeout=POOL_BACKEND_TIMEOUT,
        )
    except (asyncio.TimeoutError, Exception):
        log.warning("warm_container_dead_on_claim", sandbox=warm.sandbox_id[:12])
        with contextlib.suppress(Exception):
            await asyncio.wait_for(
                backend.stop_by_id(warm.sandbox_id),
                timeout=POOL_BACKEND_TIMEOUT,
            )
        return None
    if st != "running":
        log.warning("warm_container_dead_on_claim", sandbox=warm.sandbox_id[:12])
        with contextlib.suppress(Exception):
            await asyncio.wait_for(
                backend.stop_by_id(warm.sandbox_id),
                timeout=POOL_BACKEND_TIMEOUT,
            )
        return None

    try:
        fresh_token = mint_sandbox_token(thread_key, warm.sandbox_id)
        await backend.refresh_token_by_id(warm.sandbox_id, fresh_token)
    except Exception:
        log.warning("warm_claim_token_refresh_failed", sandbox=warm.sandbox_id[:12])

    if trace_id:
        try:
            await backend.exec_run(
                warm.sandbox_id,
                ["sh", "-c", 'printf "%s" "$TRACE_ID" > /home/agent/.trace_id'],
                environment={"TRACE_ID": trace_id},
                user="agent",
            )
        except Exception:
            log.warning("warm_claim_trace_refresh_failed", sandbox=warm.sandbox_id[:12])

    if persona or repo:
        await _inject_persona(warm.sandbox_id, persona, repo)

    session = SandboxSession(
        sandbox_id=warm.sandbox_id,
        thread_key=thread_key,
        harness=harness,
        engine=warm.engine,
        started_at=time.time(),
        trace_id=trace_id or "",
    )
    _get_runtime(session.sandbox_id)

    log.info(
        "warm_container_claimed",
        thread_key=thread_key,
        sandbox=warm.sandbox_id[:12],
        pool_age_s=round(time.time() - warm.created_at, 1),
        persona=persona,
        repo=repo,
    )
    return session


async def cleanup_pool() -> int:
    """Stop and remove all warm sandboxes. Returns count cleaned."""
    async with _pool_lock:
        to_clean = list(_pool)
        _pool.clear()
    cleaned = 0
    backend = get_backend()
    for warm in to_clean:
        with contextlib.suppress(Exception):
            await asyncio.wait_for(
                backend.stop_by_id(warm.sandbox_id),
                timeout=POOL_BACKEND_TIMEOUT,
            )
            cleaned += 1
    return cleaned


async def _recover_warm(assigned_sandbox_ids: set[str] | None = None) -> int:
    """Recover existing warm sandboxes from backend on API restart."""
    assigned = assigned_sandbox_ids or set()
    backend = get_backend()
    recovered = 0
    sessions = await asyncio.wait_for(
        backend.recover_warm(POOL_HARNESS),
        timeout=POOL_BACKEND_TIMEOUT * 2,
    )
    async with _pool_lock:
        for session in sessions:
            if len(_pool) >= POOL_SIZE:
                break
            if session.sandbox_id in assigned:
                log.info(
                    "warm_recover_skipped_assigned",
                    sandbox=session.sandbox_id[:12],
                )
                continue
            _pool.append(
                WarmContainer(
                    sandbox_id=session.sandbox_id,
                    harness=session.harness,
                    engine=session.engine,
                )
            )
            recovered += 1
    return recovered


async def _evict_existing_warm(assigned_sandbox_ids: set[str] | None = None) -> int:
    """Stop every pre-existing unassigned warm sandbox on startup.

    Pool members are pre-built with the previous deploy's image + overlay
    refs. Adopting them after an image bump leaves stale sandboxes in
    rotation; recreating them with the current refs ensures the first
    claim after a deploy uses the just-deployed code paths. Assigned
    sandboxes (still serving live threads) are left alone.
    """
    assigned = assigned_sandbox_ids or set()
    backend = get_backend()
    evicted = 0
    try:
        sessions = await asyncio.wait_for(
            backend.recover_warm(POOL_HARNESS),
            timeout=POOL_BACKEND_TIMEOUT * 2,
        )
    except Exception as exc:
        log.warning("warm_pool_evict_list_failed", error=str(exc))
        return 0
    for session in sessions:
        if session.sandbox_id in assigned:
            continue
        try:
            await asyncio.wait_for(
                backend.stop_by_id(session.sandbox_id),
                timeout=POOL_BACKEND_TIMEOUT,
            )
            evicted += 1
        except Exception as exc:
            log.warning(
                "warm_pool_evict_stop_failed",
                sandbox=session.sandbox_id[:12],
                error=str(exc),
            )
    return evicted


async def _get_assigned_sandbox_ids() -> set[str]:
    """Return sandbox IDs that are already assigned to threads in the DB."""
    from api.agent import _get_pool

    pool = _get_pool()
    rows = await pool.fetch(
        "SELECT sandbox_id FROM sandbox_sessions WHERE state IN ('running', 'idle', 'error')"
    )
    return {row["sandbox_id"] for row in rows}


async def start_replenish_loop() -> asyncio.Task | None:
    """Start a background task that keeps the pool at target size."""
    global _replenish_task
    backend = get_backend()
    if not backend.supports_warm_pool:
        log.info("warm_pool_disabled_for_backend", backend=backend.name)
        return None

    async def _loop() -> None:
        assigned = await _get_assigned_sandbox_ids()
        if POOL_EVICT_ON_STARTUP:
            evicted = await _evict_existing_warm(assigned)
            if evicted:
                log.info("warm_pool_startup_evicted", evicted=evicted)
        else:
            recovered = await _recover_warm(assigned)
            if recovered:
                log.info("warm_pool_recovered", recovered=recovered)
        count = await replenish()
        if count:
            log.info("warm_pool_initial_fill", spawned=count, target=POOL_SIZE)
        while True:
            await asyncio.sleep(POOL_REPLENISH_INTERVAL)
            try:
                await replenish()
            except Exception as exc:
                log.warning("warm_pool_replenish_error", error=str(exc))

    _replenish_task = asyncio.create_task(_loop())
    return _replenish_task


async def stop_replenish_loop() -> None:
    """Cancel replenish loop."""
    global _replenish_task
    if _replenish_task:
        _replenish_task.cancel()
        with contextlib.suppress(asyncio.CancelledError):
            await _replenish_task
        _replenish_task = None
    log.info("warm_pool_loop_stopped", pool_size=len(_pool))
