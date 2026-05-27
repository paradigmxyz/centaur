"""API key management — create, verify, revoke, and scope-check keys."""

from __future__ import annotations

import hashlib
import os
import secrets
import time
from dataclasses import dataclass, field
from threading import Lock

import asyncpg
import structlog

log = structlog.get_logger()

_KEY_BYTES = 32  # 256-bit random keys


@dataclass
class APIKeyInfo:
    """Resolved key metadata returned after verification."""

    id: str
    name: str
    key_prefix: str
    scopes: list[str]
    created_by: str
    source: str = "db"  # "db" | "root" | "sandbox" | "localhost"


@dataclass(frozen=True)
class ServiceAPIKeySpec:
    """Static service key configuration bootstrapped into Postgres on startup."""

    env_var: str
    name: str
    scopes: tuple[str, ...]


_SERVICE_API_KEYS: tuple[ServiceAPIKeySpec, ...] = (
    ServiceAPIKeySpec(
        env_var="SLACKBOT_API_KEY",
        name="service:slackbot",
        scopes=("agent",),
    ),
    ServiceAPIKeySpec(
        env_var="LOCAL_DEV_API_KEY",
        name="service:local-dev",
        scopes=("admin", "agent", "threads", "tools:*"),
    ),
    ServiceAPIKeySpec(
        env_var="MONITORING_OPERATOR_API_KEY",
        name="service:monitoring-ui",
        scopes=("admin",),
    ),
)


def generate_key() -> tuple[str, str, str]:
    """Generate a new API key. Returns (plaintext_key, key_prefix, key_hash)."""
    raw = secrets.token_urlsafe(_KEY_BYTES)
    key = f"aiv2_{raw}"
    prefix = key[:8]
    key_hash = hashlib.sha256(key.encode()).hexdigest()
    return key, prefix, key_hash


def hash_key(key: str) -> str:
    """Hash a plaintext key for comparison."""
    return hashlib.sha256(key.encode()).hexdigest()


def _normalize_scopes(scopes: list[str] | tuple[str, ...]) -> list[str]:
    """Deduplicate scopes while preserving declaration order."""
    seen: set[str] = set()
    normalized: list[str] = []
    for scope in scopes:
        cleaned = scope.strip()
        if not cleaned or cleaned in seen:
            continue
        seen.add(cleaned)
        normalized.append(cleaned)
    return normalized


# ---------------------------------------------------------------------------
# In-memory cache of active keys (refreshed periodically)
# ---------------------------------------------------------------------------


@dataclass
class _KeyCache:
    """Thread-safe cache of active API keys."""

    keys: dict[str, APIKeyInfo] = field(default_factory=dict)  # hash → info
    lock: Lock = field(default_factory=Lock)
    expires_at: float = 0.0


_cache = _KeyCache()
_CACHE_TTL = 30.0  # seconds


async def refresh_cache(pool: asyncpg.Pool) -> int:
    """Reload active keys from Postgres into memory."""
    rows = await pool.fetch(
        "SELECT id, name, key_prefix, key_hash, scopes, created_by "
        "FROM api_keys WHERE revoked_at IS NULL"
    )
    new_keys: dict[str, APIKeyInfo] = {}
    for r in rows:
        info = APIKeyInfo(
            id=str(r["id"]),
            name=r["name"],
            key_prefix=r["key_prefix"],
            scopes=list(r["scopes"]),
            created_by=r["created_by"],
        )
        new_keys[r["key_hash"]] = info
    with _cache.lock:
        _cache.keys = new_keys
        _cache.expires_at = time.monotonic() + _CACHE_TTL
    return len(new_keys)


async def lookup_key(pool: asyncpg.Pool, token: str) -> APIKeyInfo | None:
    """Look up a key by its plaintext value. Uses cache, falls back to DB."""
    h = hash_key(token)

    # Check cache first
    now = time.monotonic()
    with _cache.lock:
        if now < _cache.expires_at:
            info = _cache.keys.get(h)
            if info is not None:
                return info
            # Cache is fresh but key not found — definitely not valid
            return None

    # Cache expired — refresh
    await refresh_cache(pool)
    with _cache.lock:
        return _cache.keys.get(h)


async def create_key(
    pool: asyncpg.Pool,
    name: str,
    scopes: list[str],
    created_by: str = "",
) -> tuple[str, APIKeyInfo]:
    """Create a new API key. Returns (plaintext_key, info)."""
    plaintext, prefix, key_hash = generate_key()
    normalized_scopes = _normalize_scopes(scopes)
    row = await pool.fetchrow(
        "INSERT INTO api_keys (name, key_prefix, key_hash, scopes, created_by) "
        "VALUES ($1, $2, $3, $4, $5) "
        "RETURNING id",
        name,
        prefix,
        key_hash,
        normalized_scopes,
        created_by,
    )
    info = APIKeyInfo(
        id=str(row["id"]),
        name=name,
        key_prefix=prefix,
        scopes=normalized_scopes,
        created_by=created_by,
    )
    # Invalidate cache
    with _cache.lock:
        _cache.expires_at = 0.0
    log.info("api_key_created", name=name, prefix=prefix, scopes=scopes)
    return plaintext, info


