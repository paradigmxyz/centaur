"""Option B: tool-server sidecar reaches the core DB through the iron-proxy."""

from __future__ import annotations

import pytest

from api.proxy_config import PG_LISTEN_PORT_BASE
from api.sandbox.kubernetes import (
    _CORE_PG_PASSWORD_ENV,
    _build_core_pg,
    _build_tool_server_container,
    _proxy_iron_env,
)


def test_build_core_pg_points_at_proxy_listener(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("DATABASE_URL", "postgresql://u:p@db:5432/ai_v2")
    monkeypatch.delenv("KUBERNETES_SECRET_ENV_PREFIX", raising=False)

    core = _build_core_pg("proxy-host", {"TOOLDB": PG_LISTEN_PORT_BASE})

    assert core["port"] == PG_LISTEN_PORT_BASE + 1  # after the one tool listener
    assert core["password_env"] == _CORE_PG_PASSWORD_ENV
    assert core["dsn_env_var"] == "DATABASE_URL"
    assert core["password"]  # generated, non-empty
    # proxied DSN points at the proxy listener as app_user, dbname from the API DSN
    assert (
        core["dsn"]
        == f"postgresql://app_user:{core['password']}@proxy-host:{PG_LISTEN_PORT_BASE + 1}/ai_v2"
    )


def test_build_core_pg_respects_secret_env_prefix(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("DATABASE_URL", "postgresql://u:p@db:5432/ai_v2")
    monkeypatch.setenv("KUBERNETES_SECRET_ENV_PREFIX", "CENTAUR_")

    core = _build_core_pg("proxy-host", {})

    assert core["dsn_env_var"] == "CENTAUR_DATABASE_URL"
    assert core["port"] == PG_LISTEN_PORT_BASE  # no tool listeners


def test_tool_server_container_uses_proxied_dsn_value(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("KUBERNETES_TOOL_SERVER_IMAGE", "centaur-api:latest")
    monkeypatch.setenv("KUBERNETES_SECRET_ENV_NAME", "centaur-infra-env")

    dsn = "postgresql://app_user:pw@proxy-host:5433/ai_v2"
    container = _build_tool_server_container(
        thread_key="thread-1",
        container_name="centaur-sandbox-1",
        firewall_host="proxy-host",
        api_url="http://api:8000",
        overlay_mount=None,
        database_url=dsn,
    )

    env = {e["name"]: e for e in container["env"]}
    # DATABASE_URL is the literal proxied value, not a secretKeyRef
    assert env["DATABASE_URL"] == {"name": "DATABASE_URL", "value": dsn}
    # the signing key still comes from the secret
    assert "valueFrom" in env["SANDBOX_SIGNING_KEY"]


def test_tool_server_container_includes_pg_dsn_env(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Tool code runs in the sidecar, so pg_dsn secrets must reach it as env.

    Without this, ``secret("EXAMPLE_DSN")`` resolves to the placeholder because
    ``_resolve_secrets`` delivers ``PgDsnSecret`` via the environment, not
    ``ToolContext`` — and only the agent container had it.
    """
    monkeypatch.setenv("KUBERNETES_TOOL_SERVER_IMAGE", "centaur-api:latest")
    monkeypatch.setenv("KUBERNETES_SECRET_ENV_NAME", "centaur-infra-env")

    example_dsn = "postgresql://app_user:pw@proxy-host:5434/example"
    container = _build_tool_server_container(
        thread_key="thread-1",
        container_name="centaur-sandbox-1",
        firewall_host="proxy-host",
        api_url="http://api:8000",
        overlay_mount=None,
        database_url="postgresql://app_user:pw@proxy-host:5433/ai_v2",
        pg_dsns={"EXAMPLE_DSN": example_dsn},
    )

    env = {e["name"]: e for e in container["env"]}
    assert env["EXAMPLE_DSN"] == {"name": "EXAMPLE_DSN", "value": example_dsn}


def test_proxy_iron_env_includes_core_password_when_set(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("KUBERNETES_SECRET_ENV_NAME", "centaur-infra-env")
    core = {"password_env": _CORE_PG_PASSWORD_ENV, "password": "sekret"}

    env = _proxy_iron_env("centaur-infra-env", [], core=core)

    values = {e["name"]: e.get("value") for e in env}
    assert values[_CORE_PG_PASSWORD_ENV] == "sekret"


def test_proxy_iron_env_omits_core_password_when_none(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("KUBERNETES_SECRET_ENV_NAME", "centaur-infra-env")

    env = _proxy_iron_env("centaur-infra-env", [], core=None)

    assert _CORE_PG_PASSWORD_ENV not in [e["name"] for e in env]
