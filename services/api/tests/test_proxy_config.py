from __future__ import annotations

import pytest
import yaml

from api.proxy_config import (
    PG_LISTEN_PORT_BASE,
    assign_pg_listen_ports,
    render_proxy_yaml,
)
from api.tool_manager import (
    DEFAULT_MATCH_HEADERS,
    GcpAuthSecret,
    HttpSecret,
    OAuthFieldSource,
    OAuthTokenSecret,
    PgDsnSecret,
    SecretMode,
    _parse_secret,
    _parse_secrets,
)


# ── parser ──────────────────────────────────────────────────────────────────


def test_parser_accepts_string_for_back_compat() -> None:
    secret = _parse_secret("OPENAI_API_KEY")
    assert isinstance(secret, HttpSecret)
    assert secret.name == "OPENAI_API_KEY"
    assert secret.secret_ref == "OPENAI_API_KEY"
    assert secret.replacer == "OPENAI_API_KEY"
    # The legacy shim is the only path that falls back to the blanket header set.
    assert secret.mode is SecretMode.REPLACE
    assert secret.match_headers == DEFAULT_MATCH_HEADERS


def test_parser_typed_header_replace_mode() -> None:
    secret = _parse_secret(
        {
            "type": "header",
            "name": "CUSTOM_KEY",
            "replacer": "PLACEHOLDER",
            "secret_ref": "OP_REF",
            "match_headers": ["Authorization"],
        }
    )
    assert isinstance(secret, HttpSecret)
    assert secret.mode is SecretMode.REPLACE
    assert secret.replacer == "PLACEHOLDER"
    assert secret.secret_ref == "OP_REF"
    assert secret.match_headers == ("Authorization",)


def test_parser_typed_header_defaults_replacer_and_ref_to_name() -> None:
    secret = _parse_secret(
        {"type": "header", "name": "API_KEY", "match_headers": ["Api-Key"]}
    )
    assert isinstance(secret, HttpSecret)
    assert secret.replacer == "API_KEY"
    assert secret.secret_ref == "API_KEY"
    assert secret.match_headers == ("Api-Key",)


def test_parser_replace_secret_accepts_query_and_path_locations() -> None:
    secret = _parse_secret(
        {
            "type": "header",
            "name": "ETHERSCAN_API_KEY",
            "match_query": True,
            "match_path": True,
        }
    )
    assert secret.mode is SecretMode.REPLACE
    assert secret.match_headers == ()
    assert secret.match_query is True
    assert secret.match_path is True


def test_parser_replace_secret_requires_a_scan_location() -> None:
    with pytest.raises(ValueError, match="must declare where iron-proxy scans"):
        _parse_secret({"type": "header", "name": "API_KEY"})


def test_parser_typed_header_rejects_non_string_match_headers() -> None:
    with pytest.raises(ValueError, match="invalid 'match_headers'"):
        _parse_secret({"type": "header", "name": "API_KEY", "match_headers": [1]})


def test_parser_typed_header_allows_empty_match_headers_with_match_query() -> None:
    secret = _parse_secret(
        {
            "type": "header",
            "name": "API_KEY",
            "match_headers": [],
            "match_query": True,
        }
    )
    assert isinstance(secret, HttpSecret)
    assert secret.match_headers == ()
    assert secret.match_query is True


def test_parser_replace_mode_rejects_inject_keys() -> None:
    with pytest.raises(ValueError, match="must not declare 'inject_header'"):
        _parse_secret(
            {
                "type": "header",
                "name": "API_KEY",
                "match_headers": ["Authorization"],
                "inject_header": "Authorization",
            }
        )


def test_parser_inject_mode_header_with_formatter() -> None:
    secret = _parse_secret(
        {
            "type": "header",
            "name": "VENDOR_TOKEN",
            "mode": "inject",
            "inject_header": "Authorization",
            "inject_formatter": "Bearer {{ .Value }}",
        }
    )
    assert secret.mode is SecretMode.INJECT
    assert secret.inject_header == "Authorization"
    assert secret.inject_formatter == "Bearer {{ .Value }}"
    # Inject-mode secrets carry no placeholder — iron-proxy sets the value.
    assert secret.replacer == ""


