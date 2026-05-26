from __future__ import annotations

import pytest
import yaml

from api.broker_config import (
    BROKER_BEARER_AUTH_ENV,
    DEFAULT_BROKER_LISTEN_PORT,
    DEFAULT_BROKER_METRICS_PORT,
    collect_broker_credentials,
    render_broker_yaml,
)
from api.tool_manager import (
    BrokeredTokenSecret,
    HttpSecret,
    OAuthFieldSource,
    OAuthTokenSecret,
)


_FIELDS = (
    ("client_id", OAuthFieldSource("CODEX_CLIENT_ID")),
    ("refresh_token", OAuthFieldSource("CODEX_BLOB")),
)
_FIELDS_WITH_SECRET = (
    ("client_id", OAuthFieldSource("OKTA_CLIENT_ID")),
    ("client_secret", OAuthFieldSource("OKTA_CLIENT_SECRET")),
    ("refresh_token", OAuthFieldSource("OKTA_BLOB")),
)


def test_collect_broker_credentials_dedupes_by_name() -> None:
    secrets = [
        BrokeredTokenSecret(
            "codex", ("auth.openai.com",), _FIELDS,
            token_endpoint="https://auth.openai.com/oauth/token",
        ),
        BrokeredTokenSecret(
            "codex", ("api.openai.com",), _FIELDS,
            token_endpoint="https://auth.openai.com/oauth/token",
        ),
        BrokeredTokenSecret(
            "okta", ("h",), _FIELDS_WITH_SECRET,
            token_endpoint="https://example.okta.com/token",
        ),
    ]
    collected = collect_broker_credentials(secrets)
    assert [c.name for c in collected] == ["codex", "okta"]


def test_collect_broker_credentials_ignores_oauth_token_secrets() -> None:
    # OAuthTokenSecret with refresh_token grant is *not* a brokered secret
    # — tools must opt in via the dedicated type.
    secrets = [
        OAuthTokenSecret(
            "codex", "refresh_token", ("h",),
            (
                ("client_id", OAuthFieldSource("A")),
                ("refresh_token", OAuthFieldSource("B", "refresh_token")),
            ),
            token_endpoint="https://h/token",
        ),
    ]
    assert collect_broker_credentials(secrets) == []


def test_render_broker_yaml_empty_secrets_emits_valid_skeleton(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "onepassword")
    rendered = render_broker_yaml([])
    cfg = yaml.safe_load(rendered)
    assert cfg["listen"] == f":{DEFAULT_BROKER_LISTEN_PORT}"
    assert cfg["metrics_listen"] == f":{DEFAULT_BROKER_METRICS_PORT}"
    assert cfg["bearer_auth_env"] == BROKER_BEARER_AUTH_ENV
    assert cfg["log"] == {"level": "info", "format": "json"}
    assert cfg["credentials"] == []


def test_render_broker_yaml_emits_credential(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "onepassword")
    monkeypatch.setenv("OP_VAULT", "ai-agents")
    secrets = [
        BrokeredTokenSecret(
            name="openai-codex",
            hosts=("auth.openai.com",),
            fields=_FIELDS,
            token_endpoint="https://auth.openai.com/oauth/token",
        ),
    ]
    cfg = yaml.safe_load(render_broker_yaml(secrets))
    assert len(cfg["credentials"]) == 1
    cred = cfg["credentials"][0]
    assert cred["id"] == "openai-codex"
    assert cred["token_endpoint"] == "https://auth.openai.com/oauth/token"
    assert cred["client_id"] == {
        "type": "1password",
        "secret_ref": "op://ai-agents/CODEX_CLIENT_ID/credential",
    }
    assert cred["store"] == {
        "type": "1password",
        "secret_ref": "op://ai-agents/CODEX_BLOB/credential",
    }
    assert "client_secret" not in cred
    assert "scopes" not in cred


def test_render_broker_yaml_includes_client_secret_and_scopes(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "onepassword-connect")
    monkeypatch.setenv("OP_VAULT", "ai-agents")
    secrets = [
        BrokeredTokenSecret(
            name="okta",
            hosts=("example.okta.com",),
            fields=_FIELDS_WITH_SECRET,
            scopes=("openid", "offline_access"),
            token_endpoint="https://example.okta.com/oauth2/default/v1/token",
        ),
    ]
    cfg = yaml.safe_load(render_broker_yaml(secrets))
    cred = cfg["credentials"][0]
    assert cred["client_secret"] == {
        "type": "1password_connect",
        "secret_ref": "op://ai-agents/OKTA_CLIENT_SECRET/credential",
    }
    assert cred["store"]["type"] == "1password_connect"
    assert cred["scopes"] == ["openid", "offline_access"]


