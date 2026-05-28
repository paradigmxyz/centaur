from __future__ import annotations

import pytest
import yaml

from api.proxy_config import (
    CENTAUR_CORE_PG_LISTENER,
    PG_LISTEN_PORT_BASE,
    assign_pg_listen_ports,
    core_pg_listen_port,
    render_proxy_yaml,
)
from api.tool_manager import (
    DEFAULT_MATCH_HEADERS,
    GcpAuthSecret,
    HmacHeader,
    HmacSignSecret,
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


def test_parser_typed_oauth_token_password_grant() -> None:
    secret = _parse_secret(
        {
            "type": "oauth_token",
            "grant": "password",
            "name": "VENDOR_OAUTH",
            "hosts": ["api.example.com"],
            "token_endpoint": "https://api.example.com/token",
            "fields": {
                "username": "API_USERNAME",
                "password": "API_PASSWORD",
                "client_id": "API_CLIENT_ID",
                "client_secret": "API_CLIENT_SECRET",
            },
        }
    )
    assert isinstance(secret, OAuthTokenSecret)
    assert secret.grant == "password"
    assert secret.token_endpoint == "https://api.example.com/token"
    fields = dict(secret.fields)
    assert fields["username"] == OAuthFieldSource("API_USERNAME")
    assert fields["password"] == OAuthFieldSource("API_PASSWORD")
    assert fields["client_id"] == OAuthFieldSource("API_CLIENT_ID")
    assert fields["client_secret"] == OAuthFieldSource("API_CLIENT_SECRET")


def test_parser_oauth_token_password_grant_makes_client_secret_optional() -> None:
    # Public clients (RFC 6749 4.3) authenticate with username/password and a
    # client_id only — client_secret is optional.
    secret = _parse_secret(
        {
            "type": "oauth_token",
            "grant": "password",
            "name": "VENDOR_OAUTH",
            "hosts": ["api.example.com"],
            "fields": {
                "username": "API_USERNAME",
                "password": "API_PASSWORD",
                "client_id": "API_CLIENT_ID",
            },
        }
    )
    assert isinstance(secret, OAuthTokenSecret)
    assert "client_secret" not in dict(secret.fields)


def test_parser_oauth_token_password_grant_rejects_missing_username() -> None:
    with pytest.raises(ValueError, match="requires fields \\['username'\\]"):
        _parse_secret(
            {
                "type": "oauth_token",
                "grant": "password",
                "name": "X",
                "hosts": ["h"],
                "fields": {
                    "password": "PW",
                    "client_id": "CID",
                },
            }
        )


def test_parser_typed_oauth_token_jwt_bearer() -> None:
    secret = _parse_secret(
        {
            "type": "oauth_token",
            "grant": "jwt_bearer",
            "name": "DOCUSIGN_JWT",
            "hosts": ["*.docusign.net"],
            "audience": "account-d.docusign.com",
            "token_endpoint": "https://account-d.docusign.com/oauth/token",
            "scopes": ["signature", "impersonation"],
            "fields": {
                "issuer": "DOCUSIGN_INTEGRATION_KEY",
                "subject": "DOCUSIGN_USER_GUID",
                "private_key": {
                    "secret_ref": "DOCUSIGN_BUNDLE",
                    "json_key": "private_key",
                },
                "private_key_id": "DOCUSIGN_KEY_ID",
            },
        }
    )
    assert isinstance(secret, OAuthTokenSecret)
    assert secret.grant == "jwt_bearer"
    assert secret.audience == "account-d.docusign.com"
    assert secret.token_endpoint == "https://account-d.docusign.com/oauth/token"
    assert secret.scopes == ("signature", "impersonation")
    fields = dict(secret.fields)
    assert fields["issuer"] == OAuthFieldSource("DOCUSIGN_INTEGRATION_KEY")
    assert fields["subject"] == OAuthFieldSource("DOCUSIGN_USER_GUID")
    assert fields["private_key"] == OAuthFieldSource(
        "DOCUSIGN_BUNDLE", "private_key"
    )
    assert fields["private_key_id"] == OAuthFieldSource("DOCUSIGN_KEY_ID")


def test_parser_oauth_token_jwt_bearer_makes_private_key_id_optional() -> None:
    secret = _parse_secret(
        {
            "type": "oauth_token",
            "grant": "jwt_bearer",
            "name": "VENDOR_JWT",
            "hosts": ["api.vendor.com"],
            "audience": "api.vendor.com",
            "fields": {
                "issuer": "INTEGRATION_KEY",
                "subject": "USER_GUID",
                "private_key": "PRIVATE_KEY_PEM",
            },
        }
    )
    assert isinstance(secret, OAuthTokenSecret)
    assert "private_key_id" not in dict(secret.fields)


def test_parser_oauth_token_jwt_bearer_requires_audience() -> None:
    with pytest.raises(ValueError, match="requires a non-empty 'audience'"):
        _parse_secret(
            {
                "type": "oauth_token",
                "grant": "jwt_bearer",
                "name": "X",
                "hosts": ["h"],
                "fields": {
                    "issuer": "ISS",
                    "subject": "SUB",
                    "private_key": "PK",
                },
            }
        )


def test_parser_oauth_token_jwt_bearer_rejects_missing_private_key() -> None:
    with pytest.raises(ValueError, match="requires fields \\['private_key'\\]"):
        _parse_secret(
            {
                "type": "oauth_token",
                "grant": "jwt_bearer",
                "name": "X",
                "hosts": ["h"],
                "audience": "aud",
                "fields": {
                    "issuer": "ISS",
                    "subject": "SUB",
                },
            }
        )


def test_parser_oauth_token_audience_rejected_for_non_jwt_bearer_grant() -> None:
    with pytest.raises(ValueError, match="'audience' is only valid for grant"):
        _parse_secret(
            {
                "type": "oauth_token",
                "grant": "client_credentials",
                "name": "X",
                "hosts": ["h"],
                "audience": "aud",
                "fields": {
                    "client_id": "CID",
                    "client_secret": "CSEC",
                },
            }
        )


def test_parser_oauth_token_token_endpoint_headers_accepts_bare_string() -> None:
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
            "token_endpoint_headers": {
                "x-api-key": "API_KEY",
            },
        }
    )
    assert isinstance(secret, OAuthTokenSecret)
    assert secret.token_endpoint_headers == (
        ("x-api-key", OAuthFieldSource("API_KEY")),
    )


