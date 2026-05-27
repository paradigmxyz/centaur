"""Render iron-proxy YAML config from tool secret declarations.

Centralizes what was previously split between firewall-manager (rendering) and
tool_manager (injection map). The API server owns iron-proxy's full config:

- ``secrets`` transform — one entry per ``HttpSecret``; replace-mode entries
  swap a placeholder, inject-mode entries set the header from ``source``.
- ``gcp_auth`` transform — one entry per unique ``GcpAuthSecret`` keyfile,
  each scoped to that secret's ``hosts`` and OAuth2 ``scopes``. Superseded by
  ``oauth_token``; kept until tools migrate off the ``gcp_auth`` secret type.
- ``oauth_token`` transform — one ``tokens`` entry per ``OAuthTokenSecret``,
  minting OAuth2 access tokens for the declared grant.
- ``hmac_sign`` transforms — one per unique ``HmacSignSecret`` signing scheme,
  HMAC-signing each request and injecting the configured headers.
- top-level ``postgres:`` — one listener per ``PgDsnSecret`` on sequential ports
  starting at 5432, ordered by name.
"""

from __future__ import annotations

import os
from dataclasses import astuple, replace
from pathlib import Path
from typing import Any

import yaml

from api.tool_manager import (
    BrokeredTokenSecret,
    GcpAuthSecret,
    HmacSignSecret,
    HttpSecret,
    OAuthFieldSource,
    OAuthTokenSecret,
    PgDsnSecret,
    SecretDef,
    SecretMode,
)


BASE_CONFIG_PATH = Path(__file__).parent / "iron-proxy.base.yaml"


def load_base_config() -> str:
    """Read the bundled iron-proxy base config (allowlist, header_allowlist, tls, …)."""
    return BASE_CONFIG_PATH.read_text()


GCP_AUTH_SCOPES: tuple[str, ...] = ("https://www.googleapis.com/auth/cloud-platform",)
GCP_AUTH_HOSTS: tuple[str, ...] = ("*.googleapis.com",)

PG_LISTEN_PORT_BASE = 5432

_MANAGED_TRANSFORMS: frozenset[str] = frozenset(
    {"secrets", "gcp_auth", "oauth_token", "hmac_sign"}
)

# Iron-proxy ``source`` schema for resolving secret values. ``env`` reads the
# referenced env var on the iron-proxy container; the 1Password variants resolve
# a deterministic ``op://vault/<secret_ref>/credential`` path through the
# respective SDK.
_OP_REF_SOURCES: dict[str, str] = {
    "onepassword": "1password",
    "onepassword-connect": "1password_connect",
}


def _secret_source_kind() -> str:
    return os.environ.get("FIREWALL_MANAGER_SECRET_SOURCE", "env").strip().lower()


def _secret_ttl() -> str:
    return os.environ.get("FIREWALL_MANAGER_SECRET_TTL", "10m").strip()


def _token_broker_ttl() -> str:
    """How long iron-proxy may cache a broker-issued token before re-fetching.

    Independent of ``FIREWALL_MANAGER_SECRET_TTL`` because the broker already
    expires tokens server-side; this controls only the proxy-side cache. Must
    be strictly less than the broker's token lifetime or iron-proxy rejects
    the response (its remaining lifetime must exceed the cache TTL).
    """
    return os.environ.get("FIREWALL_MANAGER_TOKEN_BROKER_TTL", "1m").strip()


def _op_vault() -> str:
    return os.environ.get("OP_VAULT", "ai-agents").strip()


def _build_source(secret_ref: str) -> dict[str, str]:
    iron_proxy_type = _OP_REF_SOURCES.get(_secret_source_kind())
    if iron_proxy_type is not None:
        return {
            "type": iron_proxy_type,
            "secret_ref": f"op://{_op_vault()}/{secret_ref}/credential",
            "ttl": _secret_ttl(),
        }
    return {"type": "env", "var": secret_ref}


def assign_pg_listen_ports(secrets: list[SecretDef]) -> dict[str, int]:
    """Allocate listen ports for ``PgDsnSecret`` entries.

    Deterministic: sort by ``name`` and assign ``PG_LISTEN_PORT_BASE``,
    ``PG_LISTEN_PORT_BASE + 1``, .... Same logic in any caller produces the
    same mapping.
    """
    names = sorted({s.name for s in secrets if isinstance(s, PgDsnSecret)})
    return {name: PG_LISTEN_PORT_BASE + idx for idx, name in enumerate(names)}