def test_parser_inject_mode_query_param() -> None:
    secret = _parse_secret(
        {
            "type": "header",
            "name": "VENDOR_KEY",
            "mode": "inject",
            "inject_query_param": "api_key",
        }
    )
    assert secret.mode is SecretMode.INJECT
    assert secret.inject_query_param == "api_key"


def test_parser_inject_mode_requires_exactly_one_target() -> None:
    with pytest.raises(ValueError, match="exactly one of"):
        _parse_secret({"type": "header", "name": "API_KEY", "mode": "inject"})


def test_parser_inject_mode_rejects_replace_keys() -> None:
    with pytest.raises(ValueError, match="must not declare 'replacer'"):
        _parse_secret(
            {
                "type": "header",
                "name": "API_KEY",
                "mode": "inject",
                "inject_header": "Authorization",
                "replacer": "PLACEHOLDER",
            }
        )


def test_parser_rejects_unknown_mode() -> None:
    with pytest.raises(ValueError, match="unknown mode"):
        _parse_secret(
            {
                "type": "header",
                "name": "API_KEY",
                "mode": "bogus",
                "match_headers": ["Authorization"],
            }
        )


def test_parser_typed_gcp_auth() -> None:
    secret = _parse_secret(
        {"type": "gcp_auth", "name": "GCP_GCLOUD_CREDENTIAL"}
    )
    assert isinstance(secret, GcpAuthSecret)
    assert secret.secret_ref == "GCP_GCLOUD_CREDENTIAL"
    assert secret.hosts == ()
    assert secret.scopes == ()


def test_parser_typed_gcp_auth_with_hosts_and_scopes() -> None:
    secret = _parse_secret(
        {
            "type": "gcp_auth",
            "name": "GSUITE_GCP_CREDENTIAL",
            "hosts": ["gmail.googleapis.com", "calendar.googleapis.com"],
            "scopes": ["https://www.googleapis.com/auth/gmail.readonly"],
        }
    )
    assert isinstance(secret, GcpAuthSecret)
    assert secret.hosts == ("gmail.googleapis.com", "calendar.googleapis.com")
    assert secret.scopes == ("https://www.googleapis.com/auth/gmail.readonly",)


def test_parser_gcp_auth_rejects_invalid_hosts() -> None:
    with pytest.raises(ValueError, match="'hosts' must be an array"):
        _parse_secret(
            {"type": "gcp_auth", "name": "GCP_X", "hosts": "storage.googleapis.com"}
        )


def test_parser_gcp_auth_rejects_invalid_scopes() -> None:
    with pytest.raises(ValueError, match="'scopes' must be an array"):
        _parse_secret({"type": "gcp_auth", "name": "GCP_X", "scopes": ["", "ok"]})


def test_parser_typed_pg_dsn() -> None:
    secret = _parse_secret(
        {
            "type": "pg_dsn",
            "name": "DATABASE_URL",
            "secret_ref": "INVESTMEMOS_PG",
            "database": "investmemos",
        }
    )
    assert isinstance(secret, PgDsnSecret)
    assert secret.name == "DATABASE_URL"
    assert secret.secret_ref == "INVESTMEMOS_PG"
    assert secret.database == "investmemos"


def test_parser_pg_dsn_requires_database() -> None:
    with pytest.raises(ValueError, match="requires a non-empty 'database'"):
        _parse_secret({"type": "pg_dsn", "name": "DATABASE_URL"})


def test_parser_rejects_unknown_type() -> None:
    with pytest.raises(ValueError, match="unknown secret type"):
        _parse_secret({"type": "bogus", "name": "X"})


def test_parser_rejects_missing_name() -> None:
    with pytest.raises(ValueError, match="missing 'name'"):
        _parse_secret({"type": "header"})


