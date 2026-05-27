"""Tool discovery, loading, and registration."""

from __future__ import annotations

import asyncio
import base64
import difflib
import importlib.util
import inspect
import json
import os
import re
import sys
import threading
import time
import tomllib
import types
import uuid
from collections.abc import Callable
from dataclasses import asdict, dataclass, is_dataclass
from enum import Enum
from pathlib import Path
from typing import Any, ClassVar

import structlog
from fastapi import APIRouter, Depends, HTTPException, Request
from fastapi.responses import PlainTextResponse
from toon_format import encode as toon_encode

from api.api_keys import check_scope
from api.otel import (
    context_from_serialized,
    mark_error,
    record_exception,
    set_span_attributes,
    start_span,
)
from api.vm_metrics import record_tool_call
from api.deps import get_key_info, get_sandbox_claims, verify_api_key
from api import slackbot_client
from centaur_sdk import ToolContext, reset_tool_context, set_tool_context

log = structlog.get_logger()


# Headers the legacy raw-string shim lets iron-proxy scan for ``secrets``-transform
# placeholders. Literal strings match a single header name; ``/.../`` is a regex.
# Typed secret entries must instead name the exact headers they touch via
# ``match_headers`` rather than fall back to this blanket set.
DEFAULT_MATCH_HEADERS: tuple[str, ...] = (
    "Authorization",
    "Proxy-Authorization",
    "Api-Key",
    "Anthropic-Api-Key",
    "Auth-Token",
    "Jwt",
    "Cookie",
    "Apikey",
    "AccessKey",
    "Api-Access-Key",
    "Api-Signature",
    "FX-ACCESS-KEY",
    "FX-ACCESS-SIGN",
    "FX-ACCESS-PASSPHRASE",
    "X-CB-ACCESS-PASSPHRASE",
    "X-CB-ACCESS-SIGNATURE",
    "/^x-[a-z0-9-]*(api-key|apikey|secret|token|auth|key)$/",
)


class SecretMode(str, Enum):
    """How iron-proxy's ``secrets`` transform applies an HTTP credential.

    ``replace`` — the tool writes the ``replacer`` placeholder token somewhere
    in the request (a header, the query string, or the path) and iron-proxy
    swaps it for the resolved value.
    ``inject`` — iron-proxy adds the credential to the request itself; the tool
    never sees it and emits no placeholder.
    """

    REPLACE = "replace"
    INJECT = "inject"


@dataclass(frozen=True)
class HttpSecret:
    """An HTTP credential applied by iron-proxy's ``secrets`` transform.

    The credential may ride in a header, the query string, or the path —
    ``match_*`` (replace mode) and ``inject_*`` (inject mode) say exactly where.

    Replace mode (the default): the tool sees ``replacer`` (a placeholder
    token), writes it into the request, and iron-proxy swaps it for the real
    value resolved from ``secret_ref`` (env var or 1Password item).
    ``match_headers`` names the exact headers iron-proxy scans — each entry is a
    literal header name or a ``/.../``-delimited regex. ``match_path`` also
    scans the URL path. ``match_query`` also scans the query string. At least
    one of the three must be set.

    Inject mode: iron-proxy adds the credential itself and the tool never sees
    it. Set ``inject_header`` (optionally with a Go-template ``inject_formatter``
    such as ``Bearer {{ .Value }}``) or ``inject_query_param``.

    ``hosts`` scopes the secret to the upstreams it belongs to — it becomes the
    iron-proxy ``rules`` for this entry. Each secret carries its own hosts so a
    tool's credentials are never offered to an unrelated upstream.
    """

    name: str
    secret_ref: str
    mode: SecretMode = SecretMode.REPLACE
    hosts: tuple[str, ...] = ()
    # Replace mode — where iron-proxy scans for ``replacer``.
    replacer: str = ""
    match_headers: tuple[str, ...] = ()
    match_path: bool = False
    match_query: bool = False
    # Inject mode — where iron-proxy writes the resolved credential.
    inject_header: str = ""
    inject_formatter: str = ""
    inject_query_param: str = ""

    def __post_init__(self) -> None:
        # Replace-mode secrets need a placeholder; default it to the name so
        # callers only pass ``replacer`` when it must differ from ``name``.
        if self.mode is SecretMode.REPLACE and not self.replacer:
            object.__setattr__(self, "replacer", self.name)


@dataclass(frozen=True)
class GcpAuthSecret:
    """GCP service-account keyfile fed to iron-proxy's ``gcp_auth`` transform.

    iron-proxy loads the keyfile from ``secret_ref``, mints OAuth2 tokens for
    ``scopes``, and injects them as ``Authorization: Bearer`` on the upstreams
    in ``hosts``.

    ``hosts`` scopes the auth to a subset of upstreams, so multiple GCP
    keyfiles can coexist — each request routes to the right keyfile by host.
    When empty it defaults to all of ``*.googleapis.com``.

    ``scopes`` are the OAuth2 scopes the minted token is granted. When empty
    it defaults to ``cloud-platform``; Workspace APIs (Gmail, Drive, …) need
    their own scopes set explicitly.
    """

    name: str
    secret_ref: str
    hosts: tuple[str, ...] = ()
    scopes: tuple[str, ...] = ()


@dataclass(frozen=True)
class PgDsnSecret:
    """Postgres DSN proxied by iron-proxy's ``postgres`` transform.

    The sandbox sees a local DSN pointing at iron-proxy on a per-secret listen
    port; iron-proxy fronts the real upstream resolved from ``secret_ref``.

    ``database`` is the dbname the sandbox connects to. iron-proxy forwards
    the client's startup-packet database to the upstream, so this must match
    the dbname in the upstream DSN for the connection to land on the right
    database without an explicit ``\\c`` after connecting.
    """

    name: str
    secret_ref: str
    database: str


@dataclass(frozen=True)
class OAuthFieldSource:
    """One credential field for an ``OAuthTokenSecret``.

    ``secret_ref`` names the secret holding the value. ``json_key``, when set,
    pulls a single key out of a JSON-encoded secret; when unset the whole
    secret value is used.
    """

    secret_ref: str
    json_key: str | None = None


@dataclass(frozen=True)
class OAuthTokenSecret:
    """OAuth2 access token minted by iron-proxy's ``oauth_token`` transform.

    iron-proxy resolves each credential field from its own source, runs the
    ``grant`` token exchange, caches and refreshes the result, and injects it
    as ``Authorization: Bearer`` on the upstreams in ``hosts``. Neither the
    credential fields nor the minted token ever reach the sandbox.

    ``grant`` is one of ``refresh_token`` (RFC 6749 — an authorized-user
    credential), ``client_credentials`` (RFC 6749 4.4 — a client id and
    secret), ``password`` (RFC 6749 4.3 — a resource-owner username and
    password exchanged for a token), or ``jwt_bearer`` (RFC 7523 — a JWT
    assertion signed by an RSA ``private_key`` and exchanged at the token
    endpoint for a bearer token).

    ``fields`` maps each grant's credential fields to a source; fields may be
    sourced from separate secrets or pulled from one JSON secret via
    ``json_key``. ``token_endpoint`` is the OAuth2 token endpoint to exchange
    against. ``token_endpoint_headers`` adds extra headers to the token POST
    itself, each value resolved from its own source; use this when the token
    endpoint requires an API key alongside the standard form-body client auth.
    ``audience`` is the JWT ``aud`` claim — required for ``jwt_bearer``, unused
    by the other grants.
    """

    name: str
    grant: str
    hosts: tuple[str, ...]
    fields: tuple[tuple[str, OAuthFieldSource], ...]
    scopes: tuple[str, ...] = ()
    token_endpoint: str | None = None
    token_endpoint_headers: tuple[tuple[str, OAuthFieldSource], ...] = ()
    audience: str | None = None


@dataclass(frozen=True)
class BrokeredTokenSecret:
    """OAuth2 access token minted by iron-token-broker.

    Same shape as ``OAuthTokenSecret`` minus ``grant`` and ``audience`` and
    minus ``json_key`` on field sources: the broker only handles the
    refresh-token rotation grant, and its store reads/writes a single JSON
    credential blob as a whole document. Use this type when the upstream IdP
    rotates refresh tokens with strict reuse detection (OpenAI Codex,
    Anthropic Claude Code OAuth, modern Okta / Auth0 / Entra ID); the broker
    serializes refresh attempts so multiple iron-proxy instances can share the
    credential without invalidating the token family.

    ``fields`` requires ``client_id`` and ``refresh_token``; ``client_secret``
    is optional. Each field's value is the secret_ref the broker reads or
    writes through its configured store: ``client_id`` / ``client_secret`` are
    read-only credentials; ``refresh_token`` names the writable credential
    blob the broker rewrites on every rotation.
    """

    name: str
    hosts: tuple[str, ...]
    fields: tuple[tuple[str, OAuthFieldSource], ...]
    scopes: tuple[str, ...] = ()
    token_endpoint: str | None = None
    token_endpoint_headers: tuple[tuple[str, OAuthFieldSource], ...] = ()


@dataclass(frozen=True)
class HmacHeader:
    """One header injected by iron-proxy's ``hmac_sign`` transform.

    ``value`` is a Go template evaluated against the request's signing context;
    the iron-proxy schema exposes ``.Timestamp``, ``.Signature``, and
    ``.Credentials.<name>`` (where ``<name>`` is a key from the secret's
    ``credentials`` map).
    """

    name: str
    value: str


@dataclass(frozen=True)
class HmacSignSecret:
    """Per-request HMAC signature minted by iron-proxy's ``hmac_sign`` transform.

    iron-proxy resolves each entry in ``credentials`` from its own source,
    composes the canonical ``message`` template, HMACs it with the credential
    named ``secret`` (decoded per ``key_encoding``), encodes the digest per
    ``output_encoding``, and writes ``headers`` onto the upstream request.
    The credentials and signing key never reach the sandbox.

    ``credentials`` is a map of name → source; the entry named ``secret`` is the
    HMAC key and is required. Other keys are user-named and referenced from
    ``headers[].value`` as ``{{.Credentials.<name>}}``.

    ``allow_chunked_body`` opts in to signing requests with no Content-Length
    (chunked bodies); iron-proxy refuses by default since the body cannot be
    deterministically hashed in flight.
    """

    name: str
    hosts: tuple[str, ...]
    credentials: tuple[tuple[str, OAuthFieldSource], ...]
    headers: tuple[HmacHeader, ...]
    algorithm: str
    key_encoding: str
    output_encoding: str
    message: str
    timestamp_format: str
    allow_chunked_body: bool = False


SecretDef = (
    HttpSecret
    | GcpAuthSecret
    | PgDsnSecret
    | OAuthTokenSecret
    | BrokeredTokenSecret
    | HmacSignSecret
)