def _secret_action_block(secret: HttpSecret) -> tuple[str, dict[str, Any]]:
    """Return the ``replace``/``inject`` block iron-proxy expects for *secret*.

    Replace mode emits the ``proxy_value`` placeholder plus the scan locations
    (``match_headers``, optional ``match_path``, optional ``match_query``).
    Inject mode emits the target iron-proxy writes itself — a header (with an
    optional Go-template ``formatter``) or a query parameter.
    """
    if secret.mode is SecretMode.REPLACE:
        block: dict[str, Any] = {
            "proxy_value": secret.replacer,
            "match_headers": list(secret.match_headers),
        }
        if secret.match_path:
            block["match_path"] = True
        if secret.match_query:
            block["match_query"] = True
        return "replace", block
    if secret.inject_query_param:
        return "inject", {"query_param": secret.inject_query_param}
    block = {"header": secret.inject_header}
    if secret.inject_formatter:
        block["formatter"] = secret.inject_formatter
    return "inject", block


def _build_secret_transform(
    secrets: list[SecretDef],
) -> dict[str, Any] | None:
    """``secrets`` transform: one entry per HttpSecret, with its host rules.

    Entries are keyed by the ``HttpSecret`` minus its hosts, so two
    declarations of the same secret on different hosts get their rules merged,
    while genuinely distinct secrets stay separate.

    ``BrokeredTokenSecret`` entries also land here — each becomes an
    inject-mode entry sourced from ``token_broker`` so the broker mints the
    access token and iron-proxy injects it as ``Authorization: Bearer``.
    """
    by_secret: dict[HttpSecret, set[str]] = {}
    for secret in secrets:
        if not isinstance(secret, HttpSecret):
            continue
        key = replace(secret, hosts=())
        by_secret.setdefault(key, set()).update(secret.hosts)

    entries: list[dict[str, Any]] = []
    for secret, host_set in sorted(by_secret.items(), key=lambda kv: astuple(kv[0])):
        action, block = _secret_action_block(secret)
        entries.append(
            {
                "source": _build_source(secret.secret_ref),
                action: block,
                "rules": [{"host": h} for h in sorted(host_set)],
            }
        )

    entries.extend(_build_token_broker_entries(secrets))

    if not entries:
        return None
    return {"name": "secrets", "config": {"secrets": entries}}


def _build_token_broker_entries(
    secrets: list[SecretDef],
) -> list[dict[str, Any]]:
    """One ``secrets`` entry per ``BrokeredTokenSecret``.

    Hosts declared across multiple secrets with the same name are unioned
    into a single entry so the broker only sees one ``credential_id`` per
    refresh family.
    """
    by_name: dict[str, set[str]] = {}
    for secret in secrets:
        if isinstance(secret, BrokeredTokenSecret):
            by_name.setdefault(secret.name, set()).update(secret.hosts)

    ttl = _token_broker_ttl()
    entries: list[dict[str, Any]] = []
    for name in sorted(by_name):
        entries.append(
            {
                "source": {
                    "type": "token_broker",
                    "credential_id": name,
                    "ttl": ttl,
                },
                # Double-brace template is what iron-proxy's secrets transform
                # expects; the broker resolver exposes ``.Value`` as the
                # current access token.
                "inject": {
                    "header": "Authorization",
                    "formatter": "Bearer {{.Value}}",
                },
                "rules": [{"host": h} for h in sorted(by_name[name])],
            }
        )
    return entries


def _build_gcp_auth_transforms(
    secrets: list[SecretDef],
) -> list[dict[str, Any]]:
    """``gcp_auth`` transforms: one per unique ``GcpAuthSecret`` keyfile.

    Each keyfile gets its own transform scoped to that secret's ``hosts`` and
    OAuth2 ``scopes``, so multiple GCP service accounts can coexist — iron-proxy
    routes each request to the right keyfile by host. Secrets sharing a
    ``secret_ref`` are merged into one transform with the union of their hosts
    and scopes. A secret with no ``hosts`` falls back to ``GCP_AUTH_HOSTS``;
    with no ``scopes``, to ``GCP_AUTH_SCOPES``.
    """
    by_ref: dict[str, dict[str, set[str]]] = {}
    for secret in secrets:
        if not isinstance(secret, GcpAuthSecret):
            continue
        agg = by_ref.setdefault(secret.secret_ref, {"hosts": set(), "scopes": set()})
        agg["hosts"].update(secret.hosts)
        agg["scopes"].update(secret.scopes)

    transforms: list[dict[str, Any]] = []
    for secret_ref, agg in sorted(by_ref.items()):
        hosts = sorted(agg["hosts"]) if agg["hosts"] else list(GCP_AUTH_HOSTS)
        scopes = sorted(agg["scopes"]) if agg["scopes"] else list(GCP_AUTH_SCOPES)
        transforms.append(
            {
                "name": "gcp_auth",
                "config": {
                    "keyfile": _build_source(secret_ref),
                    "scopes": scopes,
                    "rules": [{"host": h} for h in hosts],
                },
            }
        )
    return transforms