def test_parser_mixed_array() -> None:
    parsed = _parse_secrets(
        [
            "RAW_STRING",
            {"type": "pg_dsn", "name": "DATABASE_URL", "database": "memo_db"},
            {"type": "gcp_auth", "name": "GCP_GCLOUD_CREDENTIAL"},
        ]
    )
    assert [type(s).__name__ for s in parsed] == [
        "HttpSecret",
        "PgDsnSecret",
        "GcpAuthSecret",
    ]


def test_parser_header_secret_carries_hosts() -> None:
    secret = _parse_secret(
        {
            "type": "header",
            "name": "API_KEY",
            "match_headers": ["Authorization"],
            "hosts": ["api.example.com"],
        }
    )
    assert secret.hosts == ("api.example.com",)


def test_parser_header_secret_falls_back_to_default_hosts() -> None:
    secret = _parse_secret(
        {"type": "header", "name": "API_KEY", "match_headers": ["Authorization"]},
        default_hosts=("api.example.com",),
    )
    assert secret.hosts == ("api.example.com",)


def test_parser_raw_string_inherits_default_hosts() -> None:
    secret = _parse_secret("API_KEY", default_hosts=("api.example.com",))
    assert secret.hosts == ("api.example.com",)


def test_parser_header_secret_rejects_empty_hosts() -> None:
    with pytest.raises(ValueError, match="invalid 'hosts'"):
        _parse_secret(
            {
                "type": "header",
                "name": "API_KEY",
                "match_headers": ["Authorization"],
                "hosts": [],
            }
        )


# ── oauth_token parser ───────────────────────────────────────────────────────


_REFRESH_FIELDS = {
    "refresh_token": {"secret_ref": "GOOGLE_TOKEN_JSON", "json_key": "refresh_token"},
    "client_id": {"secret_ref": "GOOGLE_TOKEN_JSON", "json_key": "client_id"},
    "client_secret": {"secret_ref": "GOOGLE_TOKEN_JSON", "json_key": "client_secret"},
}

def test_parser_typed_oauth_token_refresh() -> None:
    secret = _parse_secret(
        {
            "type": "oauth_token",
            "grant": "refresh_token",
            "name": "GOOGLE_TOKEN_JSON",
            "hosts": ["gmail.googleapis.com"],
            "fields": _REFRESH_FIELDS,
        }
    )
    assert isinstance(secret, OAuthTokenSecret)
    assert secret.grant == "refresh_token"
    assert secret.hosts == ("gmail.googleapis.com",)
    assert secret.scopes == ()
    assert secret.token_endpoint is None
    # fields are sorted by name for deterministic rendering
    assert [name for name, _ in secret.fields] == [
        "client_id",
        "client_secret",
        "refresh_token",
    ]
    assert dict(secret.fields)["refresh_token"] == OAuthFieldSource(
        "GOOGLE_TOKEN_JSON", "refresh_token"
    )


def test_parser_oauth_token_field_accepts_bare_string() -> None:
    secret = _parse_secret(
        {
            "type": "oauth_token",
            "grant": "client_credentials",
            "name": "OAUTH_APP",
            "hosts": ["api.example.com"],
            "fields": {
                "client_id": "OAUTH_CLIENT_ID",
                "client_secret": "OAUTH_CLIENT_SECRET",
            },
        }
    )
    assert dict(secret.fields)["client_id"] == OAuthFieldSource("OAUTH_CLIENT_ID")
    assert dict(secret.fields)["client_id"].json_key is None


def test_parser_typed_oauth_token_client_credentials() -> None:
    secret = _parse_secret(
        {
            "type": "oauth_token",
            "grant": "client_credentials",
            "name": "OAUTH_APP",
            "hosts": ["api.example.com"],
            "scopes": ["read"],
            "token_endpoint": "https://login.example.com/oauth2/token",
            "fields": {
                "client_id": "OAUTH_CLIENT_ID",
                "client_secret": "OAUTH_CLIENT_SECRET",
            },
        }
    )
    assert isinstance(secret, OAuthTokenSecret)
    assert secret.grant == "client_credentials"
    assert secret.token_endpoint == "https://login.example.com/oauth2/token"
    assert secret.scopes == ("read",)