def test_parser_oauth_token_token_endpoint_headers_accepts_table_with_json_key() -> None:
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
            "token_endpoint_headers": {
                "x-api-key": {"secret_ref": "BUNDLE", "json_key": "api_key"},
            },
        }
    )
    assert secret.token_endpoint_headers == (
        ("x-api-key", OAuthFieldSource("BUNDLE", "api_key")),
    )


def test_parser_oauth_token_token_endpoint_headers_rejects_empty_table() -> None:
    with pytest.raises(
        ValueError, match="'token_endpoint_headers' must be a non-empty table"
    ):
        _parse_secret(
            {
                "type": "oauth_token",
                "grant": "client_credentials",
                "name": "X",
                "hosts": ["h"],
                "fields": {
                    "client_id": "CID",
                    "client_secret": "CS",
                },
                "token_endpoint_headers": {},
            }
        )


def test_parser_oauth_token_token_endpoint_headers_defaults_to_empty() -> None:
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
    assert secret.token_endpoint_headers == ()


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


def _hmac_entry(**overrides: object) -> dict:
    """Minimum valid ``hmac_sign`` entry, overridable per-test."""
    entry: dict = {
        "type": "hmac_sign",
        "name": "FALCONX",
        "hosts": ["api.falconx.io"],
        "algorithm": "sha256",
        "key_encoding": "base64",
        "output_encoding": "base64",
        "timestamp_format": "unix_seconds",
        "message": "{{.Timestamp}}{{.Method}}{{.PathWithQuery}}{{.Body}}",
        "credentials": {
            "key": "FALCONX_API_KEY",
            "secret": "FALCONX_SECRET",
            "passphrase": "FALCONX_PASSPHRASE",
        },
        "headers": [
            {"name": "FX-ACCESS-KEY", "value": "{{.Credentials.key}}"},
            {"name": "FX-ACCESS-SIGN", "value": "{{.Signature}}"},
            {"name": "FX-ACCESS-TIMESTAMP", "value": "{{.Timestamp}}"},
            {
                "name": "FX-ACCESS-PASSPHRASE",
                "value": "{{.Credentials.passphrase}}",
            },
        ],
    }
    entry.update(overrides)
    return entry