def _build_field_source(field: OAuthFieldSource) -> dict[str, Any]:
    """Source object for one ``oauth_token`` credential field.

    Resolves the secret like any other source, then appends ``json_key`` when
    the field is pulled out of a JSON-encoded secret.
    """
    source = _build_source(field.secret_ref)
    if field.json_key is not None:
        source["json_key"] = field.json_key
    return source


def _build_oauth_token_transform(
    secrets: list[SecretDef],
) -> dict[str, Any] | None:
    """``oauth_token`` transform: one ``tokens`` entry per ``OAuthTokenSecret``.

    Mirrors ``secrets`` — a single transform whose ``config.tokens`` is a
    list. Entries that mint the same token (same grant, credential fields and
    token_endpoint) are merged, unioning their hosts and scopes. Optional
    fields are omitted when unset so the rendered config stays minimal.
    """
    by_token: dict[
        tuple[
            str,
            tuple[tuple[str, OAuthFieldSource], ...],
            str | None,
            tuple[tuple[str, OAuthFieldSource], ...],
            str | None,
        ],
        dict[str, set[str]],
    ] = {}
    for secret in secrets:
        if not isinstance(secret, OAuthTokenSecret):
            continue
        key = (
            secret.grant,
            secret.fields,
            secret.token_endpoint,
            secret.token_endpoint_headers,
            secret.audience,
        )
        agg = by_token.setdefault(key, {"hosts": set(), "scopes": set()})
        agg["hosts"].update(secret.hosts)
        agg["scopes"].update(secret.scopes)

    if not by_token:
        return None

    def _sort_key(
        k: tuple[
            str,
            tuple[tuple[str, OAuthFieldSource], ...],
            str | None,
            tuple[tuple[str, OAuthFieldSource], ...],
            str | None,
        ],
    ) -> tuple[str, ...]:
        grant, fields, token_endpoint, endpoint_headers, audience = k
        # None sorts before any string; normalize None to "" so mixed
        # None/str keys stay comparable.
        return (
            grant,
            token_endpoint or "",
            audience or "",
            tuple(name for name, _ in fields),
            tuple(name for name, _ in endpoint_headers),
        )

    tokens: list[dict[str, Any]] = []
    for key in sorted(by_token, key=_sort_key):
        grant, fields, token_endpoint, endpoint_headers, audience = key
        agg = by_token[key]
        entry: dict[str, Any] = {"grant": grant}
        for field_name, field_source in fields:
            entry[field_name] = _build_field_source(field_source)
        entry["rules"] = [{"host": h} for h in sorted(agg["hosts"])]
        if agg["scopes"]:
            entry["scopes"] = sorted(agg["scopes"])
        if token_endpoint is not None:
            entry["token_endpoint"] = token_endpoint
        if audience is not None:
            entry["audience"] = audience
        if endpoint_headers:
            entry["token_endpoint_headers"] = {
                header_name: _build_field_source(source)
                for header_name, source in endpoint_headers
            }
        tokens.append(entry)
    return {"name": "oauth_token", "config": {"tokens": tokens}}