def test_parser_oauth_token_rejects_unknown_grant() -> None:
    with pytest.raises(ValueError, match="'grant' must be one of"):
        _parse_secret(
            {"type": "oauth_token", "grant": "magic", "name": "X", "hosts": ["h"]}
        )


def test_parser_oauth_token_requires_hosts() -> None:
    with pytest.raises(ValueError, match="'hosts' must be a non-empty array"):
        _parse_secret(
            {"type": "oauth_token", "grant": "refresh_token", "name": "X"}
        )


def test_parser_oauth_token_requires_fields() -> None:
    with pytest.raises(ValueError, match="'fields' must be a non-empty table"):
        _parse_secret(
            {
                "type": "oauth_token",
                "grant": "refresh_token",
                "name": "X",
                "hosts": ["h"],
            }
        )


def test_parser_oauth_token_rejects_missing_required_field() -> None:
    with pytest.raises(ValueError, match="requires fields \\['client_id'\\]"):
        _parse_secret(
            {
                "type": "oauth_token",
                "grant": "refresh_token",
                "name": "X",
                "hosts": ["h"],
                "fields": {"refresh_token": "RT"},
            }
        )


def test_parser_oauth_token_rejects_field_invalid_for_grant() -> None:
    with pytest.raises(ValueError, match="is not valid for grant 'refresh_token'"):
        _parse_secret(
            {
                "type": "oauth_token",
                "grant": "refresh_token",
                "name": "X",
                "hosts": ["h"],
                "fields": {
                    "refresh_token": "RT",
                    "client_id": "CID",
                    "private_key": "PK",
                },
            }
        )


# ── port allocation ─────────────────────────────────────────────────────────


def test_pg_listen_ports_are_sequential_and_sorted_by_name() -> None:
    secrets = [
        PgDsnSecret("ZEBRA", "ZEBRA", "z"),
        PgDsnSecret("ALPHA", "ALPHA", "a"),
        PgDsnSecret("MIKE", "MIKE", "m"),
    ]
    ports = assign_pg_listen_ports(secrets)
    assert ports == {
        "ALPHA": PG_LISTEN_PORT_BASE,
        "MIKE": PG_LISTEN_PORT_BASE + 1,
        "ZEBRA": PG_LISTEN_PORT_BASE + 2,
    }


def test_pg_listen_ports_deduplicates() -> None:
    secrets = [
        PgDsnSecret("DB", "DB", "db"),
        PgDsnSecret("DB", "DB", "db"),  # duplicate (from infra + tool)
    ]
    ports = assign_pg_listen_ports(secrets)
    assert ports == {"DB": PG_LISTEN_PORT_BASE}


# ── renderer ────────────────────────────────────────────────────────────────