def test_parser_typed_hmac_sign_full_example() -> None:
    secret = _parse_secret(_hmac_entry())
    assert isinstance(secret, HmacSignSecret)
    assert secret.hosts == ("api.falconx.io",)
    assert secret.algorithm == "sha256"
    assert secret.key_encoding == "base64"
    assert secret.output_encoding == "base64"
    assert secret.timestamp_format == "unix_seconds"
    # credentials sort by name so the rendered config is deterministic
    assert [name for name, _ in secret.credentials] == ["key", "passphrase", "secret"]
    assert secret.headers[0] == HmacHeader(
        "FX-ACCESS-KEY", "{{.Credentials.key}}"
    )
    assert secret.allow_chunked_body is False


def test_parser_hmac_sign_requires_secret_credential() -> None:
    with pytest.raises(ValueError, match="must include 'secret'"):
        _parse_secret(_hmac_entry(credentials={"key": "FALCONX_API_KEY"}))


def test_parser_hmac_sign_credential_supports_json_key() -> None:
    secret = _parse_secret(
        _hmac_entry(
            credentials={
                "secret": {"secret_ref": "FALCONX_BUNDLE", "json_key": "hmac_key"},
            }
        )
    )
    assert isinstance(secret, HmacSignSecret)
    creds = dict(secret.credentials)
    assert creds["secret"] == OAuthFieldSource("FALCONX_BUNDLE", "hmac_key")


def test_parser_hmac_sign_rejects_unknown_algorithm() -> None:
    with pytest.raises(ValueError, match="'algorithm' must be one of"):
        _parse_secret(_hmac_entry(algorithm="md5"))


def test_parser_hmac_sign_rejects_unknown_timestamp_format() -> None:
    with pytest.raises(ValueError, match="'timestamp_format' must be one of"):
        _parse_secret(_hmac_entry(timestamp_format="iso8601"))


def test_parser_hmac_sign_rejects_unknown_key_encoding() -> None:
    with pytest.raises(ValueError, match="'key_encoding' must be one of"):
        _parse_secret(_hmac_entry(key_encoding="utf8"))


def test_parser_hmac_sign_rejects_empty_headers() -> None:
    with pytest.raises(ValueError, match="'headers' must be a non-empty list"):
        _parse_secret(_hmac_entry(headers=[]))


def test_parser_hmac_sign_requires_hosts() -> None:
    entry = _hmac_entry()
    entry.pop("hosts")
    with pytest.raises(ValueError, match="'hosts' must be a non-empty array"):
        _parse_secret(entry)


def test_parser_hmac_sign_requires_message() -> None:
    with pytest.raises(ValueError, match="'message' must be a non-empty"):
        _parse_secret(_hmac_entry(message=""))


def test_parser_hmac_sign_allow_chunked_body_must_be_bool() -> None:
    with pytest.raises(ValueError, match="'allow_chunked_body' must be a boolean"):
        _parse_secret(_hmac_entry(allow_chunked_body="yes"))


