"""Workflow webhook registration primitives."""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from typing import Any

import structlog

log = structlog.get_logger().bind(service="api", component="workflow_webhooks")

_SLUG_RE = re.compile(r"^[a-z0-9][a-z0-9._-]{0,127}$")
_RESERVED_SLUGS = {"slack"}


@dataclass(frozen=True)
class HeaderTriggerKey:
    header: str


@dataclass(frozen=True)
class HmacAuth:
    secret_ref: str
    signature_header: str = "X-Webhook-Signature"
    algorithm: str = "sha256"
    signature_prefix: str = "sha256="
    encoding: str = "hex"

    @classmethod
    def github(cls, *, secret_ref: str) -> HmacAuth:
        return cls(
            secret_ref=secret_ref,
            signature_header="X-Hub-Signature-256",
            signature_prefix="sha256=",
        )


@dataclass(frozen=True)
class WebhookSpec:
    slug: str
    auth: str | HmacAuth = "none"
    provider: str | None = None
    trigger_key: HeaderTriggerKey | str | None = None
    allowed_methods: list[str] = field(default_factory=lambda: ["POST"])
    allowed_content_types: list[str] = field(default_factory=lambda: ["application/json"])


@dataclass(frozen=True)
class RegisteredWebhook:
    spec: WebhookSpec
    workflow_name: str
    source_path: str


_WEBHOOKS_BY_SLUG: dict[str, RegisteredWebhook] = {}


def clear_webhook_specs() -> None:
    _WEBHOOKS_BY_SLUG.clear()


def list_webhook_specs() -> dict[str, RegisteredWebhook]:
    return dict(_WEBHOOKS_BY_SLUG)


def get_webhook_spec(slug: str) -> RegisteredWebhook | None:
    return _WEBHOOKS_BY_SLUG.get(slug)


def _coerce_trigger_key(value: Any) -> HeaderTriggerKey | str | None:
    if value is None or isinstance(value, HeaderTriggerKey):
        return value
    if isinstance(value, str):
        return value
    if isinstance(value, dict):
        kind = value.get("type", "header")
        if kind == "header" and isinstance(value.get("header"), str):
            return HeaderTriggerKey(value["header"])
    raise ValueError("trigger_key must be a string, HeaderTriggerKey, or header trigger dict")


def _coerce_auth(value: Any) -> str | HmacAuth:
    if value is None:
        return "none"
    if isinstance(value, HmacAuth):
        return value
    if isinstance(value, str):
        return value
    if isinstance(value, dict):
        kind = value.get("type", value.get("kind", "hmac"))
        if kind == "none":
            return "none"
        if kind == "github":
            secret_ref = value.get("secret_ref")
            if not isinstance(secret_ref, str):
                raise ValueError("github auth requires secret_ref")
            return HmacAuth.github(secret_ref=secret_ref)
        if kind == "hmac":
            data = {k: v for k, v in value.items() if k not in {"type", "kind"}}
            return HmacAuth(**data)
    raise ValueError("auth must be 'none', HmacAuth, or an auth dict")


def _coerce_spec(raw: Any) -> WebhookSpec:
    if isinstance(raw, WebhookSpec):
        return raw
    if isinstance(raw, str):
        return WebhookSpec(slug=raw)
    if isinstance(raw, dict):
        data = dict(raw)
        data["trigger_key"] = _coerce_trigger_key(data.get("trigger_key"))
        data["auth"] = _coerce_auth(data.get("auth"))
        return WebhookSpec(**data)
    raise ValueError("webhook spec must be a WebhookSpec, dict, or slug string")


def _validate_spec(spec: WebhookSpec) -> None:
    if not _SLUG_RE.match(spec.slug):
        raise ValueError(
            "webhook slug must be URL-safe lowercase text matching "
            "[a-z0-9][a-z0-9._-]{0,127}",
        )
    if spec.slug in _RESERVED_SLUGS:
        raise ValueError(f"webhook slug is reserved: {spec.slug}")
    if isinstance(spec.auth, str):
        if spec.auth != "none":
            raise ValueError("unsupported webhook auth string")
    elif isinstance(spec.auth, HmacAuth):
        if spec.auth.algorithm != "sha256":
            raise ValueError("only sha256 HMAC webhook auth is supported for now")
        if spec.auth.encoding not in {"hex", "base64"}:
            raise ValueError("HMAC encoding must be hex or base64")
        if not spec.auth.secret_ref:
            raise ValueError("HMAC auth requires secret_ref")
        if not spec.auth.signature_header:
            raise ValueError("HMAC auth requires signature_header")
    else:
        raise ValueError("unsupported webhook auth")
    if not spec.allowed_methods:
        raise ValueError("allowed_methods must not be empty")
    normalized_methods = [method.upper() for method in spec.allowed_methods]
    if any(not method.isalpha() for method in normalized_methods):
        raise ValueError("allowed_methods must contain HTTP method names")
    if not spec.allowed_content_types:
        raise ValueError("allowed_content_types must not be empty")


def register_workflow_webhooks(
    workflow_name: str,
    source_path: str,
    raw_specs: Any,
) -> None:
    if raw_specs is None:
        return
    if isinstance(raw_specs, (WebhookSpec, str, dict)):
        specs = [raw_specs]
    elif isinstance(raw_specs, (list, tuple)):
        specs = list(raw_specs)
    else:
        raise ValueError("WEBHOOKS must be a WebhookSpec, dict, slug string, or list")

    for raw in specs:
        spec = _coerce_spec(raw)
        _validate_spec(spec)
        existing = _WEBHOOKS_BY_SLUG.get(spec.slug)
        if existing is not None:
            raise ValueError(
                "duplicate webhook slug "
                f"{spec.slug!r} for workflows {existing.workflow_name!r} and {workflow_name!r}",
            )
        normalized_spec = WebhookSpec(
            slug=spec.slug,
            auth=spec.auth,
            provider=spec.provider,
            trigger_key=spec.trigger_key,
            allowed_methods=[method.upper() for method in spec.allowed_methods],
            allowed_content_types=[
                content_type.lower() for content_type in spec.allowed_content_types
            ],
        )
        _WEBHOOKS_BY_SLUG[normalized_spec.slug] = RegisteredWebhook(
            spec=normalized_spec,
            workflow_name=workflow_name,
            source_path=source_path,
        )
        log.info(
            "workflow_webhook_registered",
            slug=normalized_spec.slug,
            workflow_name=workflow_name,
            auth=(
                normalized_spec.auth
                if isinstance(normalized_spec.auth, str)
                else "hmac"
            ),
            provider=normalized_spec.provider,
        )