def test_render_emits_header_and_gcp_auth_transforms(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "env")
    secrets = [
        HttpSecret(
            "OPENAI_API_KEY",
            "OPENAI_API_KEY",
            hosts=("api.openai.com",),
            match_headers=("Authorization",),
        ),
        GcpAuthSecret("GCP_GCLOUD_CREDENTIAL", "GCP_GCLOUD_CREDENTIAL"),
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    names = [t["name"] for t in cfg["transforms"]]
    assert names == ["allowlist", "secrets", "gcp_auth", "header_allowlist"]
    secrets_block = next(t for t in cfg["transforms"] if t["name"] == "secrets")
    entry = secrets_block["config"]["secrets"][0]
    assert "inject" not in entry
    assert entry["replace"]["proxy_value"] == "OPENAI_API_KEY"
    assert entry["replace"]["match_headers"] == ["Authorization"]
    assert "match_path" not in entry["replace"]
    assert "match_query" not in entry["replace"]
    assert entry["rules"] == [{"host": "api.openai.com"}]


def test_render_replace_secret_emits_query_and_path_locations(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "env")
    secrets = [
        HttpSecret(
            "ETHERSCAN_API_KEY",
            "ETHERSCAN_API_KEY",
            hosts=("api.etherscan.io",),
            match_query=True,
            match_path=True,
        ),
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    secrets_block = next(t for t in cfg["transforms"] if t["name"] == "secrets")
    entry = secrets_block["config"]["secrets"][0]
    replace_block = entry["replace"]
    assert replace_block["proxy_value"] == "ETHERSCAN_API_KEY"
    assert replace_block["match_headers"] == []
    assert replace_block["match_path"] is True
    assert replace_block["match_query"] is True
    assert entry["rules"] == [{"host": "api.etherscan.io"}]


def test_render_inject_mode_header_secret(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "env")
    secrets = [
        HttpSecret(
            "VENDOR_TOKEN",
            "VENDOR_TOKEN",
            mode=SecretMode.INJECT,
            hosts=("api.vendor.com",),
            inject_header="Authorization",
            inject_formatter="Bearer {{ .Value }}",
        ),
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    secrets_block = next(t for t in cfg["transforms"] if t["name"] == "secrets")
    entry = secrets_block["config"]["secrets"][0]
    assert "replace" not in entry
    assert entry["inject"] == {
        "header": "Authorization",
        "formatter": "Bearer {{ .Value }}",
    }
    assert entry["rules"] == [{"host": "api.vendor.com"}]


def test_render_gcp_auth_defaults_hosts_and_scopes_when_unset() -> None:
    secrets = [GcpAuthSecret("GCP_GCLOUD_CREDENTIAL", "GCP_GCLOUD_CREDENTIAL")]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    gcp = next(t for t in cfg["transforms"] if t["name"] == "gcp_auth")
    assert gcp["config"]["rules"] == [{"host": "*.googleapis.com"}]
    assert gcp["config"]["scopes"] == [
        "https://www.googleapis.com/auth/cloud-platform"
    ]


def test_render_gcp_auth_uses_per_secret_scopes() -> None:
    secrets = [
        GcpAuthSecret(
            "GSUITE_GCP_CREDENTIAL",
            "GSUITE_GCP_CREDENTIAL",
            ("gmail.googleapis.com",),
            (
                "https://www.googleapis.com/auth/gmail.modify",
                "https://www.googleapis.com/auth/drive",
            ),
        )
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    gcp = next(t for t in cfg["transforms"] if t["name"] == "gcp_auth")
    assert gcp["config"]["scopes"] == [
        "https://www.googleapis.com/auth/drive",
        "https://www.googleapis.com/auth/gmail.modify",
    ]


def test_render_emits_one_gcp_auth_transform_per_keyfile() -> None:
    secrets = [
        GcpAuthSecret("WORKSPACE", "GSUITE_GCP_CREDENTIAL", ("gmail.googleapis.com",)),
        GcpAuthSecret("DATA", "BIGQUERY_GCP_CREDENTIAL", ("bigquery.googleapis.com",)),
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    gcp_blocks = [t for t in cfg["transforms"] if t["name"] == "gcp_auth"]
    assert len(gcp_blocks) == 2
    # Sorted by secret_ref: BIGQUERY_GCP_CREDENTIAL before GSUITE_GCP_CREDENTIAL.
    assert gcp_blocks[0]["config"]["rules"] == [{"host": "bigquery.googleapis.com"}]
    assert gcp_blocks[1]["config"]["rules"] == [{"host": "gmail.googleapis.com"}]


def test_render_merges_gcp_auth_hosts_for_shared_keyfile() -> None:
    secrets = [
        GcpAuthSecret("A", "SHARED_CREDENTIAL", ("gmail.googleapis.com",)),
        GcpAuthSecret("B", "SHARED_CREDENTIAL", ("drive.googleapis.com",)),
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    gcp_blocks = [t for t in cfg["transforms"] if t["name"] == "gcp_auth"]
    assert len(gcp_blocks) == 1
    assert {r["host"] for r in gcp_blocks[0]["config"]["rules"]} == {
        "drive.googleapis.com",
        "gmail.googleapis.com",
    }


# ── oauth_token renderer ─────────────────────────────────────────────────────


_RENDER_REFRESH_FIELDS = (
    ("client_id", OAuthFieldSource("GOOGLE_TOKEN_JSON", "client_id")),
    ("refresh_token", OAuthFieldSource("GOOGLE_TOKEN_JSON", "refresh_token")),
)
_RENDER_CC_FIELDS = (
    ("client_id", OAuthFieldSource("OAUTH_CLIENT_ID")),
    ("client_secret", OAuthFieldSource("OAUTH_CLIENT_SECRET")),
)


def test_render_inject_mode_query_param_secret(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "env")
    secrets = [
        HttpSecret(
            "VENDOR_KEY",
            "VENDOR_KEY",
            mode=SecretMode.INJECT,
            hosts=("api.vendor.com",),
            inject_query_param="api_key",
        ),
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    secrets_block = next(t for t in cfg["transforms"] if t["name"] == "secrets")
    assert secrets_block["config"]["secrets"][0]["inject"] == {
        "query_param": "api_key"
    }


def test_render_emits_oauth_token_transform(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "env")
    secrets = [
        OAuthTokenSecret(
            name="GOOGLE_TOKEN_JSON",
            grant="refresh_token",
            hosts=("gmail.googleapis.com", "www.googleapis.com"),
            fields=_RENDER_REFRESH_FIELDS,
        )
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    assert [t["name"] for t in cfg["transforms"]] == [
        "allowlist",
        "oauth_token",
        "header_allowlist",
    ]
    tokens = next(
        t for t in cfg["transforms"] if t["name"] == "oauth_token"
    )["config"]["tokens"]
    assert len(tokens) == 1
    assert tokens[0]["grant"] == "refresh_token"
    # each field resolves to its own source, with json_key for JSON secrets
    assert tokens[0]["refresh_token"] == {
        "type": "env",
        "var": "GOOGLE_TOKEN_JSON",
        "json_key": "refresh_token",
    }
    assert tokens[0]["client_id"] == {
        "type": "env",
        "var": "GOOGLE_TOKEN_JSON",
        "json_key": "client_id",
    }
    assert tokens[0]["rules"] == [
        {"host": "gmail.googleapis.com"},
        {"host": "www.googleapis.com"},
    ]
    # optional fields omitted when unset
    assert "scopes" not in tokens[0]
    assert "token_endpoint" not in tokens[0]


def test_render_oauth_token_field_omits_json_key_for_whole_secret(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "env")
    secrets = [
        OAuthTokenSecret(
            name="OAUTH_APP",
            grant="client_credentials",
            hosts=("api.example.com",),
            fields=(
                ("client_id", OAuthFieldSource("OAUTH_CLIENT_ID")),
                ("client_secret", OAuthFieldSource("OAUTH_CLIENT_SECRET")),
            ),
        )
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    tokens = next(
        t for t in cfg["transforms"] if t["name"] == "oauth_token"
    )["config"]["tokens"]
    assert tokens[0]["client_id"] == {"type": "env", "var": "OAUTH_CLIENT_ID"}


def test_render_oauth_token_merges_entries_by_token_identity() -> None:
    secrets = [
        OAuthTokenSecret(
            "A", "refresh_token", ("gmail.googleapis.com",),
            _RENDER_REFRESH_FIELDS, ("scope.a",),
        ),
        OAuthTokenSecret(
            "B", "refresh_token", ("drive.googleapis.com",),
            _RENDER_REFRESH_FIELDS, ("scope.b",),
        ),
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    tokens = next(
        t for t in cfg["transforms"] if t["name"] == "oauth_token"
    )["config"]["tokens"]
    assert len(tokens) == 1
    assert {r["host"] for r in tokens[0]["rules"]} == {
        "gmail.googleapis.com",
        "drive.googleapis.com",
    }
    assert tokens[0]["scopes"] == ["scope.a", "scope.b"]


def test_render_oauth_token_separate_entries_for_distinct_fields() -> None:
    secrets = [
        OAuthTokenSecret(
            "A", "refresh_token", ("gmail.googleapis.com",),
            _RENDER_REFRESH_FIELDS,
        ),
        OAuthTokenSecret(
            "B", "refresh_token", ("drive.googleapis.com",),
            (("client_id", OAuthFieldSource("OTHER", "client_id")),
             ("refresh_token", OAuthFieldSource("OTHER", "refresh_token"))),
        ),
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    tokens = next(
        t for t in cfg["transforms"] if t["name"] == "oauth_token"
    )["config"]["tokens"]
    assert len(tokens) == 2


def test_render_oauth_token_emits_token_endpoint_when_set() -> None:
    secrets = [
        OAuthTokenSecret(
            name="OAUTH_APP",
            grant="client_credentials",
            hosts=("api.example.com",),
            fields=_RENDER_CC_FIELDS,
            token_endpoint="https://login.example.com/oauth2/token",
        )
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    tokens = next(
        t for t in cfg["transforms"] if t["name"] == "oauth_token"
    )["config"]["tokens"]
    assert tokens[0]["token_endpoint"] == "https://login.example.com/oauth2/token"


def test_render_omits_managed_transforms_when_no_matching_secrets() -> None:
    cfg = yaml.safe_load(render_proxy_yaml([]))
    assert [t["name"] for t in cfg["transforms"]] == [
        "allowlist",
        "header_allowlist",
    ]
    assert "postgres" not in cfg


def test_render_emits_postgres_listeners_with_env_refs(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "env")
    secrets = [
        PgDsnSecret("DATABASE_URL", "DB_REF", "memo_db"),
        PgDsnSecret("ANALYTICS_PG", "AN_REF", "analytics"),
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    listeners = cfg["postgres"]
    assert [l["name"] for l in listeners] == ["analytics_pg", "database_url"]
    assert listeners[0]["listen"] == "0.0.0.0:5432"
    assert listeners[1]["listen"] == "0.0.0.0:5433"
    # upstream.dsn uses the secret_ref directly so iron-proxy can resolve it
    # from env (or 1Password, depending on FIREWALL_MANAGER_SECRET_SOURCE).
    assert listeners[0]["upstream"] == {"dsn": {"type": "env", "var": "AN_REF"}}
    assert listeners[1]["upstream"] == {"dsn": {"type": "env", "var": "DB_REF"}}
    assert listeners[0]["client"] == {
        "user": "app_user",
        "password_env": "PG_PROXY_PASSWORD_ANALYTICS_PG",
    }


def test_render_postgres_upstream_dsn_uses_onepassword_source(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "onepassword")
    monkeypatch.setenv("OP_VAULT", "ai-agents")
    secrets = [PgDsnSecret("DATABASE_URL", "INVESTMEMOS_PG_DSN", "memo_db")]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    upstream = cfg["postgres"][0]["upstream"]["dsn"]
    assert upstream["type"] == "1password"
    assert upstream["secret_ref"] == "op://ai-agents/INVESTMEMOS_PG_DSN/credential"


def test_render_with_onepassword_source_emits_op_ref(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "onepassword-connect")
    monkeypatch.setenv("OP_VAULT", "engineering")
    secrets = [GcpAuthSecret("GCP_GCLOUD_CREDENTIAL", "GCP_GCLOUD_CREDENTIAL")]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    gcp = next(t for t in cfg["transforms"] if t["name"] == "gcp_auth")
    assert gcp["config"]["keyfile"]["type"] == "1password_connect"
    assert (
        gcp["config"]["keyfile"]["secret_ref"]
        == "op://engineering/GCP_GCLOUD_CREDENTIAL/credential"
    )


def test_render_groups_header_secret_hosts_when_repeated() -> None:
    secrets = [
        HttpSecret(
            "GITHUB_TOKEN",
            "GITHUB_TOKEN",
            hosts=("github.com", "api.github.com"),
            match_headers=("Authorization",),
        ),
        HttpSecret(
            "GITHUB_TOKEN",
            "GITHUB_TOKEN",
            hosts=("uploads.github.com",),
            match_headers=("Authorization",),
        ),
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    secrets_block = next(t for t in cfg["transforms"] if t["name"] == "secrets")
    entries = secrets_block["config"]["secrets"]
    # The same secret declared on different hosts merges into one entry.
    assert len(entries) == 1
    assert {r["host"] for r in entries[0]["rules"]} == {
        "github.com",
        "api.github.com",
        "uploads.github.com",
    }