# brokered_token credential fields. Only the refresh-token rotation grant is
# supported (the broker doesn't speak the other OAuth2 grants), so this is a
# single tuple instead of the per-grant table the oauth_token transform uses.
_BROKERED_TOKEN_REQUIRED_FIELDS: frozenset[str] = frozenset(
    {"client_id", "refresh_token"}
)
_BROKERED_TOKEN_OPTIONAL_FIELDS: frozenset[str] = frozenset({"client_secret"})
_BROKERED_TOKEN_FIELDS: frozenset[str] = (
    _BROKERED_TOKEN_REQUIRED_FIELDS | _BROKERED_TOKEN_OPTIONAL_FIELDS
)

# Per-grant credential fields: grant -> (required, optional). Field names are
# the keys iron-proxy expects in each ``oauth_token`` token entry.
_OAUTH_GRANT_FIELDS: dict[str, tuple[frozenset[str], frozenset[str]]] = {
    "refresh_token": (
        frozenset({"refresh_token", "client_id"}),
        frozenset({"client_secret"}),
    ),
    "client_credentials": (
        frozenset({"client_id", "client_secret"}),
        frozenset(),
    ),
    "password": (
        frozenset({"username", "password", "client_id"}),
        frozenset({"client_secret"}),
    ),
    "jwt_bearer": (
        frozenset({"issuer", "subject", "private_key"}),
        frozenset({"private_key_id"}),
    ),
}

_OAUTH_GRANTS: frozenset[str] = frozenset(_OAUTH_GRANT_FIELDS)

# Enums iron-proxy's ``hmac_sign`` transform accepts. Mirrors the upstream
# schema; centralized here so parser errors list the same options the proxy
# would.
_HMAC_ALGORITHMS: frozenset[str] = frozenset({"sha256", "sha512", "sha1"})
_HMAC_KEY_ENCODINGS: frozenset[str] = frozenset({"raw", "base64", "hex"})
_HMAC_OUTPUT_ENCODINGS: frozenset[str] = frozenset({"base64", "hex"})
_HMAC_TIMESTAMP_FORMATS: frozenset[str] = frozenset(
    {"unix_seconds", "unix_millis", "unix_nanos", "rfc3339"}
)
# The HMAC key. Other ``credentials`` keys are user-named and only referenced
# from ``headers[].value`` templates.
_HMAC_REQUIRED_CREDENTIAL = "secret"


def _parse_oauth_field_source(
    secret_name: str, field_name: str, raw: Any
) -> OAuthFieldSource:
    """Parse one ``fields`` entry: a bare ``secret_ref`` string or a table."""
    if isinstance(raw, str):
        if not raw:
            raise ValueError(
                f"oauth_token entry {secret_name!r} field {field_name!r} "
                f"'secret_ref' must be a non-empty string"
            )
        return OAuthFieldSource(secret_ref=raw)
    if not isinstance(raw, dict):
        raise ValueError(
            f"oauth_token entry {secret_name!r} field {field_name!r} must be a "
            f"string or table"
        )
    ref = raw.get("secret_ref")
    if not isinstance(ref, str) or not ref:
        raise ValueError(
            f"oauth_token entry {secret_name!r} field {field_name!r} requires a "
            f"non-empty 'secret_ref'"
        )
    json_key = raw.get("json_key")
    if json_key is not None and (not isinstance(json_key, str) or not json_key):
        raise ValueError(
            f"oauth_token entry {secret_name!r} field {field_name!r} 'json_key' "
            f"must be a non-empty string"
        )
    return OAuthFieldSource(secret_ref=ref, json_key=json_key)


def _parse_oauth_fields(
    secret_name: str, grant: str, raw_fields: Any
) -> tuple[tuple[str, OAuthFieldSource], ...]:
    """Parse and validate the ``fields`` table for an ``oauth_token`` entry."""
    if not isinstance(raw_fields, dict) or not raw_fields:
        raise ValueError(
            f"oauth_token entry {secret_name!r} 'fields' must be a non-empty table"
        )
    required, optional = _OAUTH_GRANT_FIELDS[grant]
    allowed = required | optional
    parsed: dict[str, OAuthFieldSource] = {}
    for field_name, raw in raw_fields.items():
        if field_name not in allowed:
            raise ValueError(
                f"oauth_token entry {secret_name!r} field {field_name!r} is not "
                f"valid for grant {grant!r}; allowed: {sorted(allowed)}"
            )
        parsed[field_name] = _parse_oauth_field_source(
            secret_name, field_name, raw
        )
    missing = required - parsed.keys()
    if missing:
        raise ValueError(
            f"oauth_token entry {secret_name!r} grant {grant!r} requires "
            f"fields {sorted(missing)}"
        )
    return tuple(sorted(parsed.items()))


def _parse_brokered_field_source(
    secret_name: str,
    field_name: str,
    raw: Any,
    *,
    allow_json_key: bool,
) -> OAuthFieldSource:
    """Parse one ``fields`` entry for a ``brokered_token`` secret.

    Read-side fields (``client_id``, ``client_secret``, and entries under
    ``token_endpoint_headers``) accept ``json_key`` so an operator can keep a
    single JSON document in their store and pluck individual values out — the
    same affordance the ``oauth_token`` transform offers. The store field
    (``refresh_token``) rejects ``json_key``: the broker reads and rewrites
    that ref as a whole JSON document, so a key path doesn't apply.
    """
    if isinstance(raw, str):
        if not raw:
            raise ValueError(
                f"brokered_token entry {secret_name!r} field {field_name!r} "
                f"'secret_ref' must be a non-empty string"
            )
        return OAuthFieldSource(secret_ref=raw)
    if not isinstance(raw, dict):
        raise ValueError(
            f"brokered_token entry {secret_name!r} field {field_name!r} must be a "
            f"string or table"
        )
    ref = raw.get("secret_ref")
    if not isinstance(ref, str) or not ref:
        raise ValueError(
            f"brokered_token entry {secret_name!r} field {field_name!r} requires a "
            f"non-empty 'secret_ref'"
        )
    json_key = raw.get("json_key")
    if json_key is None:
        return OAuthFieldSource(secret_ref=ref)
    if not allow_json_key:
        raise ValueError(
            f"brokered_token entry {secret_name!r} field {field_name!r} does not "
            f"support 'json_key'; the broker rewrites the whole credential blob"
        )
    if not isinstance(json_key, str) or not json_key:
        raise ValueError(
            f"brokered_token entry {secret_name!r} field {field_name!r} 'json_key' "
            f"must be a non-empty string"
        )
    return OAuthFieldSource(secret_ref=ref, json_key=json_key)


# The store field (where the broker writes the rotated blob) cannot pull out
# a sub-key — its writes target the whole document.
_BROKERED_STORE_FIELD = "refresh_token"


def _parse_brokered_fields(
    secret_name: str, raw_fields: Any
) -> tuple[tuple[str, OAuthFieldSource], ...]:
    """Parse and validate the ``fields`` table for a ``brokered_token`` entry."""
    if not isinstance(raw_fields, dict) or not raw_fields:
        raise ValueError(
            f"brokered_token entry {secret_name!r} 'fields' must be a non-empty table"
        )
    parsed: dict[str, OAuthFieldSource] = {}
    for field_name, raw in raw_fields.items():
        if field_name not in _BROKERED_TOKEN_FIELDS:
            raise ValueError(
                f"brokered_token entry {secret_name!r} field {field_name!r} is not "
                f"valid; allowed: {sorted(_BROKERED_TOKEN_FIELDS)}"
            )
        parsed[field_name] = _parse_brokered_field_source(
            secret_name,
            field_name,
            raw,
            allow_json_key=field_name != _BROKERED_STORE_FIELD,
        )
    missing = _BROKERED_TOKEN_REQUIRED_FIELDS - parsed.keys()
    if missing:
        raise ValueError(
            f"brokered_token entry {secret_name!r} requires fields {sorted(missing)}"
        )
    return tuple(sorted(parsed.items()))


def _parse_brokered_token_endpoint_headers(
    secret_name: str, raw: Any
) -> tuple[tuple[str, OAuthFieldSource], ...]:
    """Parse ``token_endpoint_headers`` for a ``brokered_token`` entry.

    These are read-only header values, so ``json_key`` is allowed — same as
    the read-side fields above.
    """
    if raw is None:
        return ()
    if not isinstance(raw, dict) or not raw:
        raise ValueError(
            f"brokered_token entry {secret_name!r} 'token_endpoint_headers' must "
            f"be a non-empty table"
        )
    parsed: dict[str, OAuthFieldSource] = {}
    for header_name, value in raw.items():
        if not isinstance(header_name, str) or not header_name:
            raise ValueError(
                f"brokered_token entry {secret_name!r} 'token_endpoint_headers' "
                f"keys must be non-empty header names"
            )
        parsed[header_name] = _parse_brokered_field_source(
            secret_name,
            f"token_endpoint_headers.{header_name}",
            value,
            allow_json_key=True,
        )
    return tuple(sorted(parsed.items()))


def _parse_oauth_token_endpoint_headers(
    secret_name: str, raw: Any
) -> tuple[tuple[str, OAuthFieldSource], ...]:
    """Parse the ``token_endpoint_headers`` table for an ``oauth_token`` entry.

    Each entry maps a header name to a secret source. iron-proxy sends these
    headers on the token POST itself, alongside the form-body client auth, so
    each value is resolved like any other ``OAuthFieldSource``: a bare
    ``secret_ref`` string or a table with ``secret_ref`` and optional
    ``json_key``.
    """
    if raw is None:
        return ()
    if not isinstance(raw, dict) or not raw:
        raise ValueError(
            f"oauth_token entry {secret_name!r} 'token_endpoint_headers' must "
            f"be a non-empty table"
        )
    parsed: dict[str, OAuthFieldSource] = {}
    for header_name, value in raw.items():
        if not isinstance(header_name, str) or not header_name:
            raise ValueError(
                f"oauth_token entry {secret_name!r} 'token_endpoint_headers' "
                f"keys must be non-empty header names"
            )
        parsed[header_name] = _parse_oauth_field_source(
            secret_name, f"token_endpoint_headers.{header_name}", value
        )
    return tuple(sorted(parsed.items()))


def _parse_hmac_credential_source(
    secret_name: str, field_name: str, raw: Any
) -> OAuthFieldSource:
    """Parse one ``credentials`` entry for an ``hmac_sign`` secret.

    Accepts a bare ``secret_ref`` string or a table with ``secret_ref`` and
    optional ``json_key`` (for pulling a single key out of a JSON-encoded
    secret). Mirrors ``_parse_oauth_field_source`` so the two typed secrets
    that resolve named credential fields validate the same shape.
    """
    if isinstance(raw, str):
        if not raw:
            raise ValueError(
                f"hmac_sign entry {secret_name!r} credential {field_name!r} "
                f"'secret_ref' must be a non-empty string"
            )
        return OAuthFieldSource(secret_ref=raw)
    if not isinstance(raw, dict):
        raise ValueError(
            f"hmac_sign entry {secret_name!r} credential {field_name!r} must be "
            f"a string or table"
        )
    ref = raw.get("secret_ref")
    if not isinstance(ref, str) or not ref:
        raise ValueError(
            f"hmac_sign entry {secret_name!r} credential {field_name!r} requires "
            f"a non-empty 'secret_ref'"
        )
    json_key = raw.get("json_key")
    if json_key is not None and (not isinstance(json_key, str) or not json_key):
        raise ValueError(
            f"hmac_sign entry {secret_name!r} credential {field_name!r} "
            f"'json_key' must be a non-empty string"
        )
    return OAuthFieldSource(secret_ref=ref, json_key=json_key)