def test_parser_hmac_sign_header_requires_name_and_value() -> None:
    with pytest.raises(ValueError, match="header\\[0\\] requires a non-empty 'value'"):
        _parse_secret(
            _hmac_entry(headers=[{"name": "X-Sig", "value": ""}])
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


def test_core_pg_listen_port_is_after_tool_listeners() -> None:
    ports = {"ALPHA": PG_LISTEN_PORT_BASE, "ZEBRA": PG_LISTEN_PORT_BASE + 1}
    assert core_pg_listen_port(ports) == PG_LISTEN_PORT_BASE + 2
    assert core_pg_listen_port({}) == PG_LISTEN_PORT_BASE


def test_render_core_listener_uses_forced_env_upstream() -> None:
    # No tool pg_dsn secrets; core listener still rendered when core_pg given.
    core_pg = {
        "port": PG_LISTEN_PORT_BASE,
        "dsn_env_var": "CENTAUR_DATABASE_URL",
        "password_env": "PG_PROXY_PASSWORD_CENTAUR_CORE",
    }
    cfg = yaml.safe_load(render_proxy_yaml([], core_pg=core_pg))
    listeners = cfg["postgres"]
    assert len(listeners) == 1
    core = listeners[0]
    assert core["name"] == CENTAUR_CORE_PG_LISTENER
    assert core["listen"] == f"0.0.0.0:{PG_LISTEN_PORT_BASE}"
    # forced env source (not 1Password) since the proxy always has the DSN env
    assert core["upstream"]["dsn"] == {"type": "env", "var": "CENTAUR_DATABASE_URL"}
    assert core["client"] == {
        "user": "app_user",
        "password_env": "PG_PROXY_PASSWORD_CENTAUR_CORE",
    }


def test_render_core_listener_appended_after_tool_listeners() -> None:
    secrets = [PgDsnSecret("TOOLDB", "TOOLDB", "tooldb")]
    pg_listen_ports = assign_pg_listen_ports(secrets)
    core_pg = {
        "port": core_pg_listen_port(pg_listen_ports),
        "dsn_env_var": "CENTAUR_DATABASE_URL",
        "password_env": "PG_PROXY_PASSWORD_CENTAUR_CORE",
    }
    cfg = yaml.safe_load(
        render_proxy_yaml(secrets, pg_listen_ports=pg_listen_ports, core_pg=core_pg)
    )
    names = [listener["name"] for listener in cfg["postgres"]]
    assert names == ["tooldb", CENTAUR_CORE_PG_LISTENER]
    # core port does not collide with the tool listener's port
    ports = {listener["name"]: listener["listen"] for listener in cfg["postgres"]}
    assert ports["tooldb"] == f"0.0.0.0:{PG_LISTEN_PORT_BASE}"
    assert ports[CENTAUR_CORE_PG_LISTENER] == f"0.0.0.0:{PG_LISTEN_PORT_BASE + 1}"


def test_render_without_core_pg_emits_no_core_listener() -> None:
    cfg = yaml.safe_load(render_proxy_yaml([]))
    assert "postgres" not in cfg


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


_RENDER_PASSWORD_FIELDS = (
    ("client_id", OAuthFieldSource("API_CLIENT_ID")),
    ("client_secret", OAuthFieldSource("API_CLIENT_SECRET")),
    ("password", OAuthFieldSource("API_PASSWORD")),
    ("username", OAuthFieldSource("API_USERNAME")),
)


def test_render_oauth_token_password_grant(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "env")
    secrets = [
        OAuthTokenSecret(
            name="VENDOR_OAUTH",
            grant="password",
            hosts=("api.example.com",),
            fields=_RENDER_PASSWORD_FIELDS,
            token_endpoint="https://api.example.com/token",
        )
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    tokens = next(
        t for t in cfg["transforms"] if t["name"] == "oauth_token"
    )["config"]["tokens"]
    assert tokens[0]["grant"] == "password"
    assert tokens[0]["username"] == {"type": "env", "var": "API_USERNAME"}
    assert tokens[0]["password"] == {"type": "env", "var": "API_PASSWORD"}
    assert tokens[0]["client_id"] == {"type": "env", "var": "API_CLIENT_ID"}
    assert tokens[0]["client_secret"] == {"type": "env", "var": "API_CLIENT_SECRET"}
    assert tokens[0]["token_endpoint"] == "https://api.example.com/token"
    assert tokens[0]["rules"] == [{"host": "api.example.com"}]


def test_render_oauth_token_emits_token_endpoint_headers_when_set(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "env")
    secrets = [
        OAuthTokenSecret(
            name="OAUTH_APP",
            grant="client_credentials",
            hosts=("api.example.com",),
            fields=_RENDER_CC_FIELDS,
            token_endpoint="https://login.example.com/oauth2/token",
            token_endpoint_headers=(
                ("x-api-key", OAuthFieldSource("API_KEY")),
            ),
        )
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    tokens = next(
        t for t in cfg["transforms"] if t["name"] == "oauth_token"
    )["config"]["tokens"]
    assert tokens[0]["token_endpoint_headers"] == {
        "x-api-key": {"type": "env", "var": "API_KEY"},
    }


def test_render_oauth_token_omits_token_endpoint_headers_when_empty() -> None:
    secrets = [
        OAuthTokenSecret(
            name="OAUTH_APP",
            grant="client_credentials",
            hosts=("api.example.com",),
            fields=_RENDER_CC_FIELDS,
        )
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    tokens = next(
        t for t in cfg["transforms"] if t["name"] == "oauth_token"
    )["config"]["tokens"]
    assert "token_endpoint_headers" not in tokens[0]


def test_render_oauth_token_separate_entries_for_distinct_endpoint_headers() -> None:
    secrets = [
        OAuthTokenSecret(
            "A",
            "client_credentials",
            ("a.example.com",),
            _RENDER_CC_FIELDS,
            token_endpoint_headers=(("x-api-key", OAuthFieldSource("KEY_A")),),
        ),
        OAuthTokenSecret(
            "B",
            "client_credentials",
            ("b.example.com",),
            _RENDER_CC_FIELDS,
            token_endpoint_headers=(("x-api-key", OAuthFieldSource("KEY_B")),),
        ),
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    tokens = next(
        t for t in cfg["transforms"] if t["name"] == "oauth_token"
    )["config"]["tokens"]
    assert len(tokens) == 2


def test_render_oauth_token_endpoint_headers_resolve_via_onepassword(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "onepassword")
    monkeypatch.setenv("OP_VAULT", "ai-agents")
    secrets = [
        OAuthTokenSecret(
            name="OAUTH_APP",
            grant="client_credentials",
            hosts=("api.example.com",),
            fields=_RENDER_CC_FIELDS,
            token_endpoint_headers=(("x-api-key", OAuthFieldSource("API_KEY")),),
        )
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    tokens = next(
        t for t in cfg["transforms"] if t["name"] == "oauth_token"
    )["config"]["tokens"]
    headers = tokens[0]["token_endpoint_headers"]
    assert headers["x-api-key"]["type"] == "1password"
    assert headers["x-api-key"]["secret_ref"] == "op://ai-agents/API_KEY/credential"


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


_RENDER_JWT_BEARER_FIELDS = (
    ("issuer", OAuthFieldSource("DOCUSIGN_INTEGRATION_KEY")),
    ("private_key", OAuthFieldSource("DOCUSIGN_BUNDLE", "private_key")),
    ("private_key_id", OAuthFieldSource("DOCUSIGN_KEY_ID")),
    ("subject", OAuthFieldSource("DOCUSIGN_USER_GUID")),
)


def test_render_oauth_token_jwt_bearer_emits_audience_and_field_sources(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "env")
    secrets = [
        OAuthTokenSecret(
            name="DOCUSIGN_JWT",
            grant="jwt_bearer",
            hosts=("demo.docusign.net",),
            fields=_RENDER_JWT_BEARER_FIELDS,
            scopes=("signature", "impersonation"),
            token_endpoint="https://account-d.docusign.com/oauth/token",
            audience="account-d.docusign.com",
        )
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    tokens = next(
        t for t in cfg["transforms"] if t["name"] == "oauth_token"
    )["config"]["tokens"]
    assert tokens[0]["grant"] == "jwt_bearer"
    assert tokens[0]["issuer"] == {
        "type": "env",
        "var": "DOCUSIGN_INTEGRATION_KEY",
    }
    assert tokens[0]["subject"] == {"type": "env", "var": "DOCUSIGN_USER_GUID"}
    assert tokens[0]["private_key"] == {
        "type": "env",
        "var": "DOCUSIGN_BUNDLE",
        "json_key": "private_key",
    }
    assert tokens[0]["private_key_id"] == {
        "type": "env",
        "var": "DOCUSIGN_KEY_ID",
    }
    assert tokens[0]["audience"] == "account-d.docusign.com"
    assert tokens[0]["token_endpoint"] == "https://account-d.docusign.com/oauth/token"
    assert tokens[0]["scopes"] == ["impersonation", "signature"]
    assert tokens[0]["rules"] == [{"host": "demo.docusign.net"}]


def test_render_oauth_token_omits_audience_when_unset() -> None:
    secrets = [
        OAuthTokenSecret(
            name="OAUTH_APP",
            grant="client_credentials",
            hosts=("api.example.com",),
            fields=_RENDER_CC_FIELDS,
        )
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    tokens = next(
        t for t in cfg["transforms"] if t["name"] == "oauth_token"
    )["config"]["tokens"]
    assert "audience" not in tokens[0]


def test_render_oauth_token_separate_entries_for_distinct_audiences() -> None:
    secrets = [
        OAuthTokenSecret(
            "A",
            "jwt_bearer",
            ("a.example.net",),
            _RENDER_JWT_BEARER_FIELDS,
            audience="a.example.com",
        ),
        OAuthTokenSecret(
            "B",
            "jwt_bearer",
            ("b.example.net",),
            _RENDER_JWT_BEARER_FIELDS,
            audience="b.example.com",
        ),
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    tokens = next(
        t for t in cfg["transforms"] if t["name"] == "oauth_token"
    )["config"]["tokens"]
    assert len(tokens) == 2
    assert {t["audience"] for t in tokens} == {"a.example.com", "b.example.com"}


_RENDER_HMAC_CREDS: tuple[tuple[str, OAuthFieldSource], ...] = (
    ("key", OAuthFieldSource("FALCONX_API_KEY")),
    ("passphrase", OAuthFieldSource("FALCONX_PASSPHRASE")),
    ("secret", OAuthFieldSource("FALCONX_SECRET")),
)

_RENDER_HMAC_HEADERS: tuple[HmacHeader, ...] = (
    HmacHeader("FX-ACCESS-KEY", "{{.Credentials.key}}"),
    HmacHeader("FX-ACCESS-SIGN", "{{.Signature}}"),
    HmacHeader("FX-ACCESS-TIMESTAMP", "{{.Timestamp}}"),
    HmacHeader("FX-ACCESS-PASSPHRASE", "{{.Credentials.passphrase}}"),
)


def _falconx_secret(
    *, hosts: tuple[str, ...] = ("api.falconx.io",), **overrides: object
) -> HmacSignSecret:
    fields: dict[str, object] = {
        "name": "FALCONX",
        "hosts": hosts,
        "credentials": _RENDER_HMAC_CREDS,
        "headers": _RENDER_HMAC_HEADERS,
        "algorithm": "sha256",
        "key_encoding": "base64",
        "output_encoding": "base64",
        "message": "{{.Timestamp}}{{.Method}}{{.PathWithQuery}}{{.Body}}",
        "timestamp_format": "unix_seconds",
    }
    fields.update(overrides)
    return HmacSignSecret(**fields)  # type: ignore[arg-type]


def test_render_hmac_sign_matches_iron_proxy_schema(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "env")
    cfg = yaml.safe_load(render_proxy_yaml([_falconx_secret()]))
    assert [t["name"] for t in cfg["transforms"]] == [
        "allowlist",
        "hmac_sign",
        "header_allowlist",
    ]
    hmac = next(t for t in cfg["transforms"] if t["name"] == "hmac_sign")["config"]
    assert hmac["timestamp"] == {"format": "unix_seconds"}
    assert hmac["signature"] == {
        "algorithm": "sha256",
        "key_encoding": "base64",
        "output_encoding": "base64",
        "message": "{{.Timestamp}}{{.Method}}{{.PathWithQuery}}{{.Body}}",
    }
    assert hmac["credentials"] == {
        "key": {"type": "env", "var": "FALCONX_API_KEY"},
        "passphrase": {"type": "env", "var": "FALCONX_PASSPHRASE"},
        "secret": {"type": "env", "var": "FALCONX_SECRET"},
    }
    # header order is preserved on the wire
    assert hmac["headers"] == [
        {"name": "FX-ACCESS-KEY", "value": "{{.Credentials.key}}"},
        {"name": "FX-ACCESS-SIGN", "value": "{{.Signature}}"},
        {"name": "FX-ACCESS-TIMESTAMP", "value": "{{.Timestamp}}"},
        {"name": "FX-ACCESS-PASSPHRASE", "value": "{{.Credentials.passphrase}}"},
    ]
    assert hmac["rules"] == [{"host": "api.falconx.io"}]
    # opt-in field is omitted at the safe default
    assert "allow_chunked_body" not in hmac


def test_render_hmac_sign_emits_allow_chunked_body_when_opted_in() -> None:
    cfg = yaml.safe_load(
        render_proxy_yaml([_falconx_secret(allow_chunked_body=True)])
    )
    hmac = next(t for t in cfg["transforms"] if t["name"] == "hmac_sign")["config"]
    assert hmac["allow_chunked_body"] is True


def test_render_hmac_sign_credential_supports_json_key() -> None:
    secrets = [
        _falconx_secret(
            credentials=(
                ("secret", OAuthFieldSource("FALCONX_BUNDLE", "hmac_key")),
            )
        )
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    hmac = next(t for t in cfg["transforms"] if t["name"] == "hmac_sign")["config"]
    assert hmac["credentials"]["secret"] == {
        "type": "env",
        "var": "FALCONX_BUNDLE",
        "json_key": "hmac_key",
    }


def test_render_hmac_sign_merges_entries_with_same_scheme_unioning_hosts() -> None:
    secrets = [
        _falconx_secret(hosts=("a.falconx.io",)),
        _falconx_secret(hosts=("b.falconx.io",)),
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    transforms = [t for t in cfg["transforms"] if t["name"] == "hmac_sign"]
    assert len(transforms) == 1
    assert transforms[0]["config"]["rules"] == [
        {"host": "a.falconx.io"},
        {"host": "b.falconx.io"},
    ]


def test_render_hmac_sign_separate_transforms_for_distinct_schemes() -> None:
    secrets = [
        _falconx_secret(),
        _falconx_secret(algorithm="sha512", hosts=("v2.falconx.io",)),
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    transforms = [t for t in cfg["transforms"] if t["name"] == "hmac_sign"]
    assert len(transforms) == 2
    assert {t["config"]["signature"]["algorithm"] for t in transforms} == {
        "sha256",
        "sha512",
    }


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


# ── brokered_token parser ───────────────────────────────────────────────────


def test_parser_typed_brokered_token() -> None:
    from api.tool_manager import BrokeredTokenSecret

    secret = _parse_secret(
        {
            "type": "brokered_token",
            "name": "openai-codex",
            "hosts": ["auth.openai.com"],
            "token_endpoint": "https://auth.openai.com/oauth/token",
            "fields": {
                "client_id": "CODEX_CLIENT_ID",
                "refresh_token": "CODEX_BLOB",
            },
        }
    )
    assert isinstance(secret, BrokeredTokenSecret)
    assert secret.hosts == ("auth.openai.com",)
    assert secret.token_endpoint == "https://auth.openai.com/oauth/token"
    assert [name for name, _ in secret.fields] == ["client_id", "refresh_token"]


def test_parser_brokered_token_allows_json_key_on_read_fields() -> None:
    secret = _parse_secret(
        {
            "type": "brokered_token",
            "name": "okta",
            "hosts": ["example.okta.com"],
            "token_endpoint": "https://example.okta.com/token",
            "fields": {
                "client_id": {
                    "secret_ref": "OKTA_BUNDLE",
                    "json_key": "client_id",
                },
                "client_secret": {
                    "secret_ref": "OKTA_BUNDLE",
                    "json_key": "client_secret",
                },
                "refresh_token": "OKTA_BLOB",
            },
        }
    )
    fields = dict(secret.fields)
    assert fields["client_id"] == OAuthFieldSource("OKTA_BUNDLE", "client_id")
    assert fields["client_secret"] == OAuthFieldSource("OKTA_BUNDLE", "client_secret")


def test_parser_brokered_token_rejects_json_key_on_refresh_token() -> None:
    with pytest.raises(ValueError, match="does not support 'json_key'"):
        _parse_secret(
            {
                "type": "brokered_token",
                "name": "codex",
                "hosts": ["auth.openai.com"],
                "token_endpoint": "https://auth.openai.com/token",
                "fields": {
                    "client_id": "CODEX_CLIENT_ID",
                    "refresh_token": {
                        "secret_ref": "CODEX_BUNDLE",
                        "json_key": "refresh_token",
                    },
                },
            }
        )


def test_parser_brokered_token_requires_required_fields() -> None:
    with pytest.raises(ValueError, match="requires fields"):
        _parse_secret(
            {
                "type": "brokered_token",
                "name": "codex",
                "hosts": ["auth.openai.com"],
                "token_endpoint": "https://auth.openai.com/token",
                "fields": {"client_id": "X"},
            }
        )


def test_parser_brokered_token_rejects_unknown_field() -> None:
    with pytest.raises(ValueError, match="not valid"):
        _parse_secret(
            {
                "type": "brokered_token",
                "name": "codex",
                "hosts": ["auth.openai.com"],
                "token_endpoint": "https://auth.openai.com/token",
                "fields": {
                    "client_id": "X",
                    "refresh_token": "Y",
                    "audience": "Z",
                },
            }
        )


def test_parser_brokered_token_allows_json_key_on_endpoint_headers() -> None:
    secret = _parse_secret(
        {
            "type": "brokered_token",
            "name": "venue",
            "hosts": ["api.venue.example"],
            "token_endpoint": "https://venue.example/oauth/token",
            "fields": {
                "client_id": "VENUE_CLIENT_ID",
                "refresh_token": "VENUE_BLOB",
            },
            "token_endpoint_headers": {
                "x-api-key": {
                    "secret_ref": "VENUE_BUNDLE",
                    "json_key": "api_key",
                },
            },
        }
    )
    assert dict(secret.token_endpoint_headers)["x-api-key"] == OAuthFieldSource(
        "VENUE_BUNDLE", "api_key"
    )


# ── brokered_token routing ──────────────────────────────────────────────────


_BROKERED_FIELDS = (
    ("client_id", OAuthFieldSource("CODEX_CLIENT_ID")),
    ("refresh_token", OAuthFieldSource("CODEX_BLOB")),
)


def test_render_brokered_token_emits_token_broker_source(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from api.tool_manager import BrokeredTokenSecret

    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "env")
    monkeypatch.setenv("FIREWALL_MANAGER_TOKEN_BROKER_TTL", "30s")
    secrets = [
        BrokeredTokenSecret(
            name="openai-codex",
            hosts=("auth.openai.com",),
            fields=_BROKERED_FIELDS,
            token_endpoint="https://auth.openai.com/oauth/token",
        ),
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    # brokered_token never lands on the oauth_token transform.
    assert not any(t["name"] == "oauth_token" for t in cfg["transforms"])
    secrets_block = next(t for t in cfg["transforms"] if t["name"] == "secrets")
    entries = secrets_block["config"]["secrets"]
    assert len(entries) == 1
    entry = entries[0]
    assert entry["source"] == {
        "type": "token_broker",
        "credential_id": "openai-codex",
        "ttl": "30s",
    }
    assert entry["inject"] == {
        "header": "Authorization",
        "formatter": "Bearer {{.Value}}",
    }
    assert entry["rules"] == [{"host": "auth.openai.com"}]


def test_render_refresh_token_oauth_secret_stays_on_oauth_transform() -> None:
    # Bare OAuthTokenSecret with refresh_token grant no longer routes through
    # the broker — tools must opt in by declaring `brokered_token` instead.
    secrets = [
        OAuthTokenSecret(
            name="legacy-oauth",
            grant="refresh_token",
            hosts=("auth.openai.com",),
            fields=_RENDER_REFRESH_FIELDS,
        ),
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    oauth = next(t for t in cfg["transforms"] if t["name"] == "oauth_token")
    assert oauth["config"]["tokens"][0]["grant"] == "refresh_token"


def test_render_brokered_and_oauth_coexist(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from api.tool_manager import BrokeredTokenSecret

    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "env")
    secrets = [
        OAuthTokenSecret(
            name="api-app",
            grant="client_credentials",
            hosts=("api.example.com",),
            fields=_RENDER_CC_FIELDS,
            token_endpoint="https://api.example.com/token",
        ),
        BrokeredTokenSecret(
            name="openai-codex",
            hosts=("auth.openai.com",),
            fields=_BROKERED_FIELDS,
            token_endpoint="https://auth.openai.com/oauth/token",
        ),
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    oauth = next(t for t in cfg["transforms"] if t["name"] == "oauth_token")
    tokens = oauth["config"]["tokens"]
    assert len(tokens) == 1
    assert tokens[0]["grant"] == "client_credentials"
    secrets_block = next(t for t in cfg["transforms"] if t["name"] == "secrets")
    broker_entries = [
        e for e in secrets_block["config"]["secrets"]
        if isinstance(e.get("source"), dict)
        and e["source"].get("type") == "token_broker"
    ]
    assert len(broker_entries) == 1
    assert broker_entries[0]["source"]["credential_id"] == "openai-codex"


def test_render_brokered_token_merges_hosts_across_duplicate_names(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from api.tool_manager import BrokeredTokenSecret

    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "env")
    secrets = [
        BrokeredTokenSecret(
            "claude", ("api.anthropic.com",), _BROKERED_FIELDS,
            token_endpoint="https://console.anthropic.com/v1/oauth/token",
        ),
        BrokeredTokenSecret(
            "claude", ("console.anthropic.com",), _BROKERED_FIELDS,
            token_endpoint="https://console.anthropic.com/v1/oauth/token",
        ),
    ]
    cfg = yaml.safe_load(render_proxy_yaml(secrets))
    secrets_block = next(t for t in cfg["transforms"] if t["name"] == "secrets")
    entries = secrets_block["config"]["secrets"]
    assert len(entries) == 1
    assert {r["host"] for r in entries[0]["rules"]} == {
        "api.anthropic.com",
        "console.anthropic.com",
    }