async def ensure_static_key(
    pool: asyncpg.Pool,
    plaintext_key: str,
    name: str,
    scopes: list[str] | tuple[str, ...],
    *,
    created_by: str = "service-bootstrap",
) -> APIKeyInfo:
    """Ensure a known plaintext service key exists as an active DB-backed key."""
    token = plaintext_key.strip()
    if not token:
        raise ValueError("plaintext_key must be non-empty")

    prefix = token[:8]
    key_hash = hash_key(token)
    normalized_scopes = _normalize_scopes(scopes)
    row = await pool.fetchrow(
        "INSERT INTO api_keys (name, key_prefix, key_hash, scopes, created_by, revoked_at) "
        "VALUES ($1, $2, $3, $4, $5, NULL) "
        "ON CONFLICT (key_hash) DO UPDATE SET "
        "  name = EXCLUDED.name, "
        "  key_prefix = EXCLUDED.key_prefix, "
        "  scopes = EXCLUDED.scopes, "
        "  created_by = EXCLUDED.created_by, "
        "  revoked_at = NULL "
        "RETURNING id, name, key_prefix, scopes, created_by",
        name,
        prefix,
        key_hash,
        normalized_scopes,
        created_by,
    )
    with _cache.lock:
        _cache.expires_at = 0.0

    info = APIKeyInfo(
        id=str(row["id"]),
        name=row["name"],
        key_prefix=row["key_prefix"],
        scopes=list(row["scopes"]),
        created_by=row["created_by"],
    )
    log.info(
        "api_key_bootstrapped",
        name=info.name,
        prefix=info.key_prefix,
        scopes=info.scopes,
        created_by=info.created_by,
    )
    return info


async def bootstrap_service_api_keys(pool: asyncpg.Pool) -> list[APIKeyInfo]:
    """Seed long-lived service keys from env vars into Postgres."""
    bootstrapped: list[APIKeyInfo] = []
    for spec in _SERVICE_API_KEYS:
        token = os.environ.get(spec.env_var, "").strip()
        if not token:
            continue
        info = await ensure_static_key(
            pool,
            token,
            spec.name,
            spec.scopes,
            created_by="service-bootstrap",
        )
        bootstrapped.append(info)

    if bootstrapped:
        log.info(
            "service_api_keys_bootstrapped",
            count=len(bootstrapped),
            names=[info.name for info in bootstrapped],
        )
    return bootstrapped


async def revoke_key(pool: asyncpg.Pool, key_id: str) -> bool:
    """Revoke a key by ID. Returns True if revoked, False if not found."""
    result = await pool.execute(
        "UPDATE api_keys SET revoked_at = NOW() WHERE id = $1 AND revoked_at IS NULL",
        key_id,
    )
    revoked = result == "UPDATE 1"
    if revoked:
        with _cache.lock:
            _cache.expires_at = 0.0
        log.info("api_key_revoked", key_id=key_id)
    return revoked


async def list_keys(pool: asyncpg.Pool) -> list[dict]:
    """List all keys (active and revoked) — never exposes the hash."""
    rows = await pool.fetch(
        "SELECT id, name, key_prefix, scopes, created_by, created_at, revoked_at "
        "FROM api_keys ORDER BY created_at DESC"
    )
    return [
        {
            "id": str(r["id"]),
            "name": r["name"],
            "key_prefix": r["key_prefix"],
            "scopes": list(r["scopes"]),
            "created_by": r["created_by"],
            "created_at": r["created_at"].isoformat() if r["created_at"] else None,
            "revoked_at": r["revoked_at"].isoformat() if r["revoked_at"] else None,
            "active": r["revoked_at"] is None,
        }
        for r in rows
    ]


# ---------------------------------------------------------------------------
# Scope checking
# ---------------------------------------------------------------------------


def check_scope(key_info: APIKeyInfo, required: str, resource: str = "") -> bool:
    """Check if a key's scopes permit the requested action.

    Scope format: "*" (wildcard), "admin", "agent", "agent:execute",
    "tools:*", "tools:<name>", "workflows", "workflows:*",
    "workflows:<name>", "threads", "threads:read".

    A bare category scope (e.g. "agent") grants all sub-actions. Resource-
    qualified categories (``tools``, ``workflows``) accept either a wildcard
    (``tools:*`` / ``workflows:*``) or an exact resource match
    (``workflows:my_workflow``).
    """
    scopes = key_info.scopes

    if "*" in scopes:
        return True

    if ":" in required and not required.startswith(("tools:", "workflows:")):
        category, action = required.split(":", 1)
    else:
        category = required
        action = ""

    if category == "tools":
        for scope in scopes:
            if scope == "tools:*":
                return True
            if scope.startswith("tools:") and resource == scope[6:]:
                return True
        return False

    if category == "workflows":
        for scope in scopes:
            if scope in ("workflows", "workflows:*"):
                return True
            if scope.startswith("workflows:") and resource == scope[len("workflows:") :]:
                return True
        return False

    for scope in scopes:
        if scope == category:
            return True
        if action and scope == required:
            return True

    return False