def _parse_hmac_credentials(
    secret_name: str, raw: Any
) -> tuple[tuple[str, OAuthFieldSource], ...]:
    """Parse ``credentials`` for an ``hmac_sign`` entry; require ``secret``."""
    if not isinstance(raw, dict) or not raw:
        raise ValueError(
            f"hmac_sign entry {secret_name!r} 'credentials' must be a non-empty "
            f"table"
        )
    parsed: dict[str, OAuthFieldSource] = {}
    for field_name, value in raw.items():
        if not isinstance(field_name, str) or not field_name:
            raise ValueError(
                f"hmac_sign entry {secret_name!r} credential names must be "
                f"non-empty strings"
            )
        parsed[field_name] = _parse_hmac_credential_source(
            secret_name, field_name, value
        )
    if _HMAC_REQUIRED_CREDENTIAL not in parsed:
        raise ValueError(
            f"hmac_sign entry {secret_name!r} 'credentials' must include "
            f"{_HMAC_REQUIRED_CREDENTIAL!r} (the HMAC key)"
        )
    return tuple(sorted(parsed.items()))


def _parse_hmac_headers(secret_name: str, raw: Any) -> tuple[HmacHeader, ...]:
    """Parse the ordered ``headers`` list iron-proxy writes onto the request."""
    if not isinstance(raw, list) or not raw:
        raise ValueError(
            f"hmac_sign entry {secret_name!r} 'headers' must be a non-empty list"
        )
    headers: list[HmacHeader] = []
    for index, entry in enumerate(raw):
        if not isinstance(entry, dict):
            raise ValueError(
                f"hmac_sign entry {secret_name!r} header[{index}] must be a table"
            )
        name = entry.get("name")
        value = entry.get("value")
        if not isinstance(name, str) or not name:
            raise ValueError(
                f"hmac_sign entry {secret_name!r} header[{index}] requires a "
                f"non-empty 'name'"
            )
        if not isinstance(value, str) or not value:
            raise ValueError(
                f"hmac_sign entry {secret_name!r} header[{index}] requires a "
                f"non-empty 'value' template"
            )
        headers.append(HmacHeader(name=name, value=value))
    return tuple(headers)


def _parse_hmac_enum(
    secret_name: str, key: str, value: Any, allowed: frozenset[str]
) -> str:
    if not isinstance(value, str) or value not in allowed:
        raise ValueError(
            f"hmac_sign entry {secret_name!r} {key!r} must be one of "
            f"{sorted(allowed)}, got {value!r}"
        )
    return value


_INJECT_ONLY_KEYS: tuple[str, ...] = (
    "inject_header",
    "inject_formatter",
    "inject_query_param",
)
_REPLACE_ONLY_KEYS: tuple[str, ...] = (
    "replacer",
    "match_headers",
    "match_path",
    "match_query",
)


def _parse_str_list(
    entry: dict, key: str, *, name: str, noun: str
) -> tuple[str, ...] | None:
    """Validate an optional non-empty array-of-strings field; ``None`` if absent."""
    raw = entry.get(key)
    if raw is None:
        return None
    if (
        not isinstance(raw, list)
        or not raw
        or not all(isinstance(item, str) and item for item in raw)
    ):
        raise ValueError(
            f"HTTP secret {name!r} has invalid {key!r} "
            f"(expected a non-empty array of {noun}): {raw!r}"
        )
    return tuple(raw)


def _parse_match_headers(entry: dict, *, name: str) -> tuple[str, ...]:
    """Validate ``match_headers``; empty/missing yields ``()`` so other scan locations can stand alone."""
    raw = entry.get("match_headers")
    if raw is None:
        return ()
    if not isinstance(raw, list) or not all(
        isinstance(item, str) and item for item in raw
    ):
        raise ValueError(
            f"HTTP secret {name!r} has invalid 'match_headers' "
            f"(expected an array of header names): {raw!r}"
        )
    return tuple(raw)


def _parse_bool(entry: dict, key: str, *, name: str) -> bool:
    value = entry.get(key, False)
    if not isinstance(value, bool):
        raise ValueError(
            f"HTTP secret {name!r} has invalid {key!r} (expected a boolean): {value!r}"
        )
    return value


def _reject_foreign_keys(
    entry: dict, keys: tuple[str, ...], *, name: str, mode: str
) -> None:
    """Raise if *entry* declares a key that belongs to the other mode."""
    stray = [k for k in keys if k in entry]
    if stray:
        raise ValueError(
            f"{mode}-mode HTTP secret {name!r} must not declare {stray[0]!r}"
        )


def _parse_replace_secret(
    entry: dict, *, name: str, secret_ref: str, hosts: tuple[str, ...]
) -> HttpSecret:
    """Parse a replace-mode HTTP secret: a placeholder plus scan locations."""
    _reject_foreign_keys(entry, _INJECT_ONLY_KEYS, name=name, mode="replace")
    match_headers = _parse_match_headers(entry, name=name)
    match_path = _parse_bool(entry, "match_path", name=name)
    match_query = _parse_bool(entry, "match_query", name=name)
    if not match_headers and not match_path and not match_query:
        raise ValueError(
            f"replace-mode HTTP secret {name!r} must declare where iron-proxy "
            f"scans for it: 'match_headers', 'match_path', and/or 'match_query'"
        )
    replacer = entry.get("replacer", name)
    if not isinstance(replacer, str) or not replacer:
        raise ValueError(f"HTTP secret {name!r} has invalid 'replacer': {entry!r}")
    return HttpSecret(
        name=name,
        secret_ref=secret_ref,
        mode=SecretMode.REPLACE,
        hosts=hosts,
        replacer=replacer,
        match_headers=match_headers,
        match_path=match_path,
        match_query=match_query,
    )


def _parse_inject_secret(
    entry: dict, *, name: str, secret_ref: str, hosts: tuple[str, ...]
) -> HttpSecret:
    """Parse an inject-mode HTTP secret: a target iron-proxy writes itself."""
    _reject_foreign_keys(entry, _REPLACE_ONLY_KEYS, name=name, mode="inject")
    inject_header = entry.get("inject_header", "")
    inject_query_param = entry.get("inject_query_param", "")
    inject_formatter = entry.get("inject_formatter", "")
    for key, value in (
        ("inject_header", inject_header),
        ("inject_query_param", inject_query_param),
        ("inject_formatter", inject_formatter),
    ):
        if not isinstance(value, str):
            raise ValueError(f"HTTP secret {name!r} has invalid {key!r}: {value!r}")
    if bool(inject_header) == bool(inject_query_param):
        raise ValueError(
            f"inject-mode HTTP secret {name!r} must declare exactly one of "
            f"'inject_header' or 'inject_query_param'"
        )
    if inject_formatter and not inject_header:
        raise ValueError(
            f"inject-mode HTTP secret {name!r} sets 'inject_formatter', which "
            f"only applies alongside 'inject_header'"
        )
    return HttpSecret(
        name=name,
        secret_ref=secret_ref,
        mode=SecretMode.INJECT,
        hosts=hosts,
        inject_header=inject_header,
        inject_formatter=inject_formatter,
        inject_query_param=inject_query_param,
    )


def _parse_http_secret(
    entry: dict, *, name: str, secret_ref: str, default_hosts: tuple[str, ...]
) -> HttpSecret:
    """Parse a typed ``http`` secret entry, honoring inject/replace modes."""
    mode_raw = entry.get("mode", SecretMode.REPLACE.value)
    try:
        mode = SecretMode(mode_raw)
    except ValueError:
        raise ValueError(
            f"HTTP secret {name!r} has unknown mode {mode_raw!r} "
            f"(expected 'replace' or 'inject')"
        ) from None
    hosts = _parse_str_list(entry, "hosts", name=name, noun="host patterns")
    if hosts is None:
        hosts = default_hosts
    if mode is SecretMode.REPLACE:
        return _parse_replace_secret(
            entry, name=name, secret_ref=secret_ref, hosts=hosts
        )
    return _parse_inject_secret(entry, name=name, secret_ref=secret_ref, hosts=hosts)


