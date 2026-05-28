from __future__ import annotations

import base64
import hashlib
import hmac
import json
import os
import secrets
import time
from collections.abc import Callable
from typing import Annotated

import structlog
import structlog.contextvars
from fastapi import Header, HTTPException, Request

from api.api_keys import APIKeyInfo, check_scope, lookup_key

log = structlog.get_logger()

# Every caller must present a valid API key. There is no IP-based trust:
# the previous loopback bypass was spoofable via X-Forwarded-For and gave
# any peer that could reach the API full unauthenticated admin access.
# Unauthenticated health/readiness probes use the no-auth /health* routes.


def _get_sandbox_signing_key() -> str:
    """Return the key used for HMAC-signing sandbox tokens.

    Checks SANDBOX_SIGNING_KEY first, then falls back to API_SECRET_KEY for
    backwards compatibility. If neither is set, auto-generates a random key
    that persists for the lifetime of this process.
    """
    key = os.environ.get("SANDBOX_SIGNING_KEY") or os.environ.get("API_SECRET_KEY") or ""
    if not key:
        key = _auto_signing_key
    return key


# Auto-generated signing key — used when no explicit key is configured.
# Stable for the lifetime of the process so sandbox tokens stay valid.
_auto_signing_key: str = secrets.token_hex(32)


# ---------------------------------------------------------------------------
# Scoped sandbox tokens (HMAC-SHA256, sbx1.* format)
# ---------------------------------------------------------------------------


def mint_sandbox_token(thread_key: str, container_id: str, ttl_s: int = 7200) -> str:
    """Create a short-lived sandbox token signed with the sandbox signing key."""
    api_key = _get_sandbox_signing_key()
    if not api_key:
        raise RuntimeError("Sandbox signing key not configured")

    now = int(time.time())
    payload = {
        "thread_key": thread_key,
        "container_id": container_id,
        "created_at": now,
        "expires_at": now + ttl_s,
    }
    payload_b64 = base64.urlsafe_b64encode(json.dumps(payload).encode()).decode()
    sig = hmac.new(api_key.encode(), payload_b64.encode(), hashlib.sha256).digest()
    sig_b64 = base64.urlsafe_b64encode(sig).decode()
    return f"sbx1.{payload_b64}.{sig_b64}"


def verify_sandbox_token(token: str) -> dict | None:
    """Validate signature and expiry of a sandbox token. Returns claims or None."""
    api_key = _get_sandbox_signing_key()
    if not api_key:
        return None

    parts = token.split(".")
    if len(parts) != 3 or parts[0] != "sbx1":
        return None

    payload_b64 = parts[1]
    sig_b64 = parts[2]

    expected_sig = hmac.new(api_key.encode(), payload_b64.encode(), hashlib.sha256).digest()
    try:
        provided_sig = base64.urlsafe_b64decode(sig_b64)
    except Exception:
        return None

    if not hmac.compare_digest(expected_sig, provided_sig):
        return None

    try:
        payload = json.loads(base64.urlsafe_b64decode(payload_b64))
    except Exception:
        return None

    if time.time() > payload.get("expires_at", 0):
        return None

    return payload


async def verify_api_key(
    request: Request,
    x_api_key: Annotated[str | None, Header()] = None,
) -> str:
    client_ip = request.client.host if request.client else ""
    token = x_api_key
    if not token:
        auth = request.headers.get("authorization", "")
        if auth.lower().startswith("bearer "):
            token = auth[7:]

    if not token:
        raise HTTPException(status_code=401, detail="Missing API key")

    # Scoped sandbox tokens (sbx1.* format) — auto-issued by the API
    if token.startswith("sbx1."):
        claims = verify_sandbox_token(token)
        if claims is not None:
            request.state.sandbox_claims = claims
            structlog.contextvars.bind_contextvars(
                thread_key=claims.get("thread_key"),
                sandbox_container_id=claims.get("container_id"),
            )
            request.state.api_key_info = APIKeyInfo(
                id=claims["container_id"],
                name="sandbox",
                key_prefix="sbx1",
                scopes=["agent", "tools:*"],
                created_by="system",
                source="sandbox",
            )
            return f"sandbox:{claims['container_id']}"
        log.warning(
            "sbx_token_rejected",
            token_prefix=token[:20] if token else "",
            reason="invalid_signature_or_expired",
            client_ip=client_ip,
            path=str(request.url.path),
        )
        raise HTTPException(status_code=401, detail="Invalid or expired sandbox token")

    # DB key lookup — all external callers use DB-backed aiv2_* keys. The
    # tool-server sidecar runs without a DB pool (sandbox tokens only), so a
    # non-sandbox key reaching it is simply unauthenticated rather than a crash.
    pool = request.app.state.db_pool
    if pool is None:
        raise HTTPException(status_code=401, detail="Invalid API key")
    key_info = await lookup_key(pool, token)
    if key_info is not None:
        request.state.api_key_info = key_info
        return f"key:{key_info.key_prefix}"

    raise HTTPException(status_code=401, detail="Invalid API key")


def get_key_info(request: Request) -> APIKeyInfo:
    """Retrieve the APIKeyInfo attached during verify_api_key."""
    info = getattr(request.state, "api_key_info", None)
    if info is None:
        # No key was resolved (verify_api_key did not run, or ran without a
        # valid key). Fail closed — grant no scopes.
        return APIKeyInfo(
            id="unknown",
            name="unknown",
            key_prefix="",
            scopes=[],
            created_by="system",
            source="unknown",
        )
    return info


def get_sandbox_claims(request: Request) -> dict[str, str] | None:
    claims = getattr(request.state, "sandbox_claims", None)
    return claims if isinstance(claims, dict) else None


def sandbox_thread_in_scope(allowed_thread_key: str | None, requested_thread_key: str) -> bool:
    """Return whether a sandbox token may access the requested thread."""
    if not allowed_thread_key:
        return True
    if allowed_thread_key == requested_thread_key:
        return True
    return False


def require_scope(scope: str) -> Callable:
    """Return a FastAPI dependency that checks the caller has the given scope.

    Usage::

        @router.post("/execute", dependencies=[Depends(require_scope("agent:execute"))])
        async def execute(...): ...
    """

    async def _check(request: Request) -> None:
        key_info = get_key_info(request)
        if not check_scope(key_info, scope):
            raise HTTPException(
                status_code=403,
                detail=f"API key scope does not permit '{scope}'",
            )

    return _check


async def verify_operator_api_key(
    request: Request,
    x_api_key: Annotated[str | None, Header()] = None,
) -> str:
    token = await verify_api_key(request, x_api_key)
    key_info = get_key_info(request)
    if check_scope(key_info, "admin"):
        return token
    raise HTTPException(status_code=403, detail="Operator route requires admin scope")
