"""Render iron-token-broker YAML from ``BrokeredTokenSecret`` entries.

The broker is a separate process that owns the OAuth refresh-token state for
one or more credentials. iron-proxy fetches the current access token from it
over HTTP, so the refresh family is never touched concurrently by multiple
proxies.

One ``credentials`` entry per registered ``BrokeredTokenSecret``:

- ``id`` = ``secret.name`` (also the ``credential_id`` iron-proxy uses on the
  ``token_broker`` source side).
- ``token_endpoint`` = the secret's declared endpoint; required because the
  broker has to know where to POST the refresh.
- ``client_id`` / ``client_secret`` resolve through the same secret source the
  proxy uses (1password / 1password_connect).
- ``store`` points at the writable blob holding the refresh token; we reuse
  ``fields["refresh_token"].secret_ref`` so the operator only bootstraps the
  blob in one place.

Tools that need a coordinated refresh-token rotation opt in by declaring
``type = "brokered_token"`` instead of ``type = "oauth_token"``.
"""

from __future__ import annotations

import os
from typing import Any

import yaml

from api.tool_manager import (
    BrokeredTokenSecret,
    OAuthFieldSource,
    SecretDef,
)


# Broker HTTP API port. Matches iron-token-broker.example.yaml.
DEFAULT_BROKER_LISTEN_PORT = 8181
DEFAULT_BROKER_METRICS_PORT = 9091

# Env var the broker reads its bearer auth from. iron-proxy reads the same
# token from ``IRON_BROKER_TOKEN`` on its end.
BROKER_BEARER_AUTH_ENV = "IRON_BROKER_TOKEN"


# Per the broker README: ``env`` is read-only and cannot back the store. For
# every other source the proxy already supports, the broker accepts the same
# shape on the store side.
_STORE_SOURCES: dict[str, str] = {
    "onepassword": "1password",
    "onepassword-connect": "1password_connect",
}

# Read-side sources for ``client_id`` / ``client_secret``. ``env`` is allowed
# here even when it isn't for the store: the read side never writes.
_READ_SOURCES: dict[str, str] = {
    "onepassword": "1password",
    "onepassword-connect": "1password_connect",
}


def _secret_source_kind() -> str:
    return os.environ.get("FIREWALL_MANAGER_SECRET_SOURCE", "env").strip().lower()


def _op_vault() -> str:
    return os.environ.get("OP_VAULT", "ai-agents").strip()


def _build_read_source(field: OAuthFieldSource) -> dict[str, Any]:
    """Source object for a read-only credential field.

    Used for ``client_id``, ``client_secret``, and ``token_endpoint_headers``
    entries. Mirrors ``proxy_config._build_source`` so a deployment configured
    for 1Password resolves identical refs on both the proxy and broker sides.
    ``json_key`` is forwarded when set so operators can keep a single JSON
    blob in their store and pluck out individual values.
    """
    kind = _secret_source_kind()
    iron_proxy_type = _READ_SOURCES.get(kind)
    if iron_proxy_type is not None:
        source: dict[str, Any] = {
            "type": iron_proxy_type,
            "secret_ref": f"op://{_op_vault()}/{field.secret_ref}/credential",
        }
    else:
        source = {"type": "env", "var": field.secret_ref}
    if field.json_key is not None:
        source["json_key"] = field.json_key
    return source


def _build_store_source(field: OAuthFieldSource) -> dict[str, Any]:
    """Source object for the writable credential blob (``store``).

    The broker writes the rotated refresh-token blob back to this source on
    every refresh; ``env`` is rejected because environment variables are
    read-only at the process level.
    """
    kind = _secret_source_kind()
    iron_proxy_type = _STORE_SOURCES.get(kind)
    if iron_proxy_type is None:
        raise ValueError(
            f"iron-token-broker store cannot use secret source {kind!r}; "
            "configure FIREWALL_MANAGER_SECRET_SOURCE=onepassword or "
            "onepassword-connect (refresh tokens must be writable)"
        )
    return {
        "type": iron_proxy_type,
        "secret_ref": f"op://{_op_vault()}/{field.secret_ref}/credential",
    }


def _credential_entry(secret: BrokeredTokenSecret) -> dict[str, Any]:
    fields = dict(secret.fields)
    if secret.token_endpoint is None:
        raise ValueError(
            f"brokered_token entry {secret.name!r} requires 'token_endpoint'"
        )
    # Parser already enforces required fields; defensive lookups stay terse.
    entry: dict[str, Any] = {
        "id": secret.name,
        "token_endpoint": secret.token_endpoint,
        "client_id": _build_read_source(fields["client_id"]),
        "store": _build_store_source(fields["refresh_token"]),
    }
    if "client_secret" in fields:
        entry["client_secret"] = _build_read_source(fields["client_secret"])
    if secret.scopes:
        entry["scopes"] = list(secret.scopes)
    if secret.token_endpoint_headers:
        # Extra headers sent on the broker's token POST itself, for IdPs that
        # require an API key alongside the standard form-body client auth.
        entry["token_endpoint_headers"] = {
            header_name: _build_read_source(source)
            for header_name, source in secret.token_endpoint_headers
        }
    return entry


def collect_broker_credentials(
    secrets: list[SecretDef],
) -> list[BrokeredTokenSecret]:
    """Return one ``BrokeredTokenSecret`` per name, deduped.

    Two declarations of the same secret on different hosts share a single
    broker credential entry; the broker doesn't know about hosts at all (the
    proxy's ``token_broker`` source carries those rules).
    """
    by_name: dict[str, BrokeredTokenSecret] = {}
    for secret in secrets:
        if isinstance(secret, BrokeredTokenSecret) and secret.name not in by_name:
            by_name[secret.name] = secret
    return [by_name[name] for name in sorted(by_name)]


def render_broker_yaml(secrets: list[SecretDef]) -> str:
    """Render an iron-token-broker.yaml from ``BrokeredTokenSecret`` entries.

    Returns a YAML document with ``listen``/``metrics_listen``/``log``/
    ``credentials`` keys. An empty credentials list is valid: the broker
    happily starts with zero credentials and the HTTP API returns 404 for
    every request, which is what we want until the first tool registers a
    brokered-token secret.
    """
    credentials = [_credential_entry(s) for s in collect_broker_credentials(secrets)]
    cfg = {
        "listen": f":{DEFAULT_BROKER_LISTEN_PORT}",
        "metrics_listen": f":{DEFAULT_BROKER_METRICS_PORT}",
        "bearer_auth_env": BROKER_BEARER_AUTH_ENV,
        "log": {"level": "info", "format": "json"},
        "credentials": credentials,
    }
    return yaml.safe_dump(cfg, sort_keys=False)