def _parse_secret(entry: Any, *, default_hosts: tuple[str, ...] = ()) -> SecretDef:
    """Normalize a single secret entry from pyproject.toml into a SecretDef.

    ``default_hosts`` is the tool-level ``hosts`` fallback for entries that do
    not carry their own. Raw strings are accepted by a legacy shim: ``"FOO"``
    becomes a replace-mode ``HttpSecret`` that scans every header in
    ``DEFAULT_MATCH_HEADERS`` and inherits ``default_hosts``. Typed table entries
    name the headers they touch via ``match_headers`` and should carry ``hosts``.
    """
    if isinstance(entry, str):
        return HttpSecret(
            name=entry,
            secret_ref=entry,
            mode=SecretMode.REPLACE,
            hosts=default_hosts,
            match_headers=DEFAULT_MATCH_HEADERS,
            replacer=entry,
        )
    if not isinstance(entry, dict):
        raise ValueError(f"secret entry must be a string or table, got {type(entry).__name__}")
    name = entry.get("name")
    if not isinstance(name, str) or not name:
        raise ValueError(f"secret entry missing 'name': {entry!r}")
    # ``header`` is a deprecated alias for ``http``, kept for back-compat.
    secret_type = entry.get("type", "http")
    secret_ref = entry.get("secret_ref", name)
    if not isinstance(secret_ref, str) or not secret_ref:
        raise ValueError(f"secret entry has invalid 'secret_ref': {entry!r}")
    if secret_type in ("http", "header"):
        return _parse_http_secret(
            entry, name=name, secret_ref=secret_ref, default_hosts=default_hosts
        )
    if secret_type == "gcp_auth":
        hosts = entry.get("hosts", [])
        if not isinstance(hosts, list) or not all(
            isinstance(h, str) and h for h in hosts
        ):
            raise ValueError(
                f"gcp_auth entry {name!r} 'hosts' must be an array of non-empty strings"
            )
        scopes = entry.get("scopes", [])
        if not isinstance(scopes, list) or not all(
            isinstance(s, str) and s for s in scopes
        ):
            raise ValueError(
                f"gcp_auth entry {name!r} 'scopes' must be an array of non-empty strings"
            )
        return GcpAuthSecret(
            name=name,
            secret_ref=secret_ref,
            hosts=tuple(hosts),
            scopes=tuple(scopes),
        )
    if secret_type == "oauth_token":
        grant = entry.get("grant")
        if grant not in _OAUTH_GRANTS:
            raise ValueError(
                f"oauth_token entry {name!r} 'grant' must be one of "
                f"{sorted(_OAUTH_GRANTS)}, got {grant!r}"
            )
        hosts = entry.get("hosts", [])
        if (
            not isinstance(hosts, list)
            or not hosts
            or not all(isinstance(h, str) and h for h in hosts)
        ):
            raise ValueError(
                f"oauth_token entry {name!r} 'hosts' must be a non-empty "
                f"array of non-empty strings"
            )
        scopes = entry.get("scopes", [])
        if not isinstance(scopes, list) or not all(
            isinstance(s, str) and s for s in scopes
        ):
            raise ValueError(
                f"oauth_token entry {name!r} 'scopes' must be an array of "
                f"non-empty strings"
            )
        token_endpoint = entry.get("token_endpoint")
        if token_endpoint is not None and (
            not isinstance(token_endpoint, str) or not token_endpoint
        ):
            raise ValueError(
                f"oauth_token entry {name!r} 'token_endpoint' must be a "
                f"non-empty string"
            )
        fields = _parse_oauth_fields(name, grant, entry.get("fields"))
        token_endpoint_headers = _parse_oauth_token_endpoint_headers(
            name, entry.get("token_endpoint_headers")
        )
        audience = entry.get("audience")
        if grant == "jwt_bearer":
            if not isinstance(audience, str) or not audience:
                raise ValueError(
                    f"oauth_token entry {name!r} grant 'jwt_bearer' requires a "
                    f"non-empty 'audience'"
                )
        elif audience is not None:
            raise ValueError(
                f"oauth_token entry {name!r} 'audience' is only valid for grant "
                f"'jwt_bearer', not {grant!r}"
            )
        return OAuthTokenSecret(
            name=name,
            grant=grant,
            hosts=tuple(hosts),
            fields=fields,
            scopes=tuple(scopes),
            token_endpoint=token_endpoint,
            token_endpoint_headers=token_endpoint_headers,
            audience=audience,
        )
    if secret_type == "brokered_token":
        hosts = entry.get("hosts", [])
        if (
            not isinstance(hosts, list)
            or not hosts
            or not all(isinstance(h, str) and h for h in hosts)
        ):
            raise ValueError(
                f"brokered_token entry {name!r} 'hosts' must be a non-empty "
                f"array of non-empty strings"
            )
        scopes = entry.get("scopes", [])
        if not isinstance(scopes, list) or not all(
            isinstance(s, str) and s for s in scopes
        ):
            raise ValueError(
                f"brokered_token entry {name!r} 'scopes' must be an array of "
                f"non-empty strings"
            )
        token_endpoint = entry.get("token_endpoint")
        if token_endpoint is not None and (
            not isinstance(token_endpoint, str) or not token_endpoint
        ):
            raise ValueError(
                f"brokered_token entry {name!r} 'token_endpoint' must be a "
                f"non-empty string"
            )
        fields = _parse_brokered_fields(name, entry.get("fields"))
        token_endpoint_headers = _parse_brokered_token_endpoint_headers(
            name, entry.get("token_endpoint_headers")
        )
        return BrokeredTokenSecret(
            name=name,
            hosts=tuple(hosts),
            fields=fields,
            scopes=tuple(scopes),
            token_endpoint=token_endpoint,
            token_endpoint_headers=token_endpoint_headers,
        )
    if secret_type == "pg_dsn":
        database = entry.get("database")
        if not isinstance(database, str) or not database:
            raise ValueError(
                f"pg_dsn entry {name!r} requires a non-empty 'database' field"
            )
        return PgDsnSecret(name=name, secret_ref=secret_ref, database=database)
    if secret_type == "hmac_sign":
        hosts = entry.get("hosts", [])
        if (
            not isinstance(hosts, list)
            or not hosts
            or not all(isinstance(h, str) and h for h in hosts)
        ):
            raise ValueError(
                f"hmac_sign entry {name!r} 'hosts' must be a non-empty array "
                f"of non-empty strings"
            )
        credentials = _parse_hmac_credentials(name, entry.get("credentials"))
        headers = _parse_hmac_headers(name, entry.get("headers"))
        algorithm = _parse_hmac_enum(
            name, "algorithm", entry.get("algorithm"), _HMAC_ALGORITHMS
        )
        key_encoding = _parse_hmac_enum(
            name, "key_encoding", entry.get("key_encoding"), _HMAC_KEY_ENCODINGS
        )
        output_encoding = _parse_hmac_enum(
            name,
            "output_encoding",
            entry.get("output_encoding"),
            _HMAC_OUTPUT_ENCODINGS,
        )
        timestamp_format = _parse_hmac_enum(
            name,
            "timestamp_format",
            entry.get("timestamp_format"),
            _HMAC_TIMESTAMP_FORMATS,
        )
        message = entry.get("message")
        if not isinstance(message, str) or not message:
            raise ValueError(
                f"hmac_sign entry {name!r} 'message' must be a non-empty "
                f"Go-template string"
            )
        allow_chunked_body = entry.get("allow_chunked_body", False)
        if not isinstance(allow_chunked_body, bool):
            raise ValueError(
                f"hmac_sign entry {name!r} 'allow_chunked_body' must be a boolean"
            )
        return HmacSignSecret(
            name=name,
            hosts=tuple(hosts),
            credentials=credentials,
            headers=headers,
            algorithm=algorithm,
            key_encoding=key_encoding,
            output_encoding=output_encoding,
            message=message,
            timestamp_format=timestamp_format,
            allow_chunked_body=allow_chunked_body,
        )
    raise ValueError(f"unknown secret type {secret_type!r}")


def _parse_secrets(
    entries: Any, *, default_hosts: tuple[str, ...] = ()
) -> list[SecretDef]:
    if entries is None:
        return []
    if not isinstance(entries, list):
        raise ValueError("'secrets'/'optional_secrets' must be an array")
    return [_parse_secret(e, default_hosts=default_hosts) for e in entries]


def _is_replace_secret(secret: SecretDef) -> bool:
    """True for HTTP secrets the tool must populate itself (replace mode)."""
    return isinstance(secret, HttpSecret) and secret.mode is SecretMode.REPLACE


async def _resolve_secrets(secrets: list[SecretDef]) -> dict[str, str]:
    """Return placeholder values for replace-mode HTTP secrets.

    Only replace-mode ``HttpSecret`` entries end up in the tool's
    ``ToolContext`` — the tool gets back the ``replacer`` token, which iron-proxy
    swaps for the real credential at the network boundary. Inject-mode HTTP
    secrets are applied entirely by iron-proxy and never reach the tool.
    ``GcpAuthSecret``, ``OAuthTokenSecret`` and ``PgDsnSecret`` are likewise not
    exposed via context: gcp_auth and oauth_token are minted and injected on the
    wire by iron-proxy, and pg_dsn reaches the tool as an environment variable
    set on the sandbox by the kubernetes backend.
    """
    return {s.name: s.replacer for s in secrets if _is_replace_secret(s)}


_MAX_INLINE_TOOL_BINARY_BYTES = max(
    1024, int(os.getenv("TOOL_BINARY_INLINE_MAX_BYTES", str(1 * 1024 * 1024)))
)
_TOOL_BINARY_PREVIEW_BYTES = max(
    128, int(os.getenv("TOOL_BINARY_PREVIEW_BYTES", str(32 * 1024)))
)

# Threshold for extracting base64-encoded file data from tool results into
# the attachments table.  Anything larger gets stored as an attachment and
# replaced with a download URL so it doesn't bloat the agent context window.
_ATTACHMENT_EXTRACT_MIN_BYTES = 64 * 1024  # 64 KB

# Maximum wall-clock seconds a single tool call may run before being cancelled.
_TOOL_CALL_TIMEOUT_S = float(os.getenv("TOOL_CALL_TIMEOUT_S", "120"))


def _parse_timeout_s(
    value: Any,
    *,
    tool: str,
    default: float | None,
) -> float | None:
    if value is None:
        return default
    if isinstance(value, str) and value.strip().lower() in {"none", "disabled", "off"}:
        return None
    try:
        timeout_s = float(value)
    except (TypeError, ValueError):
        log.warning("tool_invalid_timeout", tool=tool, timeout_s=value)
        return default
    if timeout_s <= 0:
        log.warning("tool_invalid_timeout", tool=tool, timeout_s=value)
        return default
    return timeout_s


def _resolve_timeout_s(tool_conf: dict[str, Any], *, tool: str) -> float | None:
    configured = _parse_timeout_s(
        tool_conf.get("timeout_s"),
        tool=tool,
        default=_TOOL_CALL_TIMEOUT_S,
    )
    env_name = tool_conf.get("timeout_env")
    if env_name is not None:
        if isinstance(env_name, str) and env_name:
            env_value = os.getenv(env_name)
            if env_value:
                return _parse_timeout_s(env_value, tool=tool, default=configured)
        else:
            log.warning("tool_invalid_timeout_env", tool=tool, timeout_env=env_name)
    return configured


def _timeout_label(timeout_s: float | None) -> str:
    return "no timeout" if timeout_s is None else f"{timeout_s:g}s"


def _decode_jsonb(value: Any, fallback: Any) -> Any:
    if isinstance(value, (dict, list)):
        return value
    if isinstance(value, str):
        try:
            return json.loads(value)
        except json.JSONDecodeError:
            return fallback
    return fallback


async def _active_execution_parent_context(
    request: Request | None,
    sandbox_claims: dict[str, Any] | None,
):
    if request is None or not sandbox_claims:
        return None
    thread_key = str(sandbox_claims.get("thread_key") or "")
    if not thread_key:
        return None
    pool = getattr(getattr(request, "app", None), "state", None)
    pool = getattr(pool, "db_pool", None) if pool else None
    if pool is None:
        return None
    row = await pool.fetchrow(
        "SELECT metadata FROM agent_execution_requests "
        "WHERE thread_key = $1 AND status IN ('running', 'cancel_requested', 'retry_wait') "
        "ORDER BY started_at DESC NULLS LAST, claimed_at DESC NULLS LAST, created_at DESC "
        "LIMIT 1",
        thread_key,
    )
    if not row:
        return None
    metadata = _decode_jsonb(row["metadata"], {})
    if not isinstance(metadata, dict):
        return None
    otel = _decode_jsonb(metadata.get("_otel"), {})
    if not isinstance(otel, dict):
        return None
    return context_from_serialized(otel.get("execution_span_context"))


