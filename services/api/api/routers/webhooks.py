"""Public workflow webhook routes."""

from __future__ import annotations

import base64
import hashlib
import hmac
import json
import os
from typing import Any
from urllib.parse import parse_qs

import structlog
from fastapi import APIRouter, HTTPException, Request
from fastapi.responses import JSONResponse

from api.runtime_control import ControlPlaneError
from api.webhooks import HeaderTriggerKey, HmacAuth, WebhookSpec, get_webhook_spec
from api.workflow_engine import create_workflow_run

log = structlog.get_logger().bind(service="api", component="workflow_webhooks")

router = APIRouter(prefix="/api/webhooks", tags=["webhooks"])

_REDACTED_HEADERS = {
    "authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "x-centaur-api-key",
    "x-hub-signature",
    "x-hub-signature-256",
    "x-slack-signature",
    "stripe-signature",
}
_MAX_WEBHOOK_BODY_BYTES = 1024 * 1024


def _safe_headers(request: Request) -> dict[str, str]:
    headers: dict[str, str] = {}
    for key, value in request.headers.items():
        normalized = key.lower()
        if normalized in _REDACTED_HEADERS:
            continue
        headers[normalized] = value
    return headers


def _safe_headers_for_spec(request: Request, spec: WebhookSpec) -> dict[str, str]:
    headers = _safe_headers(request)
    if isinstance(spec.auth, HmacAuth):
        headers.pop(spec.auth.signature_header.lower(), None)
    return headers


def _content_type(request: Request) -> str:
    return request.headers.get("content-type", "").split(";", 1)[0].strip().lower()


def _source_ip(request: Request) -> str | None:
    forwarded = request.headers.get("x-forwarded-for")
    if forwarded:
        first = forwarded.split(",", 1)[0].strip()
        if first:
            return first
    return request.client.host if request.client else None


async def _parse_body(request: Request, raw_body: bytes) -> Any:
    if not raw_body:
        return {}
    content_type = _content_type(request)
    if content_type == "application/json":
        try:
            return json.loads(raw_body)
        except Exception as exc:
            raise HTTPException(status_code=400, detail="invalid JSON webhook body") from exc
    if content_type == "application/x-www-form-urlencoded":
        parsed = parse_qs(raw_body.decode("utf-8", errors="replace"), keep_blank_values=True)
        form = {
            key: values[0] if len(values) == 1 else values
            for key, values in parsed.items()
        }
        payload = form.get("payload")
        if isinstance(payload, str):
            try:
                return json.loads(payload)
            except json.JSONDecodeError:
                return form
        return form
    return raw_body.decode("utf-8", errors="replace")


def _extract_trigger_key(
    *,
    slug: str,
    raw_body_sha256: str,
    trigger_key_spec: HeaderTriggerKey | str | None,
    request: Request,
) -> str:
    if isinstance(trigger_key_spec, HeaderTriggerKey):
        header_value = request.headers.get(trigger_key_spec.header)
        if header_value and header_value.strip():
            return f"webhook:{slug}:{trigger_key_spec.header.lower()}:{header_value.strip()}"
    if isinstance(trigger_key_spec, str) and trigger_key_spec.strip():
        return f"webhook:{slug}:{trigger_key_spec.strip()}"
    return f"webhook:{slug}:{raw_body_sha256}"


def _expected_hmac_signature(auth: HmacAuth, secret: str, raw_body: bytes) -> str:
    digest = hmac.new(secret.encode("utf-8"), raw_body, hashlib.sha256).digest()
    if auth.encoding == "hex":
        encoded = digest.hex()
    else:
        encoded = base64.b64encode(digest).decode("ascii")
    return f"{auth.signature_prefix}{encoded}"


def _verify_webhook_auth(spec: WebhookSpec, request: Request, raw_body: bytes) -> None:
    if spec.auth == "none":
        return
    if not isinstance(spec.auth, HmacAuth):
        raise HTTPException(status_code=500, detail="unsupported webhook auth configuration")

    signature = request.headers.get(spec.auth.signature_header)
    if not signature:
        log.warning(
            "workflow_webhook_auth_failed",
            slug=spec.slug,
            reason="missing_signature",
            signature_header=spec.auth.signature_header,
        )
        raise HTTPException(status_code=401, detail="missing webhook signature")

    secret = os.getenv(spec.auth.secret_ref)
    if not secret:
        log.error(
            "workflow_webhook_auth_config_error",
            slug=spec.slug,
            secret_ref=spec.auth.secret_ref,
        )
        raise HTTPException(status_code=500, detail="webhook auth secret is not configured")

    expected = _expected_hmac_signature(spec.auth, secret, raw_body)
    if not hmac.compare_digest(signature.strip(), expected):
        log.warning(
            "workflow_webhook_auth_failed",
            slug=spec.slug,
            reason="invalid_signature",
            signature_header=spec.auth.signature_header,
        )
        raise HTTPException(status_code=401, detail="invalid webhook signature")


@router.api_route("/{slug}", methods=["GET", "POST", "PUT", "PATCH", "DELETE"])
async def invoke_workflow_webhook(slug: str, request: Request):
    registered = get_webhook_spec(slug)
    if registered is None:
        raise HTTPException(status_code=404, detail="webhook not found")

    spec = registered.spec
    method = request.method.upper()
    if method not in spec.allowed_methods:
        raise HTTPException(status_code=405, detail="method not allowed for webhook")

    content_type = _content_type(request)
    if content_type and content_type not in spec.allowed_content_types:
        raise HTTPException(status_code=400, detail="unsupported webhook content type")

    raw_body = await request.body()
    if len(raw_body) > _MAX_WEBHOOK_BODY_BYTES:
        raise HTTPException(status_code=413, detail="webhook payload too large")
    _verify_webhook_auth(spec, request, raw_body)

    raw_body_sha256 = hashlib.sha256(raw_body).hexdigest()
    body = await _parse_body(request, raw_body)
    trigger_key = _extract_trigger_key(
        slug=slug,
        raw_body_sha256=raw_body_sha256,
        trigger_key_spec=spec.trigger_key,
        request=request,
    )
    run_input = {
        "webhook": {
            "slug": spec.slug,
            "provider": spec.provider,
            "method": method,
            "path": request.url.path,
            "headers": _safe_headers_for_spec(request, spec),
            "query": dict(request.query_params),
            "body": body,
            "raw_body_sha256": raw_body_sha256,
            "source_ip": _source_ip(request),
        },
    }

    log.info(
        "workflow_webhook_received",
        slug=slug,
        workflow_name=registered.workflow_name,
        method=method,
        raw_body_sha256=raw_body_sha256,
        trigger_key=trigger_key,
    )
    try:
        result = await create_workflow_run(
            request.app.state.db_pool,
            workflow_name=registered.workflow_name,
            run_input=run_input,
            trigger_key=trigger_key,
            eager_start=False,
        )
    except ControlPlaneError as exc:
        raise HTTPException(
            status_code=exc.status_code,
            detail={"code": exc.code, "message": exc.message},
        ) from exc

    status_code = 200 if result.get("idempotent") else 202
    return JSONResponse(
        status_code=status_code,
        content={
            "ok": True,
            "run_id": result["run_id"],
            "workflow_name": result["workflow_name"],
            "status": result["status"],
            "idempotent": result.get("idempotent", False),
        },
    )