def _build_hmac_sign_transforms(
    secrets: list[SecretDef],
) -> list[dict[str, Any]]:
    """``hmac_sign`` transforms: one per unique signing config.

    Each signing scheme (algorithm, encodings, message, timestamp format,
    credentials, header layout) becomes its own transform. Two ``HmacSignSecret``
    entries that differ only in ``hosts`` are merged — their host rules are
    unioned so the same scheme covers every upstream that opted in.
    """
    by_scheme: dict[
        tuple[
            str,
            str,
            str,
            str,
            str,
            bool,
            tuple[tuple[str, OAuthFieldSource], ...],
            tuple[tuple[str, str], ...],
        ],
        set[str],
    ] = {}
    for secret in secrets:
        if not isinstance(secret, HmacSignSecret):
            continue
        key = (
            secret.algorithm,
            secret.key_encoding,
            secret.output_encoding,
            secret.message,
            secret.timestamp_format,
            secret.allow_chunked_body,
            secret.credentials,
            tuple((h.name, h.value) for h in secret.headers),
        )
        by_scheme.setdefault(key, set()).update(secret.hosts)

    transforms: list[dict[str, Any]] = []
    for key in sorted(by_scheme, key=lambda k: (k[0], k[3], tuple(n for n, _ in k[6]))):
        (
            algorithm,
            key_encoding,
            output_encoding,
            message,
            timestamp_format,
            allow_chunked_body,
            credentials,
            headers,
        ) = key
        hosts = sorted(by_scheme[key])
        config: dict[str, Any] = {
            "timestamp": {"format": timestamp_format},
            "signature": {
                "algorithm": algorithm,
                "key_encoding": key_encoding,
                "output_encoding": output_encoding,
                "message": message,
            },
            "credentials": {
                name: _build_field_source(source) for name, source in credentials
            },
            "headers": [{"name": n, "value": v} for n, v in headers],
            "rules": [{"host": h} for h in hosts],
        }
        if allow_chunked_body:
            config["allow_chunked_body"] = True
        transforms.append({"name": "hmac_sign", "config": config})
    return transforms


def _build_postgres_listeners(
    secrets: list[SecretDef],
    pg_listen_ports: dict[str, int],
) -> list[dict[str, Any]]:
    """Top-level ``postgres:`` list: one listener per unique PgDsnSecret."""
    by_name: dict[str, PgDsnSecret] = {}
    for secret in secrets:
        if isinstance(secret, PgDsnSecret):
            by_name.setdefault(secret.name, secret)

    listeners: list[dict[str, Any]] = []
    for name in sorted(by_name):
        port = pg_listen_ports.get(name)
        if port is None:
            continue
        secret = by_name[name]
        # iron-proxy resolves the upstream DSN itself (env or 1Password,
        # matching the configured secret source). The kubernetes backend only
        # injects the proxy-side password env var so the sandbox can auth.
        listeners.append(
            {
                "name": name.lower(),
                "listen": f"0.0.0.0:{port}",
                "upstream": {"dsn": _build_source(secret.secret_ref)},
                "client": {
                    "user": "app_user",
                    "password_env": f"PG_PROXY_PASSWORD_{name}",
                },
            }
        )
    return listeners


def render_proxy_yaml(
    secrets: list[SecretDef],
    base_config: str | None = None,
    *,
    pg_listen_ports: dict[str, int] | None = None,
) -> str:
    """Splice managed transforms + postgres listeners into ``base_config`` YAML.

    ``base_config`` is the seed config from ``services/iron-proxy/iron-proxy.yaml``
    (allowlist, header_allowlist, dns, proxy, management, tls, log). Managed
    transforms (``secrets``, ``gcp_auth``, ``oauth_token``) are inserted before
    ``header_allowlist``; existing managed entries are replaced. The top-level
    ``postgres:`` list is overwritten.
    """
    if base_config is None:
        base_config = load_base_config()
    cfg = yaml.safe_load(base_config) or {}
    if pg_listen_ports is None:
        pg_listen_ports = assign_pg_listen_ports(secrets)

    transforms = [
        t for t in (cfg.get("transforms") or []) if (t or {}).get("name") not in _MANAGED_TRANSFORMS
    ]
    new_transforms = [
        t for t in (_build_secret_transform(secrets),) if t is not None
    ]
    new_transforms.extend(_build_gcp_auth_transforms(secrets))
    oauth_token = _build_oauth_token_transform(secrets)
    if oauth_token is not None:
        new_transforms.append(oauth_token)
    new_transforms.extend(_build_hmac_sign_transforms(secrets))
    if new_transforms:
        for index, transform in enumerate(transforms):
            if (transform or {}).get("name") == "header_allowlist":
                transforms[index:index] = new_transforms
                break
        else:
            transforms.extend(new_transforms)
    cfg["transforms"] = transforms

    listeners = _build_postgres_listeners(secrets, pg_listen_ports)
    if listeners:
        cfg["postgres"] = listeners
    else:
        cfg.pop("postgres", None)

    return yaml.safe_dump(cfg, sort_keys=False)