async def _capture_live_slack_send(
    *,
    request: Request | None,
    sandbox_claims: dict[str, Any] | None,
    tool_name: str,
    method_name: str,
    args: dict[str, Any],
) -> dict[str, Any] | None:
    if request is None or not sandbox_claims:
        return None
    if tool_name != "slack" or method_name != "send_message":
        return None

    thread_key = str(sandbox_claims.get("thread_key") or "")
    parts = thread_key.split(":")
    if len(parts) < 4 or parts[0] != "slack":
        return None
    active_channel = parts[2]
    active_thread_ts = parts[3]
    requested_channel = str(args.get("channel") or args.get("channel_id") or "").lstrip("#")
    requested_thread_ts = str(args.get("thread_ts") or "")
    channel_is_id = bool(re.match(r"^[CDG][A-Z0-9]+$", requested_channel))
    if channel_is_id and requested_channel != active_channel:
        return None
    if requested_thread_ts and requested_thread_ts != active_thread_ts:
        return None

    text = str(args.get("text") or args.get("message") or "").strip()
    if not text:
        return None

    pool = getattr(request.app.state, "db_pool", None)
    if pool is None:
        return None
    session_id = await pool.fetchval(
        "SELECT metadata->>'slackbot_agent_session_id' "
        "FROM agent_execution_requests "
        "WHERE thread_key = $1 "
        "AND status = 'running' "
        "AND ("
        "  metadata->>'slackbot_live_delivery' = 'true' "
        "  OR metadata->>('slackbot' || '_v' || '2_live_delivery') = 'true'"
        ") "
        "AND COALESCE(metadata->>'slackbot_agent_session_id', '') <> '' "
        "ORDER BY started_at DESC NULLS LAST, created_at DESC LIMIT 1",
        thread_key,
    )
    session_id = str(session_id or "").strip()
    if not session_id:
        return None

    await slackbot_client.session_text(session_id, text)
    log.info(
        "slack_send_message_captured",
        thread_key=thread_key,
        sandbox_container_id=sandbox_claims.get("container_id"),
        slackbot_agent_session_id=session_id,
    )
    return {
        "captured": True,
        "message": "Captured into the active Slackbot live reply; no separate Slack message was posted.",
        "channel": active_channel,
        "thread_ts": active_thread_ts,
    }


async def _extract_tool_attachment(
    result: dict[str, Any],
    *,
    request: Request | None,
    thread_key: str | None,
    tool_name: str,
) -> dict[str, Any]:
    """If *result* contains a large base64 ``data`` field, store it as an
    attachment and replace the field with a download URL.

    Returns the (possibly modified) result dict.
    """
    data_b64 = result.get("data")
    if not isinstance(data_b64, str) or len(data_b64) < _ATTACHMENT_EXTRACT_MIN_BYTES:
        return result

    # Heuristic: looks like base64 (only base64 chars, length divisible by 4)
    if not re.fullmatch(r"[A-Za-z0-9+/=\n\r]+", data_b64[:256]):
        return result

    pool = getattr(getattr(request, "app", None), "state", None)
    pool = getattr(pool, "db_pool", None) if pool else None
    if pool is None:
        return result

    try:
        raw_bytes = base64.b64decode(data_b64)
    except Exception:
        return result

    att_id = f"att-{uuid.uuid4().hex[:16]}"
    mime_type = result.get("mime_type", "application/octet-stream")
    filename = result.get("filename") or f"{tool_name}_output"

    await pool.execute(
        "INSERT INTO attachments (id, thread_key, message_id, name, mime_type, data) "
        "VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (id) DO NOTHING",
        att_id,
        thread_key or "",
        None,
        filename,
        mime_type,
        raw_bytes,
    )
    log.info(
        "tool_result_attachment_stored",
        tool=tool_name,
        attachment_id=att_id,
        filename=filename,
        mime_type=mime_type,
        size=len(raw_bytes),
    )

    out = {k: v for k, v in result.items() if k != "data"}
    out["attachment_id"] = att_id
    out["download_url"] = f"/agent/attachments/{att_id}/download"
    return out


class ToolMethod:
    def __init__(self, method_name: str, fn: Callable):
        self.method_name = method_name
        self.fn = fn


_LIFECYCLE_METHODS = frozenset({"close", "connect", "disconnect", "shutdown"})

_COMMON_ARGUMENT_ALIASES: dict[str, str] = {
    "channel_id": "channel",
    "count": "limit",
    "max_results": "limit",
    "page_size": "limit",
    "range": "range_notation",
    "sql": "query",
    "table": "table_name",
}

_FORBIDDEN_TOOL_ARGUMENT_NAMES = frozenset(
    {
        "output_path",
        "output_dir",
        "download_path",
        "save_path",
        "dest_path",
        "destination_path",
    }
)


def _tool_arg_validation_error(
    method: ToolMethod, args: dict[str, Any]
) -> dict[str, Any] | None:
    """Return a structured argument error before invoking a tool method."""
    forbidden = sorted(set(args) & _FORBIDDEN_TOOL_ARGUMENT_NAMES)
    if forbidden:
        return {
            "error": "tool_argument_validation_failed",
            "message": (
                "Forbidden argument(s): "
                f"{', '.join(forbidden)}. Tools may not write API-process files "
                "to caller-supplied paths; return Centaur attachments instead."
            ),
            "forbidden_args": forbidden,
        }

    sig = inspect.signature(method.fn)
    params = sig.parameters
    accepts_var_kwargs = any(
        p.kind == inspect.Parameter.VAR_KEYWORD for p in params.values()
    )
    valid_names = {
        name
        for name, param in params.items()
        if param.kind
        in {
            inspect.Parameter.POSITIONAL_OR_KEYWORD,
            inspect.Parameter.KEYWORD_ONLY,
        }
    }
    if not accepts_var_kwargs:
        unexpected = sorted(set(args) - valid_names)
        if unexpected:
            suggestions = {
                key: (
                    _COMMON_ARGUMENT_ALIASES.get(key)
                    if _COMMON_ARGUMENT_ALIASES.get(key) in valid_names
                    else (difflib.get_close_matches(key, valid_names, n=1) or [None])[0]
                )
                for key in unexpected
            }
            return {
                "error": "tool_argument_validation_failed",
                "message": f"Unexpected argument(s): {', '.join(unexpected)}",
                "unexpected_args": unexpected,
                "accepted_args": sorted(valid_names),
                "did_you_mean": {k: v for k, v in suggestions.items() if v},
            }

    missing = sorted(
        name
        for name, param in params.items()
        if param.default is inspect.Parameter.empty
        and param.kind
        in {
            inspect.Parameter.POSITIONAL_OR_KEYWORD,
            inspect.Parameter.KEYWORD_ONLY,
        }
        and name not in args
    )
    if missing:
        return {
            "error": "tool_argument_validation_failed",
            "message": f"Missing required argument(s): {', '.join(missing)}",
            "missing_args": missing,
            "accepted_args": sorted(valid_names),
        }
    return None


def _normalize_for_serialization(data: Any) -> Any:
    """Normalize rich Python values into JSON-friendly structures."""
    if data is None or isinstance(data, (str, int, float, bool)):
        return data
    if isinstance(data, bytes):
        if len(data) > _MAX_INLINE_TOOL_BINARY_BYTES:
            return {
                "encoding": "base64_preview",
                "byte_length": len(data),
                "content_base64": base64.b64encode(
                    data[:_TOOL_BINARY_PREVIEW_BYTES]
                ).decode(),
            }
        return {
            "encoding": "base64",
            "byte_length": len(data),
            "content_base64": base64.b64encode(data).decode(),
        }
    if isinstance(data, Enum):
        return data.value
    if is_dataclass(data):
        return _normalize_for_serialization(asdict(data))
    if isinstance(data, dict):
        return {
            str(key): _normalize_for_serialization(value) for key, value in data.items()
        }
    if isinstance(data, (list, tuple, set)):
        return [_normalize_for_serialization(item) for item in data]

    model_dump = getattr(data, "model_dump", None)
    if callable(model_dump):
        try:
            return _normalize_for_serialization(model_dump())
        except TypeError:
            pass

    to_dict = getattr(data, "to_dict", None)
    if callable(to_dict):
        try:
            return _normalize_for_serialization(to_dict())
        except TypeError:
            pass
    return data


def _to_toon(data: Any) -> str:
    """Encode data as TOON for token-efficient LLM responses, falling back to JSON."""
    normalized = _normalize_for_serialization(data)
    try:
        toon = toon_encode(normalized)
        compact_json = json.dumps(normalized, separators=(",", ":"), default=str)
        return toon if len(toon) <= len(compact_json) else compact_json
    except Exception:
        return json.dumps(normalized, default=str)


def _payload_size_bytes(value: Any) -> int:
    normalized = _normalize_for_serialization(value)
    try:
        return len(
            json.dumps(normalized, separators=(",", ":"), default=str).encode("utf-8")
        )
    except Exception:
        return len(str(normalized).encode("utf-8", errors="replace"))


# Mapping from Python built-in types to clean names for schema output
_BUILTIN_TYPE_NAMES: dict[type, str] = {
    str: "string",
    int: "integer",
    float: "number",
    bool: "boolean",
    list: "array",
    dict: "object",
    type(None): "null",
}


_METHOD_DESCRIPTION_MAX_CHARS = 1200
_DOCSTRING_BOUNDARY_MARKERS = (
    "Args:",
    "Arguments:",
    "Returns:",
    "Return:",
    "Yields:",
    "Raises:",
    "Example:",
    "Examples:",
    "Note:",
    "Notes:",
    "Warning:",
    "See Also:",
    "See also:",
)


def _describe_method_docstring(doc: str | None) -> str:
    """Return the agent-facing description for a tool method's docstring.

    The base implementation used only the docstring's FIRST LINE, which
    silently stripped the rest of any multi-paragraph explanation. For tools
    whose first line is a noun phrase (e.g. ``"Hybrid research engine."``)
    and whose follow-on paragraph explains when to use the method, the agent
    never sees the load-bearing guidance.

    The replacement keeps the full prose summary up to the first Google-style
    section marker (``Args:`` / ``Returns:`` / ``Raises:`` / ``Example:`` /
    ``Note:`` / etc.) or a ``_METHOD_DESCRIPTION_MAX_CHARS`` budget, whichever
    comes first. Parameter docs continue to be exposed structurally on the
    ``parameters`` field, so excluding them from ``description`` is
    intentional — agents should pick methods from the prose and pass args
    from the schema.
    """
    if not doc:
        return ""
    # ``inspect.cleandoc`` is the canonical normalizer for Python docstrings:
    # it strips the common leading whitespace from continuation lines so any
    # subsequent markers match at column 0.
    text = inspect.cleandoc(doc)
    if not text:
        return ""
    boundary = len(text)
    for marker in _DOCSTRING_BOUNDARY_MARKERS:
        # Markers must appear at start-of-line (column 0) to count.
        idx = text.find("\n" + marker)
        if idx == -1 and text.startswith(marker):
            idx = 0
        if 0 <= idx < boundary:
            boundary = idx
    summary = text[:boundary].rstrip()
    if len(summary) > _METHOD_DESCRIPTION_MAX_CHARS:
        summary = summary[: _METHOD_DESCRIPTION_MAX_CHARS - 1].rstrip() + "\u2026"
    return summary