def test_render_broker_yaml_rejects_env_source(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "env")
    secrets = [
        BrokeredTokenSecret(
            name="codex", hosts=("h",),
            fields=_FIELDS,
            token_endpoint="https://h/token",
        ),
    ]
    with pytest.raises(ValueError, match="iron-token-broker store"):
        render_broker_yaml(secrets)


def test_render_broker_yaml_requires_token_endpoint(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "onepassword")
    secrets = [
        BrokeredTokenSecret(
            name="codex", hosts=("h",), fields=_FIELDS,
        ),
    ]
    with pytest.raises(ValueError, match="token_endpoint"):
        render_broker_yaml(secrets)


def test_render_broker_yaml_forwards_json_key_on_read_fields(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "onepassword")
    monkeypatch.setenv("OP_VAULT", "ai-agents")
    secrets = [
        BrokeredTokenSecret(
            name="okta",
            hosts=("example.okta.com",),
            fields=(
                ("client_id", OAuthFieldSource("OKTA_BUNDLE", "client_id")),
                ("client_secret", OAuthFieldSource("OKTA_BUNDLE", "client_secret")),
                ("refresh_token", OAuthFieldSource("OKTA_BLOB")),
            ),
            token_endpoint="https://example.okta.com/oauth2/default/v1/token",
        ),
    ]
    cfg = yaml.safe_load(render_broker_yaml(secrets))
    cred = cfg["credentials"][0]
    assert cred["client_id"] == {
        "type": "1password",
        "secret_ref": "op://ai-agents/OKTA_BUNDLE/credential",
        "json_key": "client_id",
    }
    assert cred["client_secret"] == {
        "type": "1password",
        "secret_ref": "op://ai-agents/OKTA_BUNDLE/credential",
        "json_key": "client_secret",
    }
    # Store never carries a json_key — the broker writes the whole blob.
    assert cred["store"] == {
        "type": "1password",
        "secret_ref": "op://ai-agents/OKTA_BLOB/credential",
    }


def test_render_broker_yaml_emits_token_endpoint_headers(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "onepassword")
    monkeypatch.setenv("OP_VAULT", "ai-agents")
    secrets = [
        BrokeredTokenSecret(
            name="venue",
            hosts=("api.venue.example",),
            fields=_FIELDS,
            token_endpoint="https://venue.example/oauth/token",
            token_endpoint_headers=(
                ("x-api-key", OAuthFieldSource("VENUE_API_KEY")),
            ),
        ),
    ]
    cfg = yaml.safe_load(render_broker_yaml(secrets))
    cred = cfg["credentials"][0]
    assert cred["token_endpoint_headers"] == {
        "x-api-key": {
            "type": "1password",
            "secret_ref": "op://ai-agents/VENUE_API_KEY/credential",
        }
    }


def test_render_broker_yaml_omits_token_endpoint_headers_when_empty(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "onepassword")
    secrets = [
        BrokeredTokenSecret(
            name="codex", hosts=("h",), fields=_FIELDS,
            token_endpoint="https://h/token",
        ),
    ]
    cfg = yaml.safe_load(render_broker_yaml(secrets))
    assert "token_endpoint_headers" not in cfg["credentials"][0]


def test_render_broker_yaml_skips_non_brokered_secrets(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "onepassword")
    secrets = [
        HttpSecret("API_KEY", "API_KEY", hosts=("h",), match_headers=("Auth",)),
        OAuthTokenSecret(
            name="legacy", grant="refresh_token", hosts=("h",),
            fields=(
                ("client_id", OAuthFieldSource("A")),
                ("refresh_token", OAuthFieldSource("B", "refresh_token")),
            ),
            token_endpoint="https://h/token",
        ),
        BrokeredTokenSecret(
            name="codex", hosts=("h",), fields=_FIELDS,
            token_endpoint="https://h/token",
        ),
    ]
    cfg = yaml.safe_load(render_broker_yaml(secrets))
    assert [c["id"] for c in cfg["credentials"]] == ["codex"]