def _friendly_type_name(annotation: Any) -> str:
    """Convert a Python type annotation to a clean, human-readable string.

    Avoids raw ``<class 'str'>`` output by using simple names for built-in types
    and ``str()`` for union / generic forms.
    """
    if annotation in _BUILTIN_TYPE_NAMES:
        return _BUILTIN_TYPE_NAMES[annotation]
    origin = getattr(annotation, "__origin__", None)
    args = getattr(annotation, "__args__", None)
    # typing.Optional / Union / str | int (PEP 604)
    if (
        isinstance(annotation, types.UnionType)
        or (origin is not None and str(origin) == "typing.Union")
    ) and args:
        parts = [_friendly_type_name(a) for a in args]
        return " | ".join(parts)
    # list[X], dict[K, V], etc.
    if origin is not None and args:
        base = _BUILTIN_TYPE_NAMES.get(origin, getattr(origin, "__name__", str(origin)))
        inner = ", ".join(_friendly_type_name(a) for a in args)
        return f"{base}[{inner}]"
    # Plain class — use __name__ if available
    name = getattr(annotation, "__name__", None)
    if name:
        return name
    # Fallback
    return str(annotation)


class LoadedTool:
    def __init__(
        self,
        name: str,
        description: str,
        ctx: ToolContext,
        methods: list[ToolMethod],
        secrets: list[SecretDef] | None = None,
        optional_secrets: list[SecretDef] | None = None,
        timeout_s: float | None = None,
    ):
        self.name = name
        self.description = description
        self.ctx = ctx
        self.methods = methods
        self.secrets: list[SecretDef] = secrets or []
        self.optional_secrets: list[SecretDef] = optional_secrets or []
        self.timeout_s = timeout_s

    @property
    def all_secrets(self) -> list[SecretDef]:
        return self.secrets + self.optional_secrets

    @property
    def secret_names(self) -> list[str]:
        """Names of all declared secrets (required + optional), in declaration order."""
        return [s.name for s in self.all_secrets]


@dataclass
class LoadedPersona:
    name: str
    description: str
    engine: str
    default_repo: str | None
    prompt_content: str
    prompt_file: str
    has_custom_executor: bool  # True if run.py exists in the persona dir
    tool_dir: Path


def load_plugins_config(config_path: Path) -> list[Path]:
    """Read a tools.toml and return resolved plugin directory paths.

    The TOML file is expected to contain a ``plugin_dirs`` key whose value is a
    list of directory paths (strings).  Relative paths are resolved against the
    config file's parent directory.  Returns an empty list when the file does not
    exist.
    """
    if not config_path.exists():
        return []
    base = config_path.parent
    with open(config_path, "rb") as f:
        data = tomllib.load(f)
    dirs: list[Path] = []
    for entry in data.get("plugin_dirs", []):
        p = Path(entry)
        dirs.append(p if p.is_absolute() else (base / p).resolve())
    return dirs


class ToolManager:
    def __init__(
        self,
        tools_dir: Path | list[Path],
    ):
        if isinstance(tools_dir, list):
            self.tools_dirs: list[Path] = list(tools_dir)
        else:
            self.tools_dirs = [tools_dir]
        self.tools: dict[str, LoadedTool] = {}
        self.personas: dict[str, LoadedPersona] = {}
        self.load_failures: list[dict[str, str]] = []
        self._reload_lock = threading.Lock()

    def _collect_tools(self) -> list[tuple[Path, dict]]:
        """Read pyproject.toml from each tool dir.

        Directories in ``self.tools_dirs`` are scanned in order.  When the same
        tool name appears in a later directory it shadows the earlier one (useful
        for private-overrides-public).

        Supports one level of category subdirectories: if a child directory has
        no ``pyproject.toml`` it is treated as a category folder and its children
        are scanned for tools (e.g. ``tools/crypto/alchemy/``).
        """
        seen: dict[str, int] = {}
        tools: list[tuple[Path, dict]] = []
        for dir_idx, base_dir in enumerate(self.tools_dirs):
            if not base_dir.exists():
                continue
            # Collect candidate tool dirs, expanding category subdirectories
            candidates: list[Path] = []
            for child in sorted(base_dir.iterdir()):
                if not child.is_dir() or child.name.startswith((".", "_")):
                    continue
                if (child / "pyproject.toml").exists():
                    candidates.append(child)
                else:
                    # Category dir — scan its children
                    for sub in sorted(child.iterdir()):
                        if sub.is_dir() and not sub.name.startswith((".", "_")):
                            candidates.append(sub)

            for tool_dir in candidates:
                pyproject_path = tool_dir / "pyproject.toml"
                if not pyproject_path.exists():
                    continue

                with open(pyproject_path, "rb") as f:
                    pyproject = tomllib.load(f)

                project = pyproject.get("project", {})
                tool_conf = pyproject.get("tool", {}).get("centaur", {})

                name = tool_dir.name
                # Tool-level ``hosts`` is the legacy fallback for secret entries
                # that do not carry their own; each secret should declare its.
                default_hosts = tuple(tool_conf.get("hosts", []))
                try:
                    secrets = _parse_secrets(
                        tool_conf.get("secrets"), default_hosts=default_hosts
                    )
                    optional_secrets = _parse_secrets(
                        tool_conf.get("optional_secrets"), default_hosts=default_hosts
                    )
                except ValueError as exc:
                    log.warning(
                        "tool_invalid_secrets",
                        tool=name,
                        error=str(exc),
                    )
                    continue

                # Validate the host patterns each secret is scoped to.
                for secret in (*secrets, *optional_secrets):
                    if not isinstance(secret, HttpSecret):
                        continue
                    for h in secret.hosts:
                        if h in ("*", "*.com", "*.org", "*.net", "*.io"):
                            log.warning(
                                "tool_invalid_host",
                                tool=name,
                                secret=secret.name,
                                host=h,
                                reason="catch-all domain not allowed",
                            )
                        elif re.match(r"^\d+\.\d+\.\d+\.\d+$", h):
                            log.warning(
                                "tool_invalid_host",
                                tool=name,
                                secret=secret.name,
                                host=h,
                                reason="IP addresses not allowed",
                            )

                # Skip persona entries — they are loaded separately
                if tool_conf.get("type") == "persona":
                    continue

                meta = {
                    "name": name,
                    "description": project.get("description", ""),
                    "module": tool_conf.get("module", "client.py"),
                    "secrets": secrets,
                    "optional_secrets": optional_secrets,
                    "timeout_s": _resolve_timeout_s(tool_conf, tool=name),
                }

                if name in seen:
                    prev_idx = seen[name]
                    prev_pos = next(
                        i for i, (_, m) in enumerate(tools) if m["name"] == name
                    )
                    log.info(
                        "tool_shadowed",
                        tool=name,
                        shadowed_dir=str(self.tools_dirs[prev_idx]),
                        by_dir=str(base_dir),
                    )
                    tools[prev_pos] = (tool_dir, meta)
                else:
                    tools.append((tool_dir, meta))
                seen[name] = dir_idx
        return tools

    def _collect_personas(self) -> list[tuple[Path, dict, dict]]:
        """Scan tools dirs for persona entries (type=persona in pyproject.toml)."""
        personas: list[tuple[Path, dict, dict]] = []
        for base_dir in self.tools_dirs:
            if not base_dir.exists():
                continue
            for child in sorted(base_dir.iterdir()):
                if not child.is_dir() or child.name.startswith((".", "_")):
                    continue
                # Check direct children and category subdirectories
                candidates: list[Path] = []
                if (child / "pyproject.toml").exists():
                    candidates.append(child)
                else:
                    for sub in sorted(child.iterdir()):
                        if sub.is_dir() and not sub.name.startswith((".", "_")):
                            if (sub / "pyproject.toml").exists():
                                candidates.append(sub)

                for tool_dir in candidates:
                    with open(tool_dir / "pyproject.toml", "rb") as f:
                        pyproject = tomllib.load(f)
                    project = pyproject.get("project", {})
                    tool_conf = pyproject.get("tool", {}).get("centaur", {})
                    if tool_conf.get("type") != "persona":
                        continue
                    personas.append((tool_dir, project, tool_conf))
        return personas

    def _load_persona(
        self, tool_dir: Path, project: dict, tool_conf: dict
    ) -> LoadedPersona:
        """Load a single persona from its directory."""
        name = tool_dir.name
        prompt_file = tool_conf.get("prompt", "PROMPT.md")
        prompt_path = tool_dir / prompt_file
        prompt_content = prompt_path.read_text() if prompt_path.exists() else ""
        has_custom_executor = (tool_dir / "run.py").exists()
        return LoadedPersona(
            name=name,
            description=project.get("description", ""),
            engine=tool_conf.get("engine", "amp"),
            default_repo=tool_conf.get("default_repo"),
            prompt_content=prompt_content,
            prompt_file=prompt_file,
            has_custom_executor=has_custom_executor,
            tool_dir=tool_dir,
        )

    def get_persona(self, name: str) -> LoadedPersona | None:
        """Return a loaded persona by name, or None."""
        return self.personas.get(name)

    def discover(self) -> list[LoadedTool]:
        """Discover and load all tools and personas."""
        existing = [d for d in self.tools_dirs if d.exists()]
        if not existing:
            self.load_failures = []
            log.info("tools_dirs_missing", paths=[str(d) for d in self.tools_dirs])
            return []

        tool_entries = self._collect_tools()

        # Load each tool
        loaded = []
        load_failures: list[dict[str, str]] = []
        for tool_dir, meta in tool_entries:
            try:
                lt = self._load_tool(tool_dir, meta)
                if lt:
                    loaded.append(lt)
            except Exception as exc:
                tool_name = str(meta.get("name", tool_dir.name))
                load_failures.append({"name": tool_name, "error": str(exc)})
                log.warning(
                    "tool_load_failed",
                    tool=tool_name,
                    error=str(exc),
                )

        self.load_failures = load_failures
        self.tools = {p.name: p for p in loaded}

        # Load personas
        personas: dict[str, LoadedPersona] = {}
        for tool_dir, project, tool_conf in self._collect_personas():
            try:
                persona = self._load_persona(tool_dir, project, tool_conf)
                personas[persona.name] = persona
                log.info("persona_loaded", persona=persona.name, engine=persona.engine)
            except Exception as exc:
                log.warning(
                    "persona_load_failed", persona=tool_dir.name, error=str(exc)
                )
        self.personas = personas

        log.info(
            "tools_discovery_complete",
            loaded=len(loaded),
            failed=len(load_failures),
            failed_tools=[f["name"] for f in load_failures],
            personas=list(personas.keys()),
        )
        return loaded

    # Hardcoded infrastructure secrets for the injection map. Each ``HttpSecret``
    # carries the hosts iron-proxy attaches it to.
    _INFRA_SECRETS: ClassVar[list[HttpSecret]] = [
        HttpSecret(
            name="ANTHROPIC_API_KEY",
            secret_ref="ANTHROPIC_API_KEY",
            hosts=("api.anthropic.com",),
            match_headers=("X-Api-Key",),
        ),
        HttpSecret(
            name="OPENAI_API_KEY",
            secret_ref="OPENAI_API_KEY",
            hosts=("api.openai.com",),
            match_headers=("Authorization",),
        ),
        HttpSecret(
            name="XAI_API_KEY",
            secret_ref="XAI_API_KEY",
            hosts=("api.x.ai",),
            match_headers=("Authorization",),
        ),
        HttpSecret(
            name="GEMINI_API_KEY",
            secret_ref="GEMINI_API_KEY",
            hosts=("generativelanguage.googleapis.com",),
            match_headers=("X-Goog-Api-Key",),
        ),
        HttpSecret(
            name="AMP_API_KEY",
            secret_ref="AMP_API_KEY",
            hosts=("ampcode.com",),
            match_headers=("Authorization",),
        ),
        HttpSecret(
            name="GITHUB_TOKEN",
            secret_ref="GITHUB_TOKEN",
            hosts=("github.com", "api.github.com"),
            match_headers=("Authorization",),
        ),
        HttpSecret(
            name="SLACK_BOT_TOKEN",
            secret_ref="SLACK_BOT_TOKEN",
            hosts=("*.slack.com",),
            match_headers=("Authorization",),
        ),
    ]

    def collect_secrets(self) -> list[SecretDef]:
        """Return all secrets (infra + tool).

        Every ``HttpSecret``, ``GcpAuthSecret`` and ``OAuthTokenSecret`` carries
        its own ``hosts``; ``PgDsnSecret`` is a TCP listener with no host.
        """
        out: list[SecretDef] = list(self._INFRA_SECRETS)
        for lt in self.tools.values():
            out.extend(lt.all_secrets)
        return out

    def reload(self) -> dict[str, Any]:
        """Reload all tools by clearing module caches and re-discovering."""
        with self._reload_lock:
            stale = [k for k in sys.modules if k.startswith("shared.tools_runtime.")]
            for k in stale:
                del sys.modules[k]

            loaded = self.discover()
            return {
                "reloaded": len(loaded),
                "tools": [p.name for p in loaded],
            }

    def _load_tool(self, tool_dir: Path, manifest: dict) -> LoadedTool | None:
        name = manifest["name"]
        ctx = ToolContext(name=name, secrets={})

        # Register the tool dir as a package so relative imports work
        pkg_name = f"shared.tools_runtime.{name}"
        init_path = tool_dir / "__init__.py"
        if init_path.exists():
            pkg_spec = importlib.util.spec_from_file_location(
                pkg_name,
                init_path,
                submodule_search_locations=[str(tool_dir)],
            )
            if pkg_spec and pkg_spec.loader:
                pkg_mod = importlib.util.module_from_spec(pkg_spec)
                sys.modules[pkg_name] = pkg_mod
                pkg_spec.loader.exec_module(pkg_mod)
        else:
            # Create a virtual package
            pkg_mod = types.ModuleType(pkg_name)
            pkg_mod.__path__ = [str(tool_dir)]  # type: ignore[attr-defined]
            sys.modules[pkg_name] = pkg_mod

        # Ensure parent namespaces exist
        if "shared" not in sys.modules:
            ns = types.ModuleType("shared")
            ns.__path__ = []  # type: ignore[attr-defined]
            sys.modules["shared"] = ns
        if "shared.tools_runtime" not in sys.modules:
            ns = types.ModuleType("shared.tools_runtime")
            ns.__path__ = []  # type: ignore[attr-defined]
            sys.modules["shared.tools_runtime"] = ns

        # Import the tool module
        module_file = manifest.get("module", "client.py")
        module_path = tool_dir / module_file
        if not module_path.exists():
            log.warning("tool_module_missing", tool=name, module=module_file)
            return None

        mod_name = f"{pkg_name}.{Path(module_file).stem}"
        spec = importlib.util.spec_from_file_location(mod_name, module_path)
        if not spec or not spec.loader:
            return None
        module = importlib.util.module_from_spec(spec)
        module.__package__ = pkg_name  # type: ignore[attr-defined]
        sys.modules[mod_name] = module

        # Set tool context so _client() factories can call secret()
        token = set_tool_context(ctx)
        try:
            spec.loader.exec_module(module)
            methods = self._collect_methods(module)
        finally:
            reset_tool_context(token)

        description = manifest.get("description", "")
        loaded_tool = LoadedTool(
            name=name,
            description=description,
            ctx=ctx,
            methods=methods,
            secrets=manifest.get("secrets", []),
            optional_secrets=manifest.get("optional_secrets", []),
            timeout_s=manifest.get("timeout_s"),
        )
        log.info(
            "tool_loaded",
            tool=name,
            methods=[m.method_name for m in methods],
        )
        return loaded_tool

    @staticmethod
    def _collect_methods(module: Any) -> list[ToolMethod]:
        """Collect tools from a tool module.

        The module must have a _client() factory. Call it once to get a cached
        instance and expose every public method as a tool.
        """
        methods: list[ToolMethod] = []

        factory = getattr(module, "_client", None)
        if factory and callable(factory):
            instance = factory()
            for method_name, descriptor in sorted(
                vars(type(instance)).items(),
                key=lambda item: item[0],
            ):
                if method_name.startswith("_") or method_name in _LIFECYCLE_METHODS:
                    continue
                if isinstance(descriptor, property):
                    continue
                if not callable(descriptor):
                    continue
                method = getattr(instance, method_name, None)
                if not inspect.ismethod(method):
                    continue
                methods.append(ToolMethod(method_name, method))

        return methods

    def describe_tool(self, tool_name: str) -> dict[str, Any]:
        """Return full method schemas for a tool's methods."""
        lt = self.tools.get(tool_name)
        if not lt:
            return {
                "error": f"Tool '{tool_name}' not found",
                "available": sorted(self.tools.keys()),
            }
        method_schemas: list[dict[str, Any]] = []
        for method in sorted(lt.methods, key=lambda m: m.method_name):
            description = _describe_method_docstring(method.fn.__doc__)
            try:
                sig = inspect.signature(method.fn)
            except (TypeError, ValueError) as exc:
                method_schemas.append(
                    {
                        "name": method.method_name,
                        "description": description,
                        "parameters": {},
                        "signature_error": str(exc),
                    }
                )
                continue
            params: dict[str, Any] = {}
            for pname, param in sig.parameters.items():
                if pname == "self":
                    continue
                ptype = "any"
                if param.annotation is not inspect.Parameter.empty:
                    ptype = _friendly_type_name(param.annotation)
                pinfo: dict[str, Any] = {"type": ptype}
                if param.default is not inspect.Parameter.empty:
                    pinfo["default"] = param.default
                else:
                    pinfo["required"] = True
                params[pname] = pinfo
            method_schemas.append(
                {
                    "name": method.method_name,
                    "description": description,
                    "parameters": params,
                }
            )
        return {
            "tool": lt.name,
            "description": lt.description,
            "methods": method_schemas,
        }

    async def call_tool_raw(
        self,
        tool_name: str,
        method_name: str,
        args: dict[str, Any],
        *,
        request: Request | None = None,
    ) -> Any:
        """Call a tool method by name and return the raw Python result.

        Like ``call_tool`` but skips TOON/JSON serialization so the caller gets
        the native return value (e.g. a dict with binary data).
        """
        lt = self.tools.get(tool_name)
        if not lt:
            return {
                "error": f"Tool '{tool_name}' not found",
                "available": sorted(self.tools.keys()),
            }

        method = next((m for m in lt.methods if m.method_name == method_name), None)
        if not method:
            return {
                "error": f"Method '{method_name}' not found in tool '{tool_name}'",
                "available_methods": sorted(m.method_name for m in lt.methods),
            }

        sandbox_claims = get_sandbox_claims(request) if request is not None else None
        call_fields = {
            "tool_name": tool_name,
            "tool_method": method_name,
            "arg_keys": sorted(args.keys()),
            "arg_size_bytes": _payload_size_bytes(args),
            **(
                {
                    "thread_key": sandbox_claims.get("thread_key"),
                    "sandbox_container_id": sandbox_claims.get("container_id"),
                }
                if sandbox_claims
                else {}
            ),
        }
        t0 = time.monotonic()
        log.info("tool_call_started", **call_fields)
        captured_slack_send = await _capture_live_slack_send(
            request=request,
            sandbox_claims=sandbox_claims,
            tool_name=tool_name,
            method_name=method_name,
            args=args,
        )
        if captured_slack_send is not None:
            duration_ms = round((time.monotonic() - t0) * 1000)
            log.info(
                "tool_call_completed",
                duration_ms=duration_ms,
                success=True,
                result_size_bytes=_payload_size_bytes(captured_slack_send),
                captured=True,
                **call_fields,
            )
            return captured_slack_send
        validation_error = _tool_arg_validation_error(method, args)
        if validation_error is not None:
            log.warning(
                "tool_argument_validation_failed",
                error=validation_error["message"],
                **call_fields,
            )
            return validation_error

        ctx = lt.ctx
        all_secrets = lt.all_secrets
        if all_secrets:
            resolved = await _resolve_secrets(all_secrets)
            if resolved:
                ctx = ToolContext(
                    name=lt.name,
                    secrets={**lt.ctx.secrets, **resolved},
                    thread_key=sandbox_claims.get("thread_key")
                    if sandbox_claims
                    else None,
                    container_id=sandbox_claims.get("container_id")
                    if sandbox_claims
                    else None,
                )
            elif sandbox_claims:
                ctx = ToolContext(
                    name=lt.name,
                    secrets=dict(lt.ctx.secrets),
                    thread_key=sandbox_claims.get("thread_key"),
                    container_id=sandbox_claims.get("container_id"),
                )
        elif sandbox_claims:
            ctx = ToolContext(
                name=lt.name,
                secrets=dict(lt.ctx.secrets),
                thread_key=sandbox_claims.get("thread_key"),
                container_id=sandbox_claims.get("container_id"),
            )

        token = set_tool_context(ctx)
        try:
            if inspect.iscoroutinefunction(method.fn):
                coro = method.fn(**args)
            else:
                coro = asyncio.to_thread(method.fn, **args)
            result = await asyncio.wait_for(coro, timeout=lt.timeout_s)
            duration_ms = round((time.monotonic() - t0) * 1000)
            log.info(
                "tool_call_completed",
                duration_ms=duration_ms,
                success=True,
                result_size_bytes=_payload_size_bytes(result),
                **call_fields,
            )
            return result
        except (SystemExit, Exception) as e:
            duration_ms = round((time.monotonic() - t0) * 1000)
            if isinstance(e, asyncio.TimeoutError):
                error_msg = f"Tool call timed out after {_timeout_label(lt.timeout_s)}"
            elif isinstance(e, SystemExit):
                error_msg = f"sys.exit({e.code})"
            else:
                error_msg = str(e)
            log.warning(
                "tool_call_completed",
                duration_ms=duration_ms,
                success=False,
                error=error_msg,
                error_type=type(e).__name__,
                **call_fields,
            )
            return {"error": error_msg, "tool": tool_name, "method": method_name}
        finally:
            reset_tool_context(token)

    async def call_tool(
        self,
        tool_name: str,
        method_name: str,
        args: dict[str, Any],
        *,
        request: Request | None = None,
        format: str = "json",
    ) -> str | Any:
        """Call a tool method by name.

        *format* controls the response serialization:
        - ``"toon"``  – token-efficient TOON string (used by sandbox agents).
        - ``"json"``  – return the normalised Python object as-is (default).
        """
        lt = self.tools.get(tool_name)
        if not lt:
            return json.dumps(
                {
                    "error": f"Tool '{tool_name}' not found",
                    "available": sorted(self.tools.keys()),
                }
            )

        method = next((m for m in lt.methods if m.method_name == method_name), None)
        if not method:
            return json.dumps(
                {
                    "error": f"Method '{method_name}' not found in tool '{tool_name}'",
                    "available_methods": sorted(m.method_name for m in lt.methods),
                }
            )

        sandbox_claims = get_sandbox_claims(request) if request is not None else None
        call_fields = {
            "tool_name": tool_name,
            "tool_method": method_name,
            "arg_keys": sorted(args.keys()),
            "arg_size_bytes": _payload_size_bytes(args),
            **(
                {
                    "thread_key": sandbox_claims.get("thread_key"),
                    "sandbox_container_id": sandbox_claims.get("container_id"),
                }
                if sandbox_claims
                else {}
            ),
        }
        parent_context = await _active_execution_parent_context(request, sandbox_claims)
        with start_span(
            "centaur.tool.call",
            parent_context=parent_context,
            attributes={
                "centaur.tool.name": tool_name,
                "centaur.tool.method": method_name,
                "centaur.thread_key": call_fields.get("thread_key"),
                "centaur.sandbox_container_id": call_fields.get("sandbox_container_id"),
                "centaur.tool.arg_keys": call_fields["arg_keys"],
                "centaur.tool.arg_size_bytes": call_fields["arg_size_bytes"],
            },
        ) as span:
            t0 = time.monotonic()
            log.info("tool_call_started", **call_fields)
            captured_slack_send = await _capture_live_slack_send(
                request=request,
                sandbox_claims=sandbox_claims,
                tool_name=tool_name,
                method_name=method_name,
                args=args,
            )
            if captured_slack_send is not None:
                duration_ms = round((time.monotonic() - t0) * 1000)
                set_span_attributes(
                    span,
                    {
                        "centaur.tool.duration_ms": duration_ms,
                        "centaur.tool.success": True,
                        "centaur.tool.captured": True,
                        "centaur.tool.result_size_bytes": _payload_size_bytes(
                            captured_slack_send
                        ),
                    },
                )
                log.info(
                    "tool_call_completed",
                    duration_ms=duration_ms,
                    success=True,
                    result_size_bytes=_payload_size_bytes(captured_slack_send),
                    captured=True,
                    **call_fields,
                )
                record_tool_call(tool_name, method_name, True, duration_ms / 1000)
                if format == "toon":
                    return _to_toon(captured_slack_send)
                return _normalize_for_serialization(captured_slack_send)
            validation_error = _tool_arg_validation_error(method, args)
            if validation_error is not None:
                mark_error(span, validation_error["message"])
                log.warning(
                    "tool_argument_validation_failed",
                    error=validation_error["message"],
                    **call_fields,
                )
                return json.dumps(validation_error)

            # Resolve placeholder secrets for tools that declare them. Required
            # secrets gate availability elsewhere; optional secrets should still be
            # present in ToolContext when declared so tool code can choose to use
            # them.
            ctx = lt.ctx
            all_secrets = lt.all_secrets
            if all_secrets:
                resolved = await _resolve_secrets(all_secrets)
                log.info(
                    "tool_secrets_resolved",
                    tool=tool_name,
                    keys=list(resolved.keys()),
                    declared=[s.name for s in all_secrets],
                )
                if resolved:
                    ctx = ToolContext(
                        name=lt.name,
                        secrets={**lt.ctx.secrets, **resolved},
                        thread_key=sandbox_claims.get("thread_key")
                        if sandbox_claims
                        else None,
                        container_id=sandbox_claims.get("container_id")
                        if sandbox_claims
                        else None,
                    )
                elif sandbox_claims:
                    ctx = ToolContext(
                        name=lt.name,
                        secrets=dict(lt.ctx.secrets),
                        thread_key=sandbox_claims.get("thread_key"),
                        container_id=sandbox_claims.get("container_id"),
                    )
            elif sandbox_claims:
                ctx = ToolContext(
                    name=lt.name,
                    secrets=dict(lt.ctx.secrets),
                    thread_key=sandbox_claims.get("thread_key"),
                    container_id=sandbox_claims.get("container_id"),
                )

            token = set_tool_context(ctx)
            try:
                if inspect.iscoroutinefunction(method.fn):
                    coro = method.fn(**args)
                else:
                    coro = asyncio.to_thread(method.fn, **args)
                result = await asyncio.wait_for(coro, timeout=lt.timeout_s)
                duration_ms = round((time.monotonic() - t0) * 1000)
                result_size_bytes = _payload_size_bytes(result)
                set_span_attributes(
                    span,
                    {
                        "centaur.tool.duration_ms": duration_ms,
                        "centaur.tool.success": True,
                        "centaur.tool.result_size_bytes": result_size_bytes,
                    },
                )
                log.info(
                    "tool_call_completed",
                    duration_ms=duration_ms,
                    success=True,
                    result_size_bytes=result_size_bytes,
                    **call_fields,
                )
                record_tool_call(tool_name, method_name, True, duration_ms / 1000)
                if isinstance(result, dict):
                    thread_key = (
                        sandbox_claims.get("thread_key") if sandbox_claims else None
                    )
                    result = await _extract_tool_attachment(
                        result,
                        request=request,
                        thread_key=thread_key,
                        tool_name=tool_name,
                    )
                if format == "toon":
                    return result if isinstance(result, str) else _to_toon(result)
                return _normalize_for_serialization(result)
            except (SystemExit, Exception) as e:
                duration_ms = round((time.monotonic() - t0) * 1000)
                if isinstance(e, asyncio.TimeoutError):
                    error_msg = f"Tool call timed out after {_timeout_label(lt.timeout_s)}"
                elif isinstance(e, SystemExit):
                    error_msg = f"sys.exit({e.code})"
                else:
                    error_msg = str(e)
                set_span_attributes(
                    span,
                    {
                        "centaur.tool.duration_ms": duration_ms,
                        "centaur.tool.success": False,
                        "error.type": type(e).__name__,
                    },
                )
                record_exception(span, e)
                log.warning(
                    "tool_call_completed",
                    duration_ms=duration_ms,
                    success=False,
                    error=error_msg,
                    error_type=type(e).__name__,
                    **call_fields,
                )
                record_tool_call(tool_name, method_name, False, duration_ms / 1000)
                return json.dumps(
                    {"error": error_msg, "tool": tool_name, "method": method_name}
                )
            finally:
                reset_tool_context(token)

    def create_rest_router(self) -> APIRouter:
        """Create a stable FastAPI router that dispatches to tools via live lookup.

        Routes are fixed at registration time — tool calls resolve through
        ``self.tools`` at request time so hot-reloads take effect without
        swapping routes.
        """
        pm = self
        router = APIRouter(
            prefix="/tools",
            dependencies=[Depends(verify_api_key)],
        )

        def _require_tool_scope(request: Request, tool_name: str) -> None:
            key_info = get_key_info(request)
            if not check_scope(key_info, "tools", tool_name):
                raise HTTPException(
                    status_code=403,
                    detail=f"API key does not have access to tool '{tool_name}'",
                )

        @router.get("")
        async def list_tools(request: Request) -> dict:
            key_info = get_key_info(request)
            result = {}
            for name, p in pm.tools.items():
                if not check_scope(key_info, "tools", name):
                    continue
                required_secrets = [s for s in p.secrets if _is_replace_secret(s)]
                if required_secrets:
                    resolved = await _resolve_secrets(required_secrets)
                    if len(resolved) < len(required_secrets):
                        continue
                result[name] = {
                    "description": p.description,
                    "methods": [m.method_name for m in p.methods],
                }
            return result

        # ── Persona endpoints (registered before catch-all /{tool_name}) ─────

        @router.get("/personas")
        async def list_personas() -> dict:
            return {
                name: {
                    "description": p.description,
                    "engine": p.engine,
                    "default_repo": p.default_repo,
                    "has_custom_executor": p.has_custom_executor,
                }
                for name, p in pm.personas.items()
            }

        @router.get("/personas/{name}")
        async def get_persona_detail(name: str) -> dict:
            p = pm.personas.get(name)
            if not p:
                raise HTTPException(
                    status_code=404, detail=f"Persona '{name}' not found"
                )
            return {
                "name": p.name,
                "description": p.description,
                "engine": p.engine,
                "default_repo": p.default_repo,
                "prompt_file": p.prompt_file,
                "has_custom_executor": p.has_custom_executor,
                "tool_dir": str(p.tool_dir),
            }

        @router.get("/personas/{name}/prompt")
        async def get_persona_prompt(name: str):
            p = pm.personas.get(name)
            if not p:
                raise HTTPException(
                    status_code=404, detail=f"Persona '{name}' not found"
                )
            return PlainTextResponse(p.prompt_content)

        # ── Tool endpoints ───────────────────────────────────────────────────

        @router.get("/{tool_name}")
        async def describe_tool(tool_name: str, request: Request) -> dict:
            _require_tool_scope(request, tool_name)
            p = pm.tools.get(tool_name)
            if p:
                required_secrets = [s for s in p.secrets if _is_replace_secret(s)]
                if required_secrets:
                    resolved = await _resolve_secrets(required_secrets)
                    if len(resolved) < len(required_secrets):
                        raise HTTPException(
                            status_code=404,
                            detail=f"Tool '{tool_name}' is not available (missing secrets)",
                        )
            return pm.describe_tool(tool_name)

        @router.post("/{tool_name}/{method_name}")
        async def call_tool(tool_name: str, method_name: str, request: Request):
            raw_body = await request.body()
            body: dict[str, Any] = {}
            if raw_body:
                try:
                    body = json.loads(raw_body)
                except json.JSONDecodeError as exc:
                    raise HTTPException(
                        status_code=400, detail="Request body must be valid JSON"
                    ) from exc
                if not isinstance(body, dict):
                    raise HTTPException(
                        status_code=400, detail="Request body must be a JSON object"
                    )
            _require_tool_scope(request, tool_name)
            accept = request.headers.get("accept", "")
            want_toon = "text/plain" in accept
            fmt = "toon" if want_toon else "json"
            result = await pm.call_tool(
                tool_name, method_name, body, request=request, format=fmt
            )
            if want_toon:
                return PlainTextResponse(
                    result if isinstance(result, str) else _to_toon(result)
                )
            return {"tool": tool_name, "method": method_name, "result": result}

        return router
